//! Local storage and the shared SQL session foundation.
pub mod session;
pub mod shared;
pub mod shared_review;
use crate::model::*;
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
}
impl Store {
    /// Open without recovery, branch checkout, configuration writes or initialization.
    pub fn open_readonly(root: &Path, binary: &Path) -> Result<Self> {
        let root = fs::canonicalize(root).context("store missing; run init first")?;
        let lock = OpenOptions::new().read(true).open(root.join("lock"))?;
        lock.lock_exclusive()?;
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
        };
        let version = s.run(&["version"])?;
        if version.lines().next() != Some(format!("dolt version {DOLT_VERSION}").as_str()) {
            bail!("Dolt {DOLT_VERSION} required");
        }
        s.clean()?;
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
    fn import_rows<T: DeserializeOwned>(
        &self,
        table: &str,
        rev: Option<&str>,
        key: fn(&T) -> String,
    ) -> Result<Vec<T>> {
        let at = rev
            .map(|r| format!(" AS OF {}", revision_literal(r)))
            .unwrap_or_default();
        self.sql(&format!("SELECT id,payload FROM {table}{at} ORDER BY id"))?
            .into_iter()
            .map(|row| {
                let value: T =
                    serde_json::from_str(row["payload"].as_str().context("import payload")?)?;
                anyhow::ensure!(
                    row["id"] == key(&value),
                    "{}: invalid keyed row in {table}",
                    if table.starts_with("import_") {
                        "source_ownership"
                    } else {
                        "stored_row_identity"
                    }
                );
                Ok(value)
            })
            .collect()
    }
    pub fn import_files(&self) -> Result<Vec<crate::source_import::File>> {
        if self.one::<u32>("metadata", "schema")?.unwrap_or(1) < 4 {
            return Ok(vec![]);
        }
        self.import_rows("import_files", None, crate::source_import::File::key)
    }
    pub fn graph_at(&self, rev: &str) -> Result<Graph> {
        let format = self.one_at::<u32>("metadata", "schema", rev)?.unwrap_or(1);
        let mut g = Graph {
            schema: self
                .one_at("metadata", "ontology", rev)?
                .unwrap_or_default(),
            records: self
                .import_rows::<Record>("records", Some(rev), |r| r.id.clone())?
                .into_iter()
                .map(|mut r| {
                    r.normalize_metadata();
                    (r.id.clone(), r)
                })
                .collect(),
            edges: self
                .import_rows::<Edge>("edges", Some(rev), Edge::key)?
                .into_iter()
                .map(|e| (e.key(), e))
                .collect(),
            ..Graph::default()
        };
        if format >= 4 {
            g.import_sources = self
                .import_rows::<crate::source_import::Source>("import_sources", Some(rev), |s| {
                    s.source_id.clone()
                })?
                .into_iter()
                .map(|s| (s.source_id.clone(), s))
                .collect();
            g.import_members = self
                .import_rows::<crate::source_import::Member>(
                    "import_members",
                    Some(rev),
                    crate::source_import::Member::row_key,
                )?
                .into_iter()
                .map(|m| (m.record_id.clone(), m))
                .collect();
        }
        crate::source_import::validate(&g)?;
        Ok(g)
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
        let out = Command::new(&self.binary)
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
                    && self.one::<u32>("metadata", "shared_writer")? == Some(2),
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
        let format = self.one::<u32>("metadata", "schema")?.unwrap_or(1);
        let mut g = Graph {
            schema: self.one("metadata", "ontology")?.unwrap_or_default(),
            records: self
                .import_rows::<Record>("records", None, |r| r.id.clone())?
                .into_iter()
                .map(|mut r| {
                    r.normalize_metadata();
                    (r.id.clone(), r)
                })
                .collect(),
            edges: self
                .import_rows::<Edge>("edges", None, Edge::key)?
                .into_iter()
                .map(|e| (e.key(), e))
                .collect(),
            ..Graph::default()
        };
        if format >= 4 {
            g.import_sources = self
                .import_rows::<crate::source_import::Source>("import_sources", None, |s| {
                    s.source_id.clone()
                })?
                .into_iter()
                .map(|s| (s.source_id.clone(), s))
                .collect();
            g.import_members = self
                .import_rows::<crate::source_import::Member>(
                    "import_members",
                    None,
                    crate::source_import::Member::row_key,
                )?
                .into_iter()
                .map(|m| (m.record_id.clone(), m))
                .collect();
        }
        crate::source_import::validate(&g)?;
        Ok(g)
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
/// Append immutable rows. Duplicate keys must fail rather than replace a tuple.
pub fn insert<T: Serialize>(table: &str, id: &str, value: &T) -> Result<String> {
    Ok(format!(
        "INSERT INTO {table} (id,payload) VALUES ({},{});\n",
        literal(id),
        literal(&serde_json::to_string(value)?)
    ))
}
pub fn put<T: Serialize>(table: &str, id: &str, value: &T) -> Result<String> {
    Ok(format!(
        "REPLACE INTO {table} (id,payload) VALUES ({},{});\n",
        literal(id),
        literal(&serde_json::to_string(value)?)
    ))
}
pub fn graph_changes(before: &Graph, after: &Graph) -> Result<String> {
    let mut sql = String::new();
    for (id, r) in &after.records {
        if before.records.get(id) != Some(r) {
            sql.push_str(&put("records", id, r)?);
        }
    }
    for (id, e) in &after.edges {
        if before.edges.get(id) != Some(e) {
            sql.push_str(&put("edges", id, e)?);
        }
    }
    for id in before.edges.keys() {
        if !after.edges.contains_key(id) {
            sql.push_str(&format!("DELETE FROM edges WHERE id={};", literal(id)));
        }
    }
    Ok(sql)
}
