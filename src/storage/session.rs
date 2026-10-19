//! Transport boundary for a single serialized operation. Shared sessions are
//! used by local and direct shared authoring; shared review remains unavailable.
use super::{DOLT_VERSION, Store, literal};
use anyhow::{Context, Result, bail, ensure};
use mysql::{Conn, OptsBuilder, consts::ColumnType, prelude::Queryable};
use serde_json::{Map, Value, json};
use std::{cell::RefCell, time::Duration};

/// The graph engine consumes JSON rows with identical types from either backend.
/// A caller keeps this object alive for the entire operation, including reads,
/// candidate validation, commit, and receipt reconciliation.
pub trait SqlSession {
    fn sql(&self, sql: &str) -> Result<Vec<Value>>;

    fn revision(&self) -> Result<String> {
        self.sql("SELECT DOLT_HASHOF('HEAD') AS revision")?
            .first()
            .and_then(|row| row["revision"].as_str())
            .map(str::to_owned)
            .context("missing Dolt revision")
    }

    fn clean(&self) -> Result<()> {
        ensure!(
            self.sql("SELECT * FROM dolt_status")?.is_empty(),
            "dirty_state: unexplained Dolt working set; inspect without discarding it"
        );
        Ok(())
    }
}

impl SqlSession for Store {
    fn sql(&self, sql: &str) -> Result<Vec<Value>> {
        Store::sql(self, sql)
    }
}

/// Machine-local connection settings. Never serialize or debug-print credentials.
/// This initial transport requires a trusted encrypted network (e.g. loopback
/// for tests, or a private tunnel); it does not silently claim TLS protection.
pub struct Connection {
    /// Numeric address only: synchronous DNS is not bounded by the TCP timeout.
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub password: String,
    pub connect_timeout: Duration,
    pub io_timeout: Duration,
    pub lock_timeout: Duration,
}

/// One owned TCP connection, never a pool or an automatically reconnecting client.
/// Dropping it releases the database-wide session lock, including on errors.
pub struct SharedSession {
    connection: RefCell<Conn>,
    database: String,
    lock_name: String,
}

fn identifier(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "invalid database identifier: use 1–64 ASCII letters, digits or underscores"
    );
    Ok(())
}

fn branch_name(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 255
            && !value.starts_with(['/', '.'])
            && !value.ends_with(['/', '.'])
            && !value.contains("..")
            && !value.contains("//")
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b)),
        "invalid branch name"
    );
    Ok(())
}

impl SharedSession {
    /// Locks before selecting/reading the candidate base. Even read-only callers
    /// of this low-level foundation serialize; snapshot caches come later.
    pub fn connect(config: &Connection, branch: &str) -> Result<Self> {
        identifier(&config.database)?;
        branch_name(branch)?;
        config.host.parse::<std::net::IpAddr>()
            .context("shared endpoint must be a numeric IP address; resolve connection aliases before opening a session")?;
        ensure!(!config.user.is_empty(), "a named SQL account is required");
        ensure!(
            !config.connect_timeout.is_zero() && !config.io_timeout.is_zero(),
            "connection and IO timeouts must be bounded and nonzero"
        );
        ensure!(
            config.lock_timeout <= Duration::from_secs(60)
                && config.io_timeout > config.lock_timeout,
            "lock timeout must be at most 60 seconds and below the IO timeout"
        );
        // No database is selected until the publication lock is held. The lock
        // key deliberately excludes branches and account names.
        let opts = OptsBuilder::new()
            .ip_or_hostname(Some(&config.host))
            .tcp_port(config.port)
            .prefer_socket(false)
            .user(Some(&config.user))
            .pass(Some(&config.password))
            .tcp_connect_timeout(Some(config.connect_timeout))
            .read_timeout(Some(config.io_timeout))
            .write_timeout(Some(config.io_timeout));
        let s = Self {
            connection: RefCell::new(Conn::new(opts).context("shared Dolt connection failed")?),
            database: config.database.clone(),
            // Database lookup may fold case; aliases must not get distinct locks.
            // A digest also keeps the MySQL lock identifier within 64 bytes.
            lock_name: crate::model::digest(format!(
                "scrolls:{}",
                config.database.to_ascii_lowercase()
            )),
        };
        let version = s.sql("SELECT DOLT_VERSION() AS version")?;
        ensure!(
            version.first().map(|v| &v["version"]) == Some(&json!(DOLT_VERSION)),
            "unsupported Dolt version; {DOLT_VERSION} required"
        );
        let lock = s.sql(&format!(
            "SELECT GET_LOCK({}, {}) AS acquired",
            literal(&s.lock_name),
            config.lock_timeout.as_secs_f64()
        ))?;
        ensure!(
            lock.first().map(|v| &v["acquired"]) == Some(&json!(1)),
            "publication_lock_timeout: another client holds the database lock"
        );
        // GET_LOCK may itself start a transaction under some server settings.
        // End that snapshot before consulting branch heads under the lock.
        s.sql("ROLLBACK")?;
        s.sql("SET autocommit = 1")?;
        s.select_branch(branch)?;
        s.clean()?;
        ensure!(
            s.sql("SELECT name FROM dolt_branches WHERE dirty = 1")?
                .is_empty(),
            "dirty_state: an unexplained branch working set requires maintenance"
        );
        Ok(s)
    }

