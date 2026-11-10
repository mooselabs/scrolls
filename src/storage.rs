//! Local storage and the shared SQL session foundation.
pub mod backup;
mod inspection;
pub mod session;
pub mod shared;
pub mod shared_review;
use crate::{
    model::*,
    store::{Change, ChangeSet},
};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use session::SqlSession;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub const DOLT_VERSION: &str = "2.2.0";
pub fn literal(value: &str) -> String {
    format!("CONVERT(0x{} USING utf8mb4)", hex::encode(value))
}
pub fn revision_literal(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "''"))
}
pub struct Store {
    pub root: PathBuf,
    pub db: PathBuf,
    binary: PathBuf,
    _lock: Option<File>,
    shared: Option<session::SharedSession>,
    inspection: Option<Box<std::cell::RefCell<inspection::Session>>>,
}
impl Store {
    /// Open without recovery, branch checkout, configuration writes or initialization.
    pub fn open_readonly(root: &Path, binary: &Path) -> Result<Self> {
        Self::open_readonly_wait(root, binary, None)
    }
    /// Same read-only opener with bounded lock admission for the HTTP server.
    pub fn open_readonly_wait(
        root: &Path,
        binary: &Path,
        timeout: Option<std::time::Duration>,
    ) -> Result<Self> {
        let root = fs::canonicalize(root).context("store missing; run init first")?;
        let lock = OpenOptions::new().read(true).open(root.join("lock"))?;
        if let Some(timeout) = timeout {
            let start = std::time::Instant::now();
            loop {
                match lock.try_lock_exclusive() {
                    Ok(()) => break,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        anyhow::ensure!(start.elapsed() < timeout, "read_lock_timeout");
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        } else {
            lock.lock_exclusive()?;
        }
        // Check recovery state only after the previous writer has released its lock.
        for journal in ["review-journal.json", "migration-journal.json"] {
            if root.join(journal).exists() {
                bail!("recovery_required: outstanding {journal}; run doctor before planning");
            }
        }
        let s = Self {
            db: root.join("dolt"),
            root,
            binary: fs::canonicalize(binary)?,
            _lock: Some(lock),
            shared: None,
            inspection: None,
        };
        let version = s.run(&["version"])?;
        if version.lines().next() != Some(format!("dolt version {DOLT_VERSION}").as_str()) {
            bail!("Dolt {DOLT_VERSION} required");
        }
        session::SqlSession::clean(&s)?;
        // Inspect all branch working sets; a dirty inactive branch is not a snapshot.
        if !s
            .sql("SELECT * FROM dolt_branches WHERE dirty = 1")?
            .is_empty()
        {
            bail!("dirty_state: resolve dirty branches before planning");
        }
        Ok(s)
    }
    pub fn rows_at<T: DeserializeOwned>(&self, table: &str, rev: &str) -> Result<Vec<T>> {
        self.sql(&format!(
            "SELECT payload FROM {table} AS OF {} ORDER BY id",
            revision_literal(rev)
        ))?
        .into_iter()
        .map(|v| {
            Ok(serde_json::from_str(
                v["payload"].as_str().context("missing payload")?,
            )?)
        })
        .collect()
    }
    pub fn one_at<T: DeserializeOwned>(
        &self,
        table: &str,
        id: &str,
        rev: &str,
    ) -> Result<Option<T>> {
        self.sql(&format!(
            "SELECT payload FROM {table} AS OF {} WHERE id={}",
            revision_literal(rev),
            literal(id)
        ))?
        .first()
        .map(|v| {
            Ok(serde_json::from_str(
                v["payload"].as_str().context("missing payload")?,
            )?)
        })
        .transpose()
    }
    /// Read keyed payloads in the selected view. Metadata retains its historical
    /// unordered health query; graph/import tables retain their keyed ordering.
    pub fn read_rows(&self, table: &str, rev: Option<&str>) -> Result<Vec<(String, String)>> {
        let at = rev
            .map(|r| format!(" AS OF {}", revision_literal(r)))
            .unwrap_or_default();
        let order = if table == "metadata" {
            ""
        } else {
            " ORDER BY id"
        };
        self.sql(&format!("SELECT id,payload FROM {table}{at}{order}"))?
            .into_iter()
            .map(|row| {
                Ok((
                    row["id"].as_str().context("missing row key")?.into(),
                    row["payload"].as_str().context("missing payload")?.into(),
                ))
            })
            .collect()
    }
    pub fn read_one(&self, table: &str, id: &str, rev: Option<&str>) -> Result<Option<String>> {
        let at = rev
            .map(|r| format!(" AS OF {}", revision_literal(r)))
            .unwrap_or_default();
        self.sql(&format!(
            "SELECT payload FROM {table}{at} WHERE id={}",
            literal(id)
        ))?
        .first()
        .map(|row| Ok(row["payload"].as_str().context("missing payload")?.into()))
        .transpose()
    }
    pub fn import_files(&self) -> Result<Vec<crate::source_import::File>> {
        crate::store::import_files(self, None)
    }
    pub fn shared_query_limit(&self) -> Option<usize> {
        self.shared.as_ref().map(|s| s.query_limit())
    }
    pub fn import_files_at(&self, rev: &str) -> Result<Vec<crate::source_import::File>> {
        crate::store::import_files(self, Some(rev))
    }
    pub fn graph_at(&self, rev: &str) -> Result<Graph> {
        crate::store::graph(self, Some(rev))
    }
    fn migration_journal(&self, value: &Value) -> Result<()> {
        let mut f = tempfile::NamedTempFile::new_in(&self.root)?;
        f.write_all(&serde_json::to_vec(value)?)?;
        f.as_file().sync_all()?;
        f.persist(self.root.join("migration-journal.json"))?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    fn migration_expected(&self, sql: &str) -> Result<Value> {
        let tables = [
            "records",
            "edges",
            "source_files",
            "metadata",
            "operations",
            "registry",
            "import_sources",
            "import_files",
            "import_members",
        ];
        let mut expected =
            std::collections::BTreeMap::<String, std::collections::BTreeMap<String, String>>::new();
        for table in tables {
            let mut rows = std::collections::BTreeMap::new();
            if !table.starts_with("import_") {
                for row in self.sql(&format!("SELECT id,payload FROM {table} ORDER BY id"))? {
                    let id = row["id"].as_str().context("migration row ID")?;
                    if table != "metadata" || id != "migration_owner" {
                        rows.insert(
                            id.into(),
                            row["payload"].as_str().context("migration payload")?.into(),
                        );
                    }
                }
            }
            expected.insert(table.into(), rows);
        }
        let re = regex::Regex::new(
            r"REPLACE INTO ([a-z_]+) \(id,payload\) VALUES \(CONVERT\(0x([0-9a-f]+) USING utf8mb4\),CONVERT\(0x([0-9a-f]+) USING utf8mb4\)\);",
        )?;
        for line in sql.lines().filter(|l| !l.trim().is_empty()) {
            let cap = re.captures(line).context("unexpected migration SQL")?;
            anyhow::ensure!(
                cap.get(0).unwrap().as_str() == line,
                "unexpected migration SQL suffix"
            );
            let id = String::from_utf8(hex::decode(&cap[2])?)?;
            let payload = String::from_utf8(hex::decode(&cap[3])?)?;
            expected
                .get_mut(&cap[1])
                .context("unexpected migration table")?
                .insert(id, payload);
        }
        Ok(json!(
            expected
                .into_iter()
                .map(|(table, rows)| (table, digest(serde_json::to_vec(&rows).unwrap())))
                .collect::<std::collections::BTreeMap<_, _>>()
        ))
    }
    fn verify_migration(&self, j: &Value, rev: &str) -> Result<()> {
        let tables = self.sql(&format!("SHOW TABLES AS OF {}", revision_literal(rev)))?;
        let names: std::collections::BTreeSet<_> = tables
            .iter()
            .filter_map(|row| {
                row.as_object()?
                    .values()
                    .next()?
                    .as_str()
                    .map(str::to_string)
            })
            .collect();
        let wanted: std::collections::BTreeSet<_> = [
            "records",
            "edges",
            "source_files",
            "metadata",
            "operations",
            "registry",
            "import_sources",
            "import_files",
            "import_members",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        anyhow::ensure!(
            names == wanted,
            "unexpected migration table inventory; preserve journal for inspection"
        );
        let expected = j["expected"]
            .as_object()
            .context("migration journal lacks expected state; inspect manually")?;
        for (table, hash) in expected {
            anyhow::ensure!(
                [
                    "records",
                    "edges",
                    "source_files",
                    "metadata",
                    "operations",
                    "registry",
                    "import_sources",
                    "import_files",
                    "import_members"
                ]
                .contains(&table.as_str()),
                "invalid migration table"
            );
            let mut rows = std::collections::BTreeMap::<String, String>::new();
            for row in self.sql(&format!(
                "SELECT id,payload FROM {table} AS OF {} ORDER BY id",
                revision_literal(rev)
            ))? {
                let id = row["id"].as_str().context("migration row")?;
                if table != "metadata" || id != "migration_owner" {
                    rows.insert(
                        id.into(),
                        row["payload"].as_str().context("migration payload")?.into(),
                    );
                }
            }
            anyhow::ensure!(
                hash == &json!(digest(serde_json::to_vec(&rows)?)),
                "unexpected migration content in {table}; preserve journal for inspection"
            );
        }
        anyhow::ensure!(expected.len() == 9, "invalid migration expected state");
        Ok(())
    }
    pub fn migrate_format4(&self, sql: &str, actor: &str) -> Result<()> {
        self.clean()?;
        let active = self.sql("SELECT active_branch() AS branch")?;
        anyhow::ensure!(
            active[0]["branch"] == "main",
            "migration requires clean main"
        );
        let base = self.head("main")?;
        let branch = format!("migration/{}", uuid::Uuid::new_v4().simple());
        let mut journal = json!({"format":1,"branch":branch,"base":base,"target":null,"expected":self.migration_expected(sql)?});
        self.migration_journal(&journal)?;
        self.run(&["checkout", "-b", &branch, "main"])?;
        // DDL survives SQL rollback in Dolt 2.2.0. Only this private branch sees it.
        self.sql("CREATE TABLE import_sources (id VARCHAR(128) PRIMARY KEY, payload LONGTEXT NOT NULL); CREATE TABLE import_files (id VARCHAR(128) PRIMARY KEY, payload LONGTEXT NOT NULL); CREATE TABLE import_members (id VARCHAR(128) PRIMARY KEY, payload LONGTEXT NOT NULL);")?;
        self.transaction(
            &(sql.to_owned() + &put("metadata", "migration_owner", &journal)?),
            "migrate Scrolls format 4",
            actor,
        )?;
        journal["target"] = json!(self.head(&branch)?);
        self.migration_journal(&journal)?;
        self.graph()?.schema.validate()?;
        self.verify_migration(&journal, &self.head(&branch)?)?;
        anyhow::ensure!(
            self.head("main")? == base,
            "migration main changed; inspect journal"
        );
        self.checkout("main")?;
        self.run(&["merge", "--ff-only", &branch])?;
        self.recover_migration()
    }
    pub fn recover_migration(&self) -> Result<()> {
        let path = self.root.join("migration-journal.json");
        if !path.exists() {
            return Ok(());
        }
        let j: Value = serde_json::from_slice(&fs::read(&path)?)?;
        let branch = j["branch"].as_str().context("invalid migration journal")?;
        let base = j["base"].as_str().context("invalid migration base")?;
        anyhow::ensure!(
            j["format"] == 1
                && branch.starts_with("migration/")
                && branch.len() == 42
                && branch[10..].bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid migration journal; inspect manually"
        );
        let main = self.head("main")?;
        let active = self.sql("SELECT active_branch() AS branch")?[0]["branch"]
            .as_str()
            .context("active branch")?
            .to_string();
        let branches = self.branches()?;
        if !branches.iter().any(|b| b == branch) {
            anyhow::ensure!(
                active == "main" && (main == base || j["target"] == main),
                "unexpected migration state; inspect journal"
            );
            self.clean()?;
            fs::remove_file(path)?;
            return Ok(());
        }
        let head = self.head(branch)?;
        let owned = if head == base {
            true
        } else {
            let marker: Option<Value> = self.one_at("metadata", "migration_owner", branch)?;
            marker.is_some_and(|m| m["branch"] == branch && m["base"] == base && m["format"] == 1)
                && (j["target"].is_null() || j["target"] == head)
        };
        anyhow::ensure!(
            owned && (main == base || main == head) && ["main", branch].contains(&active.as_str()),
            "unexpected migration state; preserve journal for inspection"
        );
        if head != base {
            self.verify_migration(&j, &head)?;
        }
        if active == branch {
            let dirty = self.sql("SELECT * FROM dolt_status")?;
            anyhow::ensure!(
                head == base || dirty.is_empty(),
                "unexpected dirty migrated branch; inspect journal"
            );
            for row in &dirty {
                anyhow::ensure!(
                    ["import_sources", "import_files", "import_members"]
                        .iter()
                        .any(|n| row["table_name"] == *n)
                        && row["status"] == "new table",
                    "unexpected migration working set; inspect journal"
                );
            }
            if !dirty.is_empty() {
                // Dolt reset --hard leaves untracked tables. Drop only the new tables
                // whose names/status were verified above, on our temporary branch.
                for row in &dirty {
                    let table = row["table_name"].as_str().context("migration table name")?;
                    anyhow::ensure!(
                        self.rows::<Value>(table)?.is_empty(),
                        "unexpected migration table data in {table}; preserve journal for inspection"
                    );
                    self.sql(&format!(
                        "DROP TABLE IF EXISTS {}",
                        row["table_name"].as_str().context("migration table name")?
                    ))?;
                }
                self.run(&["reset", "--hard"])?;
            }
            self.checkout("main")?;
        } else {
            self.clean()?;
        }
        // Never reset main, including after a successful fast-forward.
        self.run(&["branch", "-D", branch])?;
        fs::remove_file(path)?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }

    pub fn open(root: &Path, binary: &Path, init: bool) -> Result<Self> {
        let binary = fs::canonicalize(binary).with_context(|| {
            format!(
                "private Dolt missing at {}; run scripts/install-dolt.sh or pass --dolt",
                binary.display()
            )
        })?;
        if !root.exists() {
            if !init {
                bail!("store missing at {}; run init", root.display())
            }
            fs::create_dir_all(root)?;
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("lock"))?;
        lock.lock_exclusive()?;
        let root = fs::canonicalize(root)?;
        let config = root.join("dolt-home/.dolt/config_global.json");
        fs::create_dir_all(config.parent().unwrap())?;
        let settings = b"{\"versioncheck.disabled\":\"true\",\"metrics.disabled\":\"true\"}";
        if fs::read(&config).ok().as_deref() != Some(settings) {
            fs::write(&config, settings)?;
        }
        let version = Command::new(&binary)
            .arg("version")
            .env("DOLT_ROOT_PATH", root.join("dolt-home"))
            .env("DOLT_DISABLE_EVENT_FLUSH", "1")
            .output()?;
        if !version.status.success()
            || String::from_utf8_lossy(&version.stdout).lines().next()
                != Some(format!("dolt version {DOLT_VERSION}").as_str())
        {
            bail!("Dolt {DOLT_VERSION} required; shared tools are not used");
        }

        let s = Self {
            root: root.clone(),
            db: root.join("dolt"),
            binary,
            _lock: Some(lock),
            shared: None,
            inspection: None,
        };
        if !s.db.join(".dolt").exists() {
            if !init {
                bail!("store has no database; run init");
            }
            fs::create_dir_all(&s.db)?;
            s.run(&["init", "--name", "Scrolls", "--email", "scrolls@localhost"])?;
            s.sql("CREATE TABLE import_sources (id VARCHAR(128) PRIMARY KEY, payload LONGTEXT NOT NULL); CREATE TABLE import_files (id VARCHAR(128) PRIMARY KEY, payload LONGTEXT NOT NULL); CREATE TABLE import_members (id VARCHAR(128) PRIMARY KEY, payload LONGTEXT NOT NULL); CREATE TABLE records (id VARCHAR(128) PRIMARY KEY, payload LONGTEXT NOT NULL); CREATE TABLE edges (id VARCHAR(128) PRIMARY KEY, payload LONGTEXT NOT NULL); CREATE TABLE source_files (id VARCHAR(2048) PRIMARY KEY, payload LONGTEXT NOT NULL); CREATE TABLE metadata (id VARCHAR(128) PRIMARY KEY, payload LONGTEXT NOT NULL); CREATE TABLE operations (id VARCHAR(256) PRIMARY KEY, payload LONGTEXT NOT NULL); CREATE TABLE registry (id VARCHAR(128) PRIMARY KEY, payload LONGTEXT NOT NULL);")?;
            let mut sql = String::new();
            sql.push_str(&put(
                "metadata",
                "ontology",
                &crate::schema::Schema::default(),
            )?);
            sql.push_str(&put("metadata", "schema", &4)?);
            sql.push_str(&put(
                "metadata",
                "store_id",
                &uuid::Uuid::new_v4().to_string(),
            )?);
            s.transaction(&sql, "initialize Scrolls", "system")?;
        }
        // A killed operation may leave a clean proposal or review branch checked out.
        // Never reset an unexplained working set, even on a temporary branch.
        s.recover_migration()?;
        s.recover_review()?;
        s.clean()?;
        let schema: Option<u32> = s.one("metadata", "schema")?;
        if !matches!(schema, Some(1..=4)) {
            bail!("unsupported store schema; doctor before upgrading");
        }
        Ok(s)
    }
    pub fn recover_review(&self) -> Result<()> {
        let path = self.root.join("review-journal.json");
        if !path.exists() {
            return Ok(());
        }
        let journal: Value = serde_json::from_slice(&fs::read(&path)?)?;
        let branch = journal["branch"]
            .as_str()
            .context("invalid review recovery journal")?;
        if !branch.starts_with("review/") || branch.len() != 39 {
            bail!("invalid review recovery branch; inspect journal")
        }
        let active = self.sql("SELECT active_branch() AS branch")?;
        if active[0]["branch"] == branch {
            let _ = self.run(&["merge", "--abort"]);
            self.run(&["reset", "--hard"])?;
            self.run(&["checkout", "main"])?;
        } else {
            self.clean()?;
        }
        fs::remove_file(path)?;
        Ok(())
    }
    pub fn journal_review(&self, branch: &str, base: &str) -> Result<()> {
        let mut f = tempfile::NamedTempFile::new_in(&self.root)?;
        f.write_all(&serde_json::to_vec(&json!({"branch":branch,"base":base}))?)?;
        f.as_file().sync_all()?;
        f.persist(self.root.join("review-journal.json"))?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    pub fn run(&self, args: &[&str]) -> Result<String> {
        anyhow::ensure!(
            !self.is_shared(),
            "shared_unsupported: local Dolt subprocess"
        );
        if self.db.join(".dolt/sql-server.info").exists() {
            bail!("unexpected Dolt server; stop it before opening the local store");
        }
        anyhow::ensure!(
            self.inspection.is_none(),
            "backup_inspection_readonly: separate subprocess refused"
        );
        let mut command = Command::new(&self.binary);
        let out = command
            .args(args)
            .current_dir(&self.db)
            .env("DOLT_ROOT_PATH", self.root.join("dolt-home"))
            .env("DOLT_DISABLE_EVENT_FLUSH", "1")
            .stdin(Stdio::null())
            .output()?;
        if !out.status.success() {
            bail!(
                "Dolt {} failed: {} {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr),
                String::from_utf8_lossy(&out.stdout)
            );
        }
        Ok(String::from_utf8(out.stdout)?)
    }
    pub fn sql(&self, sql: &str) -> Result<Vec<Value>> {
        if let Some(session) = &self.inspection {
            return session.borrow_mut().query(sql);
        }
        if let Some(session) = &self.shared {
            return session.sql(sql);
        }
        let mut f = tempfile::NamedTempFile::new()?;
        f.write_all(sql.as_bytes())?;
        let raw = self.run(&[
            "sql",
            "-r",
            "json",
            "--file",
            f.path().to_str().context("non-UTF8 store path")?,
        ])?;
        let mut rows = vec![];
        for value in serde_json::Deserializer::from_str(&raw).into_iter::<Value>() {
            let value = value.with_context(|| {
                format!(
                    "invalid Dolt response: {}",
                    raw.chars().take(250).collect::<String>()
                )
            })?;
            if let Some(v) = value["rows"].as_array() {
                rows.extend(v.clone());
            }
        }
        Ok(rows)
    }
    pub fn clean(&self) -> Result<()> {
        if !self.sql("SELECT * FROM dolt_status")?.is_empty() {
            bail!(
                "unexplained dirty Dolt state; inspect with the pinned dolt status/diff and recover explicitly; Scrolls will not discard it"
            )
        }
        Ok(())
    }
    pub fn checkout(&self, branch: &str) -> Result<()> {
        if let Some(session) = &self.shared {
            anyhow::ensure!(
                branch == "main" || branch.starts_with("proposal/"),
                "shared_unsupported: select public proposal branches only"
            );
            session.select_branch(branch)?;
            session.clean()?;
            self.check_versioned_inventory("HEAD")?;
            anyhow::ensure!(
                self.one::<u32>("metadata", "schema")? == Some(5)
                    && self.one::<u32>("metadata", "shared_writer")? == Some(4),
                "unsupported shared branch format"
            );
            return Ok(());
        }
        self.clean()?;
        self.run(&["checkout", branch])?;
        let format = self
            .one::<u32>("metadata", "schema")?
            .context("missing store format")?;
        anyhow::ensure!(
            format <= 4,
            "unsupported store format {format}; use a newer Scrolls writer"
        );
        Ok(())
    }
    pub fn revision(&self) -> Result<String> {
        Ok(
            self.sql("SELECT DOLT_HASHOF('HEAD') AS revision")?[0]["revision"]
                .as_str()
                .context("missing Dolt revision")?
                .into(),
        )
    }
    pub fn merge_base(&self, from: &str, to: &str) -> Result<String> {
        if !self.is_shared() {
            return Ok(self.run(&["merge-base", from, to])?.trim().into());
        }
        self.sql(&format!(
            "SELECT DOLT_MERGE_BASE({}, {}) AS revision",
            literal(from),
            literal(to)
        ))?
        .first()
        .and_then(|r| r["revision"].as_str())
        .map(str::to_owned)
        .context("missing merge base")
    }
    pub fn diff_text(&self, from: &str, to: &str, table: &str) -> Result<String> {
        if !self.is_shared() {
            return self.run(&["diff", from, to, table]);
        }
        Ok(serde_json::to_string_pretty(&self.sql(&format!(
            "SELECT * FROM DOLT_DIFF({}, {}, {})",
            literal(from),
            literal(to),
            literal(table)
        ))?)?)
    }
    pub(crate) fn select_shared(&self, branch: &str) -> Result<()> {
        self.shared
            .as_ref()
            .context("shared operation required")?
            .select_branch(branch)
    }
    pub(crate) fn commit_shared_checked(
        &self,
        changes: &str,
        message: &str,
        actor: &str,
        owned: bool,
        validate: impl FnOnce() -> Result<()>,
    ) -> Result<String> {
        self.shared
            .as_ref()
            .context("shared operation required")?
            .commit_checked(changes, message, actor, owned, validate)
    }
    pub(crate) fn commit_shared_markdown_checked(
        &self,
        changes: &str,
        message: &str,
        actor: &str,
        validate: impl FnOnce() -> Result<()>,
    ) -> Result<String> {
        self.shared
            .as_ref()
            .context("shared operation required")?
            .commit_markdown_checked(changes, message, actor, validate)
    }
    pub fn head(&self, branch: &str) -> Result<String> {
        Ok(self.sql(&format!(
            "SELECT DOLT_HASHOF({}) AS revision",
            literal(branch)
        ))?[0]["revision"]
            .as_str()
            .context("missing revision")?
            .into())
    }
    pub fn rows<T: DeserializeOwned>(&self, table: &str) -> Result<Vec<T>> {
        self.sql(&format!("SELECT payload FROM {table} ORDER BY id"))?
            .into_iter()
            .map(|v| {
                Ok(serde_json::from_str(
                    v["payload"].as_str().context("missing payload")?,
                )?)
            })
            .collect()
    }
    pub fn one<T: DeserializeOwned>(&self, table: &str, id: &str) -> Result<Option<T>> {
        self.sql(&format!(
            "SELECT payload FROM {table} WHERE id={}",
            literal(id)
        ))?
        .first()
        .map(|v| {
            Ok(serde_json::from_str(
                v["payload"].as_str().context("missing payload")?,
            )?)
        })
        .transpose()
    }
    pub fn graph(&self) -> Result<Graph> {
        crate::store::graph(self, None)
    }
    pub fn transaction(&self, changes: &str, message: &str, actor: &str) -> Result<()> {
        if let Some(session) = &self.shared {
            session.commit(changes, message, actor)?;
            return Ok(());
        }
        let sql = format!(
            "START TRANSACTION;\n{changes}\nCALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m',{},'--author',{}); COMMIT;",
            literal(message),
            literal(&format!("{actor} <scrolls@localhost>"))
        );
        self.sql(&sql)?;
        self.clean()
    }
    pub fn branches(&self) -> Result<Vec<String>> {
        Ok(self
            .sql("SELECT name FROM dolt_branches ORDER BY name")?
            .into_iter()
            .filter_map(|v| v["name"].as_str().map(str::to_owned))
            .collect())
    }
    pub fn proposals(&self) -> Result<Vec<Proposal>> {
        let mut out = vec![];
        for branch in self
            .branches()?
            .iter()
            .filter(|b| b.starts_with("proposal/"))
        {
            let rows = self.sql(&format!(
                "SELECT payload FROM metadata AS OF {} WHERE id={}",
                revision_literal(branch),
                literal(branch)
            ))?;
            if let Some(row) = rows.first() {
                let p: Proposal =
                    serde_json::from_str(row["payload"].as_str().context("proposal payload")?)?;
                let merged = self.sql(&format!(
                    "SELECT payload FROM metadata AS OF 'main' WHERE id={}",
                    literal(branch)
                ))?;
                out.push(if let Some(row) = merged.first() {
                    serde_json::from_str(row["payload"].as_str().context("proposal payload")?)?
                } else {
                    p
                });
            }
        }
        Ok(out)
    }
    pub fn history(&self, revision: &str, id: &str) -> Result<Vec<Value>> {
        self.sql(&format!("SELECT commit_hash,committer,commit_date,payload FROM dolt_history_records AS OF {} WHERE id={} ORDER BY commit_date,commit_hash,payload",revision_literal(revision),literal(id)))
    }
    pub fn log(&self, revision: &str) -> Result<Vec<Value>> {
        self.sql(&format!(
            "SELECT * FROM DOLT_LOG({}) ORDER BY date,commit_hash",
            literal(revision)
        ))
    }
    /// Dolt-only review guard spanning the current working set and main.
    pub(crate) fn merged_review_guards(&self) -> Result<Vec<Value>> {
        self.sql("SELECT payload FROM metadata WHERE id LIKE 'guard:%' AND id NOT IN (SELECT id FROM metadata AS OF 'main' WHERE id LIKE 'guard:%')")
    }
    pub fn receipt(&self, op: &str, hash: &str) -> Result<Option<Value>> {
        if self.is_shared() {
            shared_review::check_identity(self, op, hash)?;
        }
        if let Some(r) = (if self.is_shared() {
            crate::beads::Local::default()
        } else {
            crate::beads::Local::load(&self.root)?
        })
        .operations
        .get(op)
        {
            if r["hash"] != hash {
                bail!("--op-id {op} was already used with different arguments or attribution");
            }
            return Ok(r.get("result").cloned());
        }
        // Include rejected branches: an operation identity is store-wide forever.
        let queries: Vec<_> = self
            .branches()?
            .iter()
            .filter(|b| {
                b.as_str() == "main"
                    || b.starts_with("proposal/")
                    || (self.is_shared() && b.as_str() == shared_review::JOURNAL)
            })
            .map(|b| {
                format!(
                    "SELECT payload FROM operations AS OF {} WHERE id={}",
                    revision_literal(b),
                    literal(op)
                )
            })
            .collect();
        let mut result = None;
        for row in self.sql(&queries.join(" UNION ALL "))? {
            let r: Value =
                serde_json::from_str(row["payload"].as_str().context("operation payload")?)?;
            if r["args_hash"] != hash {
                bail!("--op-id {op} was already used with different arguments or attribution");
            }
            result = Some(r["result"].clone());
        }
        if self.is_shared() && result.is_some() {
            shared_review::verify_receipt(self, op, hash)?;
        }
        Ok(result)
    }
    pub fn audit(
        &self,
        session: &str,
        actor: &str,
        revision: &str,
        via: &str,
        ids: &[String],
    ) -> Result<()> {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("consumed.jsonl"))?;
        writeln!(
            f,
            "{}",
            json!({"session":session,"actor":actor,"revision":revision,"via":via,"ids":ids,"record_revisions": self.graph()?.records.values().filter(|r|ids.contains(&r.id)).map(|r|(&r.id,r.revision)).collect::<std::collections::BTreeMap<_,_>>(),"read_at":now()})
        )?;
        f.sync_data()?;
        Ok(())
    }
}
impl ChangeSet {
    /// Dolt encoding, kept byte-identical for the shared proposal parser and
    /// Markdown bulk transport. Engine code builds changes without SQL text.
    pub fn to_sql(&self) -> String {
        let mut sql = String::new();
        for change in &self.changes {
            match change {
                Change::Put { table, id, payload } | Change::Insert { table, id, payload } => {
                    let verb = if matches!(change, Change::Put { .. }) {
                        "REPLACE"
                    } else {
                        "INSERT"
                    };
                    sql.push_str(&format!(
                        "{verb} INTO {table} (id,payload) VALUES ({},{});\n",
                        literal(id),
                        literal(payload),
                    ));
                }
                Change::DeleteEdge { id } => {
                    sql.push_str(&format!("DELETE FROM edges WHERE id={};", literal(id)));
                }
            }
        }
        sql
    }
}

/// Append immutable rows. Duplicate keys must fail rather than replace a tuple.
/// BKLG-20261108-enit: retain the String API used by existing callers.
pub fn insert<T: Serialize>(table: &str, id: &str, value: &T) -> Result<String> {
    Ok(ChangeSet::from(Change::insert(table, id, value)?).to_sql())
}
/// BKLG-20261108-enit: retain the String API used by existing callers.
pub fn put<T: Serialize>(table: &str, id: &str, value: &T) -> Result<String> {
    Ok(ChangeSet::from(Change::put(table, id, value)?).to_sql())
}
/// BKLG-20261108-enit: retain the String API used by existing callers.
pub fn graph_changes(before: &Graph, after: &Graph) -> Result<String> {
    Ok(ChangeSet::graph(before, after)?.to_sql())
}

#[cfg(test)]
mod sql_text_tests {
    use super::*;

    #[test]
    fn put_and_insert_sql_bytes() -> Result<()> {
        assert_eq!(
            put("metadata", "k'", &"é\n")?,
            "REPLACE INTO metadata (id,payload) VALUES (CONVERT(0x6b27 USING utf8mb4),CONVERT(0x22c3a95c6e22 USING utf8mb4));\n"
        );
        assert_eq!(
            insert("import_files", "k'", &"é\n")?,
            "INSERT INTO import_files (id,payload) VALUES (CONVERT(0x6b27 USING utf8mb4),CONVERT(0x22c3a95c6e22 USING utf8mb4));\n"
        );
        assert_eq!(
            put(
                "operations",
                "op",
                &json!({"op_id":"op","args_hash":"h","actor":"a","command":"new","result":{"count":0},"committed_at":"2026-11-08"})
            )?,
            "REPLACE INTO operations (id,payload) VALUES (CONVERT(0x6f70 USING utf8mb4),CONVERT(0x7b226163746f72223a2261222c22617267735f68617368223a2268222c22636f6d6d616e64223a226e6577222c22636f6d6d69747465645f6174223a22323032362d31312d3038222c226f705f6964223a226f70222c22726573756c74223a7b22636f756e74223a307d7d USING utf8mb4));\n"
        );
        Ok(())
    }

    #[test]
    fn graph_sql_bytes() -> Result<()> {
        let record = Record {
            id: "r".into(),
            kind: "note".into(),
            title: "New".into(),
            status: "".into(),
            body: "quote ' and é\n".into(),
            owner: "".into(),
            confidence: "medium".into(),
            created: "2026-11-08".into(),
            last_reviewed: "2026-11-08".into(),
            review_every: 0,
            actor: "a".into(),
            visibility: "internal".into(),
            sync: "always".into(),
            fields: json!({}),
            revision: 2,
            path: None,
            frozen: false,
            original: None,
            aliases: vec![],
            metadata: None,
        };
        let edge = Edge {
            from: "r".into(),
            rel: "related".into(),
            to: "t".into(),
            why: "new".into(),
            actor: "a".into(),
            created: "2026-11-08".into(),
        };
        let mut before = Graph::default();
        let mut old = record.clone();
        old.title = "Old".into();
        before.records.insert("r".into(), old);
        let mut old = edge.clone();
        old.why = "old".into();
        before.edges.insert("changed".into(), old);
        before.edges.insert("deleted".into(), edge.clone());
        let mut after = Graph::default();
        after.records.insert("r".into(), record);
        after.edges.insert("changed".into(), edge);
        assert_eq!(
            graph_changes(&before, &after)?,
            "REPLACE INTO records (id,payload) VALUES (CONVERT(0x72 USING utf8mb4),CONVERT(0x7b226964223a2272222c226b696e64223a226e6f7465222c227469746c65223a224e6577222c22737461747573223a22222c22626f6479223a2271756f7465202720616e6420c3a95c6e222c226f776e6572223a22222c22636f6e666964656e6365223a226d656469756d222c2263726561746564223a22323032362d31312d3038222c226c6173745f7265766965776564223a22323032362d31312d3038222c227265766965775f6576657279223a302c226163746f72223a2261222c227669736962696c697479223a22696e7465726e616c222c2273796e63223a22616c77617973222c226669656c6473223a7b7d2c227265766973696f6e223a322c2270617468223a6e756c6c2c2266726f7a656e223a66616c73652c226f726967696e616c223a6e756c6c2c22616c6961736573223a5b5d7d USING utf8mb4));\nREPLACE INTO edges (id,payload) VALUES (CONVERT(0x6368616e676564 USING utf8mb4),CONVERT(0x7b2266726f6d223a2272222c2272656c223a2272656c61746564222c22746f223a2274222c22776879223a226e6577222c226163746f72223a2261222c2263726561746564223a22323032362d31312d3038227d USING utf8mb4));\nDELETE FROM edges WHERE id=CONVERT(0x64656c65746564 USING utf8mb4);"
        );
        assert_eq!(graph_changes(&after, &after)?, "");
        Ok(())
    }
}
