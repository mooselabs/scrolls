//! Direct shared authoring. Local files are never recovery authority.
use super::{
    Store, put,
    session::{Connection, SharedSession},
};
use crate::{
    cli::{Cli, Cmd, Role, SchemaCommand},
    model::digest,
    schema::Schema,
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const TABLES: &[(&str, usize)] = &[
    ("import_sources", 128),
    ("import_files", 128),
    ("import_members", 128),
    ("records", 128),
    ("edges", 128),
    ("source_files", 2048),
    ("metadata", 128),
    ("operations", 256),
    ("registry", 128),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    host: String,
    port: u16,
    database: String,
    user: String,
    password: String,
}
fn connect(path: &Path) -> Result<SharedSession> {
    let bytes = fs::read(path).context("cannot read machine-local connection file")?;
    // Parsing errors may quote supplied values. Never echo a credential file.
    let settings: Settings = serde_json::from_slice(&bytes).map_err(|_| {
        anyhow::anyhow!("invalid connection JSON: expected host, port, database, user, password")
    })?;
    SharedSession::connect(
        &Connection {
            host: settings.host,
            port: settings.port,
            database: settings.database,
            user: settings.user,
            password: settings.password,
            connect_timeout: Duration::from_secs(5),
            io_timeout: Duration::from_secs(30),
            lock_timeout: Duration::from_secs(10),
        },
        "main",
    )
}

pub fn check_command(cli: &Cli) -> Result<()> {
    ensure!(
        cli.proposal.is_none(),
        "shared_unsupported: proposal branches"
    );
    ensure!(
        !matches!(
            cli.command,
            Cmd::Migrate
                | Cmd::Import { .. }
                | Cmd::Beads { .. }
                | Cmd::Consumed { .. }
                | Cmd::Review { .. }
                | Cmd::Proposals
                | Cmd::Context { bead: Some(_), .. }
                | Cmd::Schema {
                    command: SchemaCommand::Apply { .. }
                }
        ),
        "shared_unsupported: review, migration, import, Beads and audit paths require further qualification"
    );
    if cli.command.mutation() && !cli.dry_run {
        let op = cli
            .op_id
            .as_deref()
            .context("shared writes require --op-id for cross-client reconciliation")?;
        ensure!(
            !op.is_empty() && op.len() <= 256,
            "--op-id must contain 1–256 bytes"
        );
    }
    Ok(())
}
impl Store {
    fn shared_session(path: &Path, root: &Path) -> Result<Self> {
        Ok(Self {
            root: root.to_owned(),
            db: PathBuf::new(),
            binary: PathBuf::new(),
            _lock: None,
            shared: Some(connect(path)?),
        })
    }
    pub fn is_shared(&self) -> bool {
        self.shared.is_some()
    }
    pub fn open_shared(path: &Path, root: &Path) -> Result<Self> {
        let s = Self::shared_session(path, root)?;
        s.check_shared_format()?;
        Ok(s)
    }
    fn table_names(&self) -> Result<std::collections::BTreeSet<String>> {
        self.sql("SHOW TABLES")?
            .iter()
            .map(|row| {
                row.as_object()
                    .and_then(|v| v.values().next())
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .context("invalid table inventory")
            })
            .collect()
    }
    pub(super) fn check_shared_format(&self) -> Result<()> {
        let expected = TABLES.iter().map(|(name, _)| name.to_string()).collect();
        ensure!(
            self.table_names()? == expected,
            "unsupported_shared_format: missing or unexpected tables; initialization or maintenance required"
        );
        ensure!(
            self.one::<u32>("metadata", "schema")? == Some(5)
                && self.one::<u32>("metadata", "shared_writer")? == Some(1),
            "unsupported_shared_writer: this client requires shared format 5, writer 1; no automatic migration"
        );
        let uuid: String = self
            .one("metadata", "store_id")?
            .context("missing shared store identity")?;
        uuid::Uuid::parse_str(&uuid).context("invalid shared store identity")?;
        Ok(())
    }
    fn pristine(&self) -> Result<()> {
        ensure!(
            self.table_names()?.is_empty(),
            "shared_init_refused: existing or partially initialized database; no automatic conversion or repair"
        );
        ensure!(
            self.branches()? == ["main"],
            "shared_init_refused: additional branches"
        );
        ensure!(
            self.sql("SELECT commit_hash FROM dolt_log")?.len() == 1,
            "shared_init_refused: database has non-initial history"
        );
        Ok(())
    }
}

pub fn initialize(cli: &Cli, path: &Path, schema: &Schema) -> Result<Value> {
    let Cmd::Init {
        expect_base,
        expect_schema,
        ..
    } = &cli.command
    else {
        unreachable!()
    };
    let s = Store::shared_session(path, &cli.store)?;
    let ontology_digest = schema.revision();
    let op = cli.op_id.as_deref().unwrap_or("");
    let hash = digest(serde_json::to_vec(
        &json!({"command":"shared_init", "ontology":schema,
        "base":expect_base, "schema":expect_schema, "actor":cli.actor, "role":cli.role}),
    )?);
    if !s.table_names()?.is_empty() {
        s.check_shared_format()?;
        if !cli.dry_run {
            if let Some(result) = s.receipt(op, &hash)? {
                return Ok(
                    json!({"schema_version":2,"ok":true,"revision":s.revision()?,"branch":"main","data":result}),
                );
            }
        }
        anyhow::bail!("shared_init_refused: existing database; preserve its identity and history");
    }
    s.pristine()?;
    let base = s.revision()?;
    if cli.dry_run {
        return Ok(
            json!({"schema_version":2,"ok":true,"revision":base,"branch":"main",
            "data":{"dry_run":true,"base_revision":base,"ontology_digest":ontology_digest,"format":5,"writer":1}}),
        );
    }
    ensure!(
        cli.role == Role::Human,
        "shared_init_review_required: initialization requires --role human"
    );
    ensure!(
        expect_base.as_deref() == Some(&base) && expect_schema.as_deref() == Some(&ontology_digest),
        "shared_init_plan_stale: --expect-base and --expect-schema must match a reviewed init --dry-run plan"
    );
    let graph = crate::model::Graph {
        schema: schema.clone(),
        ..Default::default()
    };
    crate::policy::validate_change(&graph, &graph, &Default::default())?;
    let mut sql = String::new();
    for (table, size) in TABLES {
        sql.push_str(&format!(
            "CREATE TABLE {table} (id VARCHAR({size}) PRIMARY KEY, payload LONGTEXT NOT NULL);\n"
        ));
    }
    let store_id = uuid::Uuid::new_v4().to_string();
    sql.push_str(&put("metadata", "schema", &5)?);
    sql.push_str(&put("metadata", "shared_writer", &1)?);
    sql.push_str(&put("metadata", "store_id", &store_id)?);
    sql.push_str(&put("metadata", "ontology", schema)?);
    let result =
        json!({"store_id":store_id,"op_id":op,"ontology_digest":ontology_digest,"format":5});
    let actor = cli
        .actor
        .as_deref()
        .context("initialization requires actor")?;
    sql.push_str(&super::insert("operations", op, &json!({"op_id":op,"args_hash":hash,"actor":actor,"result":result,"committed_at":crate::model::now()}))?);
    s.transaction(&sql, "initialize shared Scrolls", actor)?;
    s.check_shared_format()?;
    Ok(json!({"schema_version":2,"ok":true,"revision":s.revision()?,"branch":"main","data":result}))
}
