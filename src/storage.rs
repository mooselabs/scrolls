//! All Dolt access is serialized, offline and through one pinned private CLI.
use crate::model::*;
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
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
    _lock: File,
}
impl Store {
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
            _lock: lock,
        };
        if !s.db.join(".dolt").exists() {
            if !init {
                bail!("store has no database; run init");
            }
            fs::create_dir_all(&s.db)?;
            s.run(&["init", "--name", "Scrolls", "--email", "scrolls@localhost"])?;
            s.sql("CREATE TABLE records (id VARCHAR(128) PRIMARY KEY, payload LONGTEXT NOT NULL); CREATE TABLE edges (id VARCHAR(128) PRIMARY KEY, payload LONGTEXT NOT NULL); CREATE TABLE source_files (id VARCHAR(2048) PRIMARY KEY, payload LONGTEXT NOT NULL); CREATE TABLE metadata (id VARCHAR(128) PRIMARY KEY, payload LONGTEXT NOT NULL); CREATE TABLE operations (id VARCHAR(256) PRIMARY KEY, payload LONGTEXT NOT NULL); CREATE TABLE registry (id VARCHAR(128) PRIMARY KEY, payload LONGTEXT NOT NULL);")?;
            let mut sql = String::new();
            for kind in KINDS {
                sql.push_str(&put("registry",kind,&json!({"kind":kind,"lifecycle":lifecycle(kind)?,"write_policy":if matches!(*kind,"evidence"|"question"|"collection"|"note"){"direct"}else if *kind=="answer"{"human-direct"}else{"proposal"}}))?);
            }
            for rel in RELATIONS {
                sql.push_str(&put("registry",&format!("rel:{rel}"),&json!({"relation":rel,"requires_why":matches!(*rel,"supersedes"|"contradicts"|"depends_on")}))?);
            }
            for (field, kind, values) in [
                ("owner", "string", json!(null)),
                ("title", "string", json!(null)),
                ("confidence", "enum", json!(["high", "medium", "low"])),
                ("review_every", "positive-days", json!(null)),
                ("sync", "enum", json!(["always", "on-demand", "never"])),
                ("visibility", "label", json!(null)),
            ] {
                sql.push_str(&put(
                    "registry",
                    &format!("field:{field}"),
                    &json!({"name":field,"type":kind,"required":true,"values":values}),
                )?);
            }
            sql.push_str(&put("metadata", "schema", &2)?);
            sql.push_str(&put(
                "metadata",
                "store_id",
                &uuid::Uuid::new_v4().to_string(),
            )?);
            s.transaction(&sql, "initialize Scrolls", "system")?;
        }
        // A killed operation may leave a clean proposal or review branch checked out.
        // Never reset an unexplained working set, even on a temporary branch.
        s.recover_review()?;
        s.clean()?;
        let schema: Option<u32> = s.one("metadata", "schema")?;
        if !matches!(schema, Some(1 | 2)) {
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
        let mut f = tempfile::NamedTempFile::new_in(&self.root)?;
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
        self.clean()?;
        self.run(&["checkout", branch])?;
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
        Ok(Graph {
            records: self
                .rows::<Record>("records")?
                .into_iter()
                .map(|r| (r.id.clone(), r))
                .collect(),
            edges: self
                .rows::<Edge>("edges")?
                .into_iter()
                .map(|e| (e.key(), e))
                .collect(),
        })
    }
    pub fn transaction(&self, changes: &str, message: &str, actor: &str) -> Result<()> {
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
        if let Some(r) = crate::beads::Local::load(&self.root)?.operations.get(op) {
            if r["hash"] != hash {
                bail!("--op-id {op} was already used with different arguments or attribution");
            }
            return Ok(r.get("result").cloned());
        }
        // Include rejected branches: an operation identity is store-wide forever.
        let queries: Vec<_> = self
            .branches()?
            .iter()
            .filter(|b| b.as_str() == "main" || b.starts_with("proposal/"))
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
            json!({"session":session,"actor":actor,"revision":revision,"via":via,"ids":ids,"read_at":now()})
        )?;
        f.sync_data()?;
        Ok(())
    }
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
