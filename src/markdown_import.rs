//! Reviewed, one-shot editable Markdown bootstrap. Captured sources keep their own protocol.
use crate::{
    cli::*,
    import::{GitEntry, Imported},
    model::*,
    storage::{self, Store, put},
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
const KIND: &str = "editable_markdown";
const CONTRACT: &str = "scrolls.editable-markdown.v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub kind: String,
    pub contract: String,
    pub store_id: String,
    pub main_revision: String,
    pub schema_hash: String,
    pub actor: String,
    pub role: String,
    pub directory: PathBuf,
    pub commit: String,
    pub prefix: String,
    pub git_web_url: String,
    pub timestamp: String,
    pub inventory: Vec<GitEntry>,
    pub record_ids: Vec<String>,
    pub candidate_digest: String,
    pub summary: Value,
    pub digest: String,
}
impl Plan {
    fn seal(&mut self) -> Result<()> {
        self.digest.clear();
        self.digest = digest(serde_json::to_vec(self)?);
        Ok(())
    }
    fn verify(&self) -> Result<()> {
        let mut copy = self.clone();
        copy.seal()?;
        ensure!(
            self.kind == KIND
                && self.contract == CONTRACT
                && self.digest == copy.digest
                && self.summary["limits"] == crate::source_import::limits(),
            "stale_plan: Markdown plan integrity/interpreter mismatch"
        );
        web_url(&self.git_web_url)?;
        ensure!(
            !self.actor.trim().is_empty()
                && !self.actor.contains(['\n', '\r', '<', '>'])
                && self.role == "human",
            "stale_plan: Markdown bootstrap requires human attribution"
        );
        ensure!(
            [40, 64].contains(&self.commit.len())
                && self.commit.bytes().all(|c| c.is_ascii_hexdigit()),
            "stale_plan: expected resolved Git commit"
        );
        crate::import::safe_path(&self.prefix)?;
        Ok(())
    }
}
pub fn handles(command: &ImportCommand) -> Result<bool> {
    Ok(match command {
        ImportCommand::Plan {
            command: PlanCommand::Markdown { .. },
        } => true,
        ImportCommand::Apply { plan } => {
            ensure!(
                fs::metadata(plan)?.len() <= crate::source_import::TOTAL_LIMIT,
                "input_limit: plan exceeds 1 GiB"
            );
            let v: Value =
                serde_json::from_slice(&fs::read(plan)?).context("stale_plan: invalid plan")?;
            v.get("kind").is_some()
        }
        _ => false,
    })
}
fn web_url(url: &str) -> Result<()> {
    let rest = url
        .strip_prefix("https://github.com/")
        .context("Markdown import requires an explicit https://github.com/OWNER/REPO web URL")?;
    let parts: Vec<_> = rest.split('/').collect();
    ensure!(
        parts.len() == 2
            && parts.iter().all(|s| !s.is_empty()
                && *s != "."
                && *s != ".."
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c)))
            && !url.ends_with(".git"),
        "invalid Git web repository URL"
    );
    Ok(())
}
fn normalize(imported: &mut Imported, timestamp: &str) -> Result<()> {
    for r in imported.graph.records.values_mut() {
        if r.fields["created"].as_str().is_none() {
            r.created = timestamp.into();
        }
        if r.fields["last_reviewed"].as_str().is_none() {
            r.last_reviewed = r.created.clone();
        }
        r.original = None;
        r.original = Some(serde_json::to_value(&r)?);
    }
    for e in imported.graph.edges.values_mut() {
        e.created = timestamp.into();
    }
    Ok(())
}
fn candidate_digest(i: &Imported) -> Result<String> {
    Ok(digest(serde_json::to_vec(
        &json!({"records":i.graph.records,"edges":i.graph.edges,"files":i.files,"info":i.info,"routing":i.routing}),
    )?))
}
fn empty(s: &Store, g: &Graph) -> Result<()> {
    ensure!(
        g.records.is_empty()
            && g.edges.is_empty()
            && g.import_sources.is_empty()
            && g.import_members.is_empty()
            && s.rows::<Value>("source_files")?.is_empty()
            && s.one::<Value>("metadata", "import")?.is_none()
            && s.one::<Value>("metadata", "markdown_import")?.is_none(),
        "Markdown bootstrap requires an empty document/source store"
    );
    ensure!(
        s.proposals()?.iter().all(|p| p.status != "pending"),
        "Markdown bootstrap refuses pending proposals"
    );
    storage::shared_review::check_bootstrap_ready(s)?;
    ensure!(
        g.schema.revision() == crate::schema::Schema::default().revision(),
        "Markdown bootstrap requires the bundled knowledge schema"
    );
    Ok(())
}
fn response(s: &Store, data: Value) -> Result<Value> {
    storage::shared_review::receipt_response(s, data, "main")
}
pub fn run(cli: &Cli, command: &ImportCommand) -> Result<Value> {
    let connection = cli
        .connection
        .as_ref()
        .context("editable Markdown plan/apply requires --connection")?;
    ensure!(
        cli.proposal.is_none(),
        "Markdown bootstrap operates on main; omit --proposal"
    );
    ensure!(
        cli.role == Role::Human,
        "Markdown bootstrap requires --role human"
    );
    let actor = cli
        .actor
        .as_deref()
        .context("Markdown planning/apply requires --actor")?;
    ensure!(
        !actor.trim().is_empty() && !actor.contains(['\n', '\r', '<', '>']),
        "invalid actor"
    );
    match command {
        ImportCommand::Plan {
            command:
                PlanCommand::Markdown {
                    dir,
                    rev,
                    git_web_url,
                    out,
                },
        } => {
            web_url(git_web_url)?;
            let s = Store::open_shared(connection, &cli.store)?;
            let before = s.graph()?;
            empty(&s, &before)?;
            let mut i = crate::import::import_selected(dir, rev, actor, true)?;
            let timestamp = now();
            normalize(&mut i, &timestamp)?;
            let root = Path::new(&i.info.repository);
            let inventory = crate::import::inventory(root, &i.info.commit)?;
            let mut plan = Plan {
                kind: KIND.into(),
                contract: CONTRACT.into(),
                store_id: s
                    .one("metadata", "store_id")?
                    .context("missing store UUID")?,
                main_revision: s.head("main")?,
                schema_hash: before.schema.revision(),
                actor: actor.into(),
                role: cli.role.name().into(),
                directory: dir.canonicalize()?,
                commit: i.info.commit.clone(),
                prefix: i.info.prefix.clone(),
                git_web_url: git_web_url.clone(),
                timestamp,
                inventory,
                record_ids: i.graph.records.keys().cloned().collect(),
                candidate_digest: candidate_digest(&i)?,
                summary: Value::Null,
                digest: String::new(),
            };
            let references = crate::markdown_links::report(&i.files, &plan)?;
            plan.summary = json!({"import":i.info,"frozen":i.graph.records.values().filter(|r|r.frozen).count(),"source_bytes":i.files.iter().map(|f|STANDARD.decode(&f.bytes).map(|v|v.len())).collect::<std::result::Result<Vec<_>,_>>()?.iter().sum::<usize>(),"references":references,"limits":crate::source_import::limits()});
            plan.seal()?;
            crate::source_import::write_json_plan(out, &plan)?;
            response(
                &s,
                json!({"plan":out,"digest":plan.digest,"candidate_digest":plan.candidate_digest,"main_revision":plan.main_revision,"summary":plan.summary}),
            )
        }
        ImportCommand::Apply { plan: file } => {
            ensure!(
                fs::metadata(file)?.len() <= crate::source_import::TOTAL_LIMIT,
                "input_limit: plan exceeds 1 GiB"
            );
            let plan: Plan = serde_json::from_slice(&fs::read(file)?)
                .context("stale_plan: invalid Markdown plan")?;
            plan.verify()?;
            ensure!(
                actor == plan.actor && cli.role.name() == plan.role,
                "stale_plan: actor/role changed"
            );
            let op = cli
                .op_id
                .as_deref()
                .context("import apply requires --op-id ID")?;
            let hash = digest(serde_json::to_vec(
                &json!({"markdown_plan":plan.digest,"actor":actor,"role":plan.role}),
            )?);
            let s = if cli.dry_run {
                Store::open_shared(connection, &cli.store)?
            } else {
                Store::open_shared_recovery(connection, &cli.store, op)?
            };
            ensure!(
                s.one::<String>("metadata", "store_id")?.as_deref() == Some(&plan.store_id),
                "stale_plan: wrong store UUID"
            );
            if let Some(result) = s.receipt(op, &hash)? {
                return response(&s, result);
            }
            storage::shared_review::check_identity(&s, op, &hash)?;
            let before = s.graph()?;
            if let Some(existing) = s.one::<Plan>("metadata", "markdown_import")? {
                ensure!(
                    existing.candidate_digest == plan.candidate_digest
                        && existing.git_web_url == plan.git_web_url
                        && existing.commit == plan.commit
                        && existing.prefix == plan.prefix,
                    "source_update_unsupported: changed Markdown snapshot requires a fresh store"
                );
                health(&s, &before, None)?;
                let result = json!({"identical_replay":true,"import":plan.summary["import"],"branch":"main","op_id":op});
                if !cli.dry_run {
                    s.commit_shared_checked(
                        &crate::app::receipt(op, &hash, actor, &cli.command, &result)?,
                        "record identical Markdown replay",
                        actor,
                        false,
                        || Ok(()),
                    )?;
                }
                return response(
                    &s,
                    if cli.dry_run {
                        json!({"dry_run":true,"result":result})
                    } else {
                        result
                    },
                );
            }
            ensure!(
                s.head("main")? == plan.main_revision
                    && before.schema.revision() == plan.schema_hash,
                "stale_plan: main or schema changed; replan"
            );
            empty(&s, &before)?;
            let mut i = crate::import::import_selected(&plan.directory, &plan.commit, actor, true)?;
            normalize(&mut i, &plan.timestamp)?;
            ensure!(
                candidate_digest(&i)? == plan.candidate_digest
                    && crate::import::inventory(Path::new(&i.info.repository), &plan.commit)?
                        == plan.inventory
                    && i.graph.records.keys().cloned().collect::<Vec<_>>() == plan.record_ids,
                "stale_plan: pinned Markdown candidate changed"
            );
            let mut sql = storage::graph_changes(&before, &i.graph)?;
            for f in &i.files {
                sql.push_str(&put("source_files", &f.path, f)?);
            }
            sql.push_str(&put("metadata", "import", &i.info)?);
            sql.push_str(&put("metadata", "routing", &i.routing)?);
            sql.push_str(&put(
                "metadata",
                "import_edges",
                &i.graph.edges.values().collect::<Vec<_>>(),
            )?);
            sql.push_str(&put("metadata", "markdown_import", &plan)?);
            let result = json!({"import":i.info,"candidate_digest":plan.candidate_digest,"op_id":op,"branch":"main"});
            sql.push_str(&crate::app::receipt(
                op,
                &hash,
                actor,
                &cli.command,
                &result,
            )?);
            ensure!(
                sql.len()
                    <= s.shared_query_limit()
                        .context("missing shared SQL budget")?,
                "input_limit: encoded Markdown SQL request exceeds the shared packet budget"
            );
            if cli.dry_run {
                return response(
                    &s,
                    json!({"dry_run":true,"result":result,"sql_bytes":sql.len(),"shared_query_bytes":s.shared_query_limit()}),
                );
            }
            s.commit_shared_markdown_checked(&sql, "bootstrap editable Markdown", actor, || {
                let actual = s.graph()?;
                health(&s, &actual, None)?;
                ensure!(
                    actual.records == i.graph.records && actual.edges == i.graph.edges,
                    "Markdown bootstrap readback mismatch"
                );
                Ok(())
            })?;
            response(&s, result)
        }
        _ => anyhow::bail!("unsupported editable Markdown command"),
    }
}
/// Check original bytes and the complete imported baseline, independently of current edits.
pub fn health_content(
    g: &Graph,
    files: &[SourceFile],
    info: Option<&ImportInfo>,
    routing: &Value,
    edges: &[Edge],
    plan: Option<&Plan>,
) -> Result<Value> {
    let Some(plan) = plan else {
        return Ok(json!({"enabled":false}));
    };
    plan.verify()?;
    let info = info.context("markdown_original_integrity: missing import metadata")?;
    let mut originals = Graph::default();
    for id in &plan.record_ids {
        let current = g
            .records
            .get(id)
            .context("markdown_original_integrity: missing imported record")?;
        let value = current
            .original
            .as_ref()
            .context("markdown_original_integrity: missing original snapshot")?;
        let mut old: Record = serde_json::from_value(value.clone())?;
        ensure!(
            old.id == *id && old.original.is_none(),
            "markdown_original_integrity: invalid original identity"
        );
        old.original = Some(value.clone());
        originals.records.insert(id.clone(), old);
    }
    originals.edges = edges.iter().map(|e| (e.key(), e.clone())).collect();
    let mut sorted = files.to_vec();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let inventory: BTreeMap<_, _> = plan
        .inventory
        .iter()
        .map(|e| (e.path.as_str(), e))
        .collect();
    ensure!(
        inventory.len() == plan.inventory.len(),
        "markdown_original_integrity: duplicate Git inventory path"
    );
    let mut total = 0;
    for f in &sorted {
        crate::import::safe_path(&f.path)?;
        let bytes = STANDARD.decode(&f.bytes)?;
        total += bytes.len();
        ensure!(
            f.path.ends_with(".md")
                && digest(&bytes) == f.sha256
                && [0o100644, 0o100755].contains(&f.mode),
            "markdown_original_integrity: checksum/mode/selection mismatch"
        );
        let full = format!("{}/{}", plan.prefix, f.path);
        ensure!(
            inventory
                .get(full.as_str())
                .is_some_and(|e| e.mode == f.mode
                    && e.oid == f.git_oid
                    && e.size == bytes.len() as u64),
            "markdown_original_integrity: Git provenance mismatch"
        );
    }
    let imported = Imported {
        graph: originals,
        files: sorted,
        info: info.clone(),
        routing: routing.clone(),
    };
    ensure!(
        candidate_digest(&imported)? == plan.candidate_digest,
        "markdown_original_integrity: imported baseline changed"
    );
    Ok(
        json!({"enabled":true,"files":files.len(),"bytes":total,"checksums_valid":true,"candidate_digest":plan.candidate_digest}),
    )
}
pub fn health(s: &Store, g: &Graph, rev: Option<&str>) -> Result<Value> {
    let metadata: BTreeMap<String, Value> = if let Some(rev) = rev {
        s.sql(&format!(
            "SELECT id,payload FROM metadata AS OF {}",
            storage::revision_literal(rev)
        ))?
    } else {
        s.sql("SELECT id,payload FROM metadata")?
    }
    .iter()
    .map(|r| {
        Ok((
            r["id"].as_str().context("metadata key")?.into(),
            serde_json::from_str(r["payload"].as_str().context("metadata payload")?)?,
        ))
    })
    .collect::<Result<_>>()?;
    let Some(plan) = metadata.get("markdown_import") else {
        return Ok(json!({"enabled":false}));
    };
    let at = rev
        .map(|r| format!(" AS OF {}", storage::revision_literal(r)))
        .unwrap_or_default();
    let files = s
        .sql(&format!(
            "SELECT id,payload FROM source_files{at} ORDER BY id"
        ))?
        .iter()
        .map(|r| {
            let f: SourceFile =
                serde_json::from_str(r["payload"].as_str().context("Markdown source payload")?)?;
            ensure!(
                r["id"].as_str() == Some(f.path.as_str()),
                "markdown_original_integrity: source row identity mismatch"
            );
            Ok(f)
        })
        .collect::<Result<Vec<_>>>()?;
    health_content(
        g,
        &files,
        Some(&serde_json::from_value(
            metadata
                .get("import")
                .context("missing import metadata")?
                .clone(),
        )?),
        metadata.get("routing").context("missing import routing")?,
        &serde_json::from_value::<Vec<Edge>>(
            metadata
                .get("import_edges")
                .context("missing import edges")?
                .clone(),
        )?,
        Some(&serde_json::from_value(plan.clone())?),
    )
}