    pub fn select_branch(&self, branch: &str) -> Result<()> {
        branch_name(branch)?;
        self.sql(&format!("USE `{}/{branch}`", self.database))?;
        let active = self.sql("SELECT active_branch() AS branch")?;
        ensure!(
            active.first().map(|v| &v["branch"]) == Some(&json!(branch)),
            "branch selection did not select the requested branch"
        );
        Ok(())
    }

    /// Commit already-validated changes AND their operation receipt together.
    /// SQL visibility alone is insufficient. The returned revision is verified
    /// against HEAD. On error, the caller must reconcile the same operation ID
    /// through a new session; this method never retries an uncertain commit.
    pub fn commit(&self, changes: &str, message: &str, actor: &str) -> Result<String> {
        self.clean()?;
        self.sql("START TRANSACTION")?;
        let result = (|| {
            self.sql(changes)?;
            self.sql("CALL DOLT_ADD('.')")?;
            let committed = self.sql(&format!(
                "CALL DOLT_COMMIT('-m', {}, '--author', {})",
                literal(message),
                literal(&format!("{actor} <scrolls@localhost>"))
            ))?;
            let revision = committed
                .first()
                .and_then(|row| row["hash"].as_str())
                .context("Dolt commit did not return a durable revision")?
                .to_owned();
            self.sql("COMMIT")?;
            ensure!(
                self.revision()? == revision,
                "commit revision differs from HEAD"
            );
            self.clean()?;
            Ok(revision)
        })();
        match result {
            Ok(revision) => Ok(revision),
            Err(error) => {
                let rollback = self.sql("ROLLBACK");
                if let Err(rollback) = rollback {
                    bail!(
                        "indeterminate_commit: reconcile operation receipt; {error:#}; rollback failed: {rollback:#}"
                    );
                }
                // DOLT_COMMIT may already have made a versioned commit. Rolling
                // back SQL is never proof that the operation did not publish.
                bail!("commit_failed: reconcile operation receipt before retry; {error:#}")
            }
        }
    }
}

impl SqlSession for SharedSession {
    fn sql(&self, sql: &str) -> Result<Vec<Value>> {
        let mut connection = self.connection.borrow_mut();
        let mut result = connection
            .query_iter(sql)
            .context("shared SQL operation failed")?;
        let mut rows = vec![];
        while let Some(set) = result.iter() {
            for row in set {
                let row = row.context("shared SQL result failed")?;
                let mut mapped = Map::new();
                for (index, column) in row.columns_ref().iter().enumerate() {
                    let value = match row.as_ref(index).context("missing SQL column")? {
                        // The pinned CLI's JSON format omits NULL columns.
                        // Indexing an absent key still yields Value::Null.
                        mysql::Value::NULL => continue,
                        mysql::Value::Bytes(v) => {
                            let text = std::str::from_utf8(v).context("non-UTF8 SQL value")?;
                            // Text protocol returns Bytes even for numeric columns.
                            // Preserve numbers without coercing textual payloads.
                            match column.column_type() {
                                ColumnType::MYSQL_TYPE_TINY
                                | ColumnType::MYSQL_TYPE_SHORT
                                | ColumnType::MYSQL_TYPE_LONG
                                | ColumnType::MYSQL_TYPE_LONGLONG
                                | ColumnType::MYSQL_TYPE_INT24
                                | ColumnType::MYSQL_TYPE_YEAR => {
                                    if text.starts_with('-') {
                                        json!(text.parse::<i64>()?)
                                    } else {
                                        json!(text.parse::<u64>()?)
                                    }
                                }
                                ColumnType::MYSQL_TYPE_FLOAT | ColumnType::MYSQL_TYPE_DOUBLE => {
                                    let number = text.parse::<f64>()?;
                                    ensure!(number.is_finite(), "non-finite SQL number");
                                    json!(number)
                                }
                                _ => json!(text),
                            }
                        }
                        mysql::Value::Int(v) => json!(v),
                        mysql::Value::UInt(v) => json!(v),
                        mysql::Value::Float(v) => json!(v),
                        mysql::Value::Double(v) => json!(v),
                        _ => bail!(
                            "unsupported SQL cell type; select an explicit textual representation"
                        ),
                    };
                    let name = std::str::from_utf8(column.name_ref())?.to_owned();
                    ensure!(
                        mapped.insert(name, value).is_none(),
                        "duplicate SQL column name"
                    );
                }
                rows.push(Value::Object(mapped));
            }
        }
        Ok(rows)
    }
}
