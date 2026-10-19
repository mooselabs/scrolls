//! Durable shared publication. Intents live on a versioned auxiliary branch;
//! only a verified candidate is fast-forwarded into main.
use super::{Store, literal, revision_literal};
use crate::{
    cli::{Cli, Cmd, Role},
    model::{Graph, ImportInfo, Proposal, digest},
    mutation::Requirement,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const JOURNAL: &str = "scrolls-operations";
type Rows = BTreeMap<String, BTreeMap<String, String>>;
#[derive(Clone, Serialize)]
struct Snapshot {
    rows: Rows,
    schemas: BTreeMap<String, String>,
}
impl Snapshot {
    fn read(s: &Store, revision: Option<&str>) -> Result<Self> {
        // SHOW TABLES omits writable Dolt internal tables. The full versioned
        // diff from the pristine initialization root includes those tables.
        s.check_versioned_inventory(revision.unwrap_or("WORKING"))?;
        let at = revision
            .map(|r| format!(" AS OF {}", revision_literal(r)))
            .unwrap_or_default();
        let names: BTreeSet<String> = s
            .sql(&format!("SHOW TABLES{at}"))?
            .iter()
            .map(|r| {
                r.as_object()
                    .and_then(|o| o.values().next())
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .context("table inventory")
            })
            .collect::<Result<_>>()?;
        ensure!(
            names
                == super::shared::TABLES
                    .iter()
                    .map(|(t, _)| t.to_string())
                    .collect(),
            "dirty_state: unexpected SQL table inventory; preserve it"
        );
        if revision.is_none() {
            ensure!(
                s.sql("SELECT * FROM dolt_ignore")?.is_empty(),
                "dirty_state: unsupported Dolt ignore configuration"
            );
        }
        let mut out = Self {
            rows: BTreeMap::new(),
            schemas: BTreeMap::new(),
        };
        for (table, _) in super::shared::TABLES {
            let rows = s.sql(&format!(
                "SELECT id,SHA2(payload,256) AS hash FROM {table}{at} ORDER BY id"
            ))?;
            let mut values = BTreeMap::new();
            for row in rows {
                ensure!(
                    values
                        .insert(
                            row["id"].as_str().context("row id")?.into(),
                            row["hash"].as_str().context("row hash")?.into()
                        )
                        .is_none(),
                    "duplicate persisted row"
                );
            }
            out.rows.insert(table.to_string(), values);
            let row = s.sql(&format!("SHOW CREATE TABLE {table}{at}"))?;
            let schema = row
                .first()
                .and_then(Value::as_object)
                .and_then(|o| {
                    o.values()
                        .filter_map(Value::as_str)
                        .find(|v| v.starts_with("CREATE TABLE "))
                })
                .context("SQL schema definition")?;
            out.schemas.insert(table.to_string(), schema.into());
        }
        Ok(out)
    }
    fn hash(&self) -> Result<String> {
        Ok(digest(serde_json::to_vec(self)?))
    }
    fn apply(&self, changes: &[Change]) -> Result<Self> {
        let mut out = self.clone();
        for change in changes {
            let table = out
                .rows
                .get_mut(&change.table)
                .context("unknown change table")?;
            if let Some(payload) = &change.payload {
                table.insert(change.id.clone(), digest(payload));
            } else {
                table.remove(&change.id);
            }
        }
        Ok(out)
    }
}
fn merge(a: &Snapshot, ours: &Snapshot, theirs: &Snapshot) -> Result<Snapshot> {
    ensure!(
        a.schemas == ours.schemas && a.schemas == theirs.schemas,
        "merge_conflict: SQL schema changes require maintenance"
    );
    let mut out = ours.clone();
    for (table, base) in &a.rows {
        let ours = &ours.rows[table];
        let theirs = &theirs.rows[table];
        let ids: BTreeSet<_> = base
            .keys()
            .chain(ours.keys())
            .chain(theirs.keys())
            .collect();
        let mut rows = BTreeMap::new();
        for id in ids {
            let b = base.get(id);
            let o = ours.get(id);
            let t = theirs.get(id);
            let value = if o == t {
                o
            } else if o == b {
                t
            } else if t == b {
                o
            } else {
                bail!("merge_conflict: {table}/{id}; main remains unchanged")
            };
            if let Some(value) = value {
                rows.insert(id.clone(), value.clone());
            }
        }
        out.rows.insert(table.clone(), rows);
    }
    Ok(out)
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    table: String,
    id: String,
    payload: Option<String>,
}
impl Change {
    fn sql(&self) -> Result<String> {
        ensure!(
            [
                "records",
                "edges",
                "metadata",
                "operations",
                "import_sources",
                "import_files",
                "import_members"
            ]
            .contains(&self.table.as_str()),
            "invalid recovery change table"
        );
        if let Some(payload) = &self.payload {
            let _: Value = serde_json::from_str(payload)?;
            let verb = if self.table == "operations" || self.table.starts_with("import_") {
                "INSERT"
            } else {
                "REPLACE"
            };
            Ok(format!(
                "{verb} INTO {} (id,payload) VALUES ({},{});\n",
                self.table,
                literal(&self.id),
                literal(payload)
            ))
        } else {
            ensure!(self.table == "edges", "invalid recovery deletion");
            Ok(format!(
                "DELETE FROM edges WHERE id={};\n",
                literal(&self.id)
            ))
        }
    }
}
fn changes(sql: &str, op: &str) -> Result<Vec<Change>> {
    // Decode only the writer's canonical generated DML; recovery never executes
    // SQL text loaded from database metadata.
    let put = regex::Regex::new(
        r"^(REPLACE|INSERT) INTO ([a-z_]+) \(id,payload\) VALUES \(CONVERT\(0x([0-9a-f]+) USING utf8mb4\),CONVERT\(0x([0-9a-f]+) USING utf8mb4\)\)$",
    )?;
    let delete =
        regex::Regex::new(r"^DELETE FROM edges WHERE id=CONVERT\(0x([0-9a-f]+) USING utf8mb4\)$")?;
    let mut out = vec![];
    for statement in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let change = if let Some(c) = put.captures(statement) {
            ensure!(
                !c[2].starts_with("import_") || &c[1] == "INSERT",
                "captured imports require immutable inserts"
            );
            Change {
                table: c[2].into(),
                id: String::from_utf8(hex::decode(&c[3])?)?,
                payload: Some(String::from_utf8(hex::decode(&c[4])?)?),
            }
        } else if let Some(c) = delete.captures(statement) {
            Change {
                table: "edges".into(),
                id: String::from_utf8(hex::decode(&c[1])?)?,
                payload: None,
            }
        } else {
            bail!("unsupported generated recovery DML")
        };
        validate_change_frame(&change, op)?;
        out.push(change);
    }
    Ok(out)
}
fn validate_change_frame(change: &Change, op: &str) -> Result<()> {
    change.sql()?;
    if change.table == "metadata" {
        ensure!(
            change.id == "ontology"
                || change.id.starts_with("guard:")
                || change.id.starts_with("proposal/"),
            "recovery cannot change backend metadata"
        );
    }
    if change.table == "operations" {
        ensure!(change.id == op, "recovery receipt identity mismatch");
    }
    if change.table.starts_with("import_") {
        let payload = change
            .payload
            .as_deref()
            .context("captured imports cannot be deleted")?;
        match change.table.as_str() {
            "import_sources" => {
                let value: crate::source_import::Source = serde_json::from_str(payload)?;
                crate::source_import::namespace(&value.source_id)?;
                ensure!(
                    change.id == value.source_id && value.op_id == op,
                    "source_ownership: recovery source identity mismatch"
                );
            }
            "import_files" => {
                let value: crate::source_import::File = serde_json::from_str(payload)?;
                ensure!(
                    change.id == value.key(),
                    "source_ownership: recovery file key mismatch"
                );
                crate::source_import::file_health(&value)?;
            }
            "import_members" => {
                let value: crate::source_import::Member = serde_json::from_str(payload)?;
                ensure!(
                    change.id == value.row_key()
                        && value.record_id
                            == crate::source_import::record_id(&value.source_id, &value.key),
                    "source_ownership: recovery membership key mismatch"
                );
                crate::source_import::path(&value.path)?;
            }
            _ => bail!("unsupported capture table"),
        }
    }
    Ok(())
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WriteKind {
    Propose,
    Reject,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Plan {
    Write {
        kind: WriteKind,
        branch: String,
        base: String,
        before: String,
        after: String,
        changes: Vec<Change>,
        message: String,
    },
    Accept {
        branch: String,
        base: String,
        proposal: String,
        proposal_revision: String,
        base_snapshot: String,
        before: String,
        after: String,
        changes: Vec<Change>,
        message: String,
    },
    Abandon {
        operation: String,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum Stage {
    Prepared,
    Candidate { revision: String },
    Complete { revision: String },
    Failed { message: String },
    Abandoning { operation: String },
    Abandoned { operation: String },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    format: u32,
    op_id: String,
    args_hash: String,
    actor: String,
    role: Role,
    result: Value,
    plan: Plan,
    stage: Stage,
}
fn key(op: &str) -> String {
    format!("intent:{}", digest(op))
}
fn read(s: &Store, op: &str) -> Result<Option<Intent>> {
    let value = s.one_at::<Intent>("metadata", &key(op), JOURNAL)?;
    if let Some(i) = &value {
        ensure!(i.format == 1 && i.op_id == op, "invalid recovery identity");
        ensure!(
            i.args_hash.len() == 64 && i.args_hash.bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid recovery input digest"
        );
        match &i.plan {
            Plan::Write {
                branch,
                base,
                changes,
                ..
            }
            | Plan::Accept {
                branch,
                base,
                changes,
                ..
            } => {
                ensure!(
                    (branch.starts_with("proposal/") || branch.starts_with("review/"))
                        && branch.len() <= 255
                        && base.len() == 32,
                    "invalid owned recovery branch/base"
                );
                for change in changes {
                    validate_change_frame(change, op)?;
                    if change.table == "import_sources" {
                        let source: crate::source_import::Source = serde_json::from_str(
                            change.payload.as_deref().context("source payload")?,
                        )?;
                        ensure!(
                            source.actor == i.actor
                                && source.role == i.role.name()
                                && i.result["source"] == source.source_id
                                && i.result["candidate_digest"] == source.candidate_digest,
                            "source_ownership: recovery source attribution/result mismatch"
                        );
                    }
                }
            }
            Plan::Abandon { operation } => ensure!(
                operation != op && !operation.is_empty(),
                "invalid abandonment target"
            ),
        }
    }
    Ok(value)
}
fn active(s: &Store) -> Result<String> {
    s.sql("SELECT active_branch() AS branch")?
        .first()
        .and_then(|v| v["branch"].as_str())
        .map(str::to_owned)
        .context("active branch")
}
fn save_many(s: &Store, intents: &[&Intent], extra: &str, actor: &str) -> Result<()> {
    let previous = active(s)?;
    s.select_shared(JOURNAL)?;
    let mut sql = String::new();
    for i in intents {
        sql.push_str(&super::put("metadata", &key(&i.op_id), i)?);
    }
    sql.push_str(extra);
    s.transaction(&sql, "record shared operation recovery", actor)?;
    s.select_shared(&previous)
}
fn save(s: &Store, i: &Intent) -> Result<()> {
    save_many(s, &[i], "", &i.actor)
}
pub(crate) fn check_identity(s: &Store, op: &str, hash: &str) -> Result<()> {
    if let Some(i) = read(s, op)? {
        ensure!(
            i.args_hash == hash,
            "--op-id {op} was already used with different arguments or attribution"
        );
    }
    Ok(())
}
/// Nonterminal intents reserve namespaces before a proposal branch exists.
/// Completed receipts retain replay precedence even if the journal's final
/// bookkeeping acknowledgement was lost.
pub(crate) fn check_import_reservation(s: &Store, source: &str, op: &str) -> Result<()> {
    for row in s.sql(&format!("SELECT id,JSON_UNQUOTE(JSON_EXTRACT(payload,'$.op_id')) AS operation FROM metadata AS OF {} WHERE id LIKE 'intent:%' AND (JSON_EXTRACT(payload,'$.stage.state') IS NULL OR JSON_UNQUOTE(JSON_EXTRACT(payload,'$.stage.state')) NOT IN ('complete','failed','abandoned'))", revision_literal(JOURNAL)))? {
        let owner = row["operation"].as_str().context("invalid reservation identity")?;
        ensure!(row["id"] == key(owner), "invalid reservation key");
        if owner == op { continue; }
        let intent = read(s, owner)?.context("missing reservation")?;
        if s.receipt(owner, &intent.args_hash)?.is_some() { continue; }
        if let Plan::Write { changes, .. } = &intent.plan {
            ensure!(!changes.iter().any(|c| c.table == "import_sources" && c.id == source),
                "source_reserved: source {source} belongs to interrupted operation {owner}; recover or explicitly abandon it");
        }
    }
    Ok(())
}
fn check_import_publication(s: &Store, changes: &[Change], op: &str) -> Result<()> {
    for c in changes.iter().filter(|c| c.table == "import_sources") {
        check_import_reservation(s, &c.id, op)?;
        ensure!(
            s.one_at::<Value>("import_sources", &c.id, "main")?
                .is_none(),
            "source_update_unsupported: recovery namespace {} is already published; reconcile the original operation",
            c.id
        );
        for p in s.proposals()?.into_iter().filter(|p| p.status == "pending") {
            ensure!(
                s.one_at::<Value>("import_sources", &c.id, &p.branch)?
                    .is_none(),
                "source_update_unsupported: namespace {} is reserved by {}",
                c.id,
                p.branch
            );
        }
    }
    Ok(())
}
pub(crate) fn check_dirt(s: &Store, op: &str) -> Result<()> {
    let dirty = s.sql("SELECT name FROM dolt_branches WHERE dirty = 1")?;
    if dirty.is_empty() {
        return Ok(());
    }
    let mut i = read(s, op)?
        .context("dirty_state: no matching durable recovery owner; preserve all working sets")?;
    if let Plan::Abandon { operation } = &i.plan {
        let original = read(s, operation)?.context("missing original recovery owner")?;
        ensure!(
            matches!(&original.stage, Stage::Abandoning { operation } if operation == op),
            "recovery_required: abandonment ownership changed"
        );
        i = original;
    }
    let branch = match &i.plan {
        Plan::Write { branch, .. } | Plan::Accept { branch, .. } => branch,
        Plan::Abandon { .. } => bail!("inspect the original operation for recovery"),
    };
    ensure!(
        dirty.iter().all(|r| r["name"] == *branch),
        "dirty_state: an unrelated working set is not owned by this operation"
    );
    Ok(())
}
fn ancestor(s: &Store, older: &str, newer: &str) -> Result<bool> {
    Ok(s.merge_base(older, newer)? == older)
}
pub(crate) fn verify_receipt(s: &Store, op: &str, hash: &str) -> Result<()> {
    let Some(i) = read(s, op)? else { return Ok(()) };
    ensure!(i.args_hash == hash, "operation input mismatch");
    if let Plan::Accept { after, .. } = &i.plan {
        let revision = match &i.stage {
            Stage::Candidate { revision } | Stage::Complete { revision } => revision,
            _ => bail!("recovery_required: acceptance receipt has no durable validated candidate"),
        };
        ensure!(
            s.one_at::<Value>("operations", op, "main")?.is_some()
                && ancestor(s, revision, "main")?,
            "recovery_required: candidate receipt is not publication into main"
        );
        ensure!(
            Snapshot::read(s, Some(revision))?.hash()? == *after,
            "recovery_required: published candidate differs from the recorded validation"
        );
    }
    Ok(())
}
pub(crate) fn receipt_response(s: &Store, result: Value, fallback: &str) -> Result<Value> {
    let branch = result["branch"].as_str().unwrap_or(fallback).to_owned();
    ensure!(
        branch == "main" || branch == JOURNAL || branch.starts_with("proposal/"),
        "invalid receipt branch"
    );
    s.select_shared(&branch)?;
    crate::app::wrap(s, result, &branch)
}
fn existing(s: &Store, cli: &Cli, op: &str, hash: &str) -> Result<Option<Intent>> {
    let i = read(s, op)?;
    if let Some(i) = &i {
        ensure!(
            i.args_hash == hash
                && Some(i.actor.as_str()) == cli.actor.as_deref()
                && i.role == cli.role,
            "--op-id {op} was already used with different arguments or attribution"
        );
    }
    Ok(i)
}
fn payloads(s: &Store, table: &str, rev: &str) -> Result<BTreeMap<String, String>> {
    s.sql(&format!(
        "SELECT id,payload FROM {table} AS OF {} ORDER BY id",
        revision_literal(rev)
    ))?
    .iter()
    .map(|r| {
        Ok((
            r["id"].as_str().context("row id")?.into(),
            r["payload"].as_str().context("row payload")?.into(),
        ))
    })
    .collect()
}
fn chosen<T: Clone>(
    table: &str,
    merged: &Snapshot,
    ours: &Snapshot,
    a: &BTreeMap<String, T>,
    b: &BTreeMap<String, T>,
) -> Result<BTreeMap<String, T>> {
    merged.rows[table]
        .iter()
        .map(|(id, hash)| {
            let value = if ours.rows[table].get(id) == Some(hash) {
                a.get(id)
            } else {
                b.get(id)
            };
            Ok((
                id.clone(),
                value.context("merged row identity missing")?.clone(),
            ))
        })
        .collect()
}
fn dry_candidate(
    s: &Store,
    main: &Graph,
    base: &str,
    proposal: &str,
    ours: &Snapshot,
    merged: &Snapshot,
    cli: &Cli,
) -> Result<()> {
    let who = cli.actor.as_deref().context("actor")?;
    let role = cli.role;
    let a = s.graph_at(base)?;
    let b = s.graph_at(proposal)?;
    let ma = payloads(s, "metadata", base)?;
    let mb = payloads(s, "metadata", proposal)?;
    let metadata = chosen("metadata", merged, ours, &ma, &mb)?;
    let mut g = Graph {
        schema: serde_json::from_str(metadata.get("ontology").context("missing ontology")?)?,
        records: chosen("records", merged, ours, &a.records, &b.records)?,
        edges: chosen("edges", merged, ours, &a.edges, &b.edges)?,
        import_sources: chosen(
            "import_sources",
            merged,
            ours,
            &a.import_sources,
            &b.import_sources,
        )?,
        ..Graph::default()
    };
    let am = a
        .import_members
        .values()
        .map(|m| (m.row_key(), m.clone()))
        .collect();
    let bm = b
        .import_members
        .values()
        .map(|m| (m.row_key(), m.clone()))
        .collect();
    g.import_members = chosen("import_members", merged, ours, &am, &bm)?
        .into_values()
        .map(|m| (m.record_id.clone(), m))
        .collect();
    crate::source_import::validate(&g)?;
    let af = s
        .import_files_at(base)?
        .into_iter()
        .map(|f| (f.key(), f))
        .collect();
    let bf = s
        .import_files_at(proposal)?
        .into_iter()
        .map(|f| (f.key(), f))
        .collect();
    let files = chosen("import_files", merged, ours, &af, &bf)?
        .into_values()
        .collect::<Vec<_>>();
    crate::source_import::health(&g, &files)?;
    let debt = metadata
        .get("import")
        .map(|p| serde_json::from_str::<ImportInfo>(p))
        .transpose()?
        .map(|v| v.lint_debt)
        .unwrap_or_default();
    let mut requirements = vec![];
    for (id, payload) in &metadata {
        if id.starts_with("guard:") && !ours.rows["metadata"].contains_key(id) {
            requirements.extend(serde_json::from_str::<Vec<Requirement>>(payload)?);
        }
    }
    let routing = metadata
        .get("routing")
        .map(|p| serde_json::from_str(p))
        .transpose()?
        .unwrap_or(json!({}));
    crate::app::validate_review_candidate(main, &g, &debt, &requirements, &routing, who, role)
}
fn commit_changes(
    s: &Store,
    changes: &[Change],
    message: &str,
    actor: &str,
    owned: bool,
    validate: impl FnOnce() -> Result<()>,
) -> Result<String> {
    let mut sql = String::new();
    for c in changes {
        sql.push_str(&c.sql()?);
    }
    s.commit_shared_checked(&sql, message, actor, owned, validate)
}
fn branch_state(s: &Store, i: &Intent) -> Result<String> {
    let (branch, base, before, after, base_snapshot) = match &i.plan {
        Plan::Write {
            branch,
            base,
            before,
            after,
            ..
        } => (branch, base, before, after, before),
        Plan::Accept {
            branch,
            base,
            before,
            after,
            base_snapshot,
            ..
        } => (branch, base, before, after, base_snapshot),
        Plan::Abandon { .. } => return Ok("control".into()),
    };
    if !s.branches()?.contains(branch) {
        return Ok("absent".into());
    }
    let previous = active(s)?;
    s.select_shared(branch)?;
    let head = s.head(branch)?;
    let hash = Snapshot::read(s, None)?.hash()?;
    let staged = Snapshot::read(s, Some("STAGED"))?.hash()?;
    let staged_known = [&**base_snapshot, &**before, &**after].contains(&staged.as_str());
    let merging = s.sql("SELECT * FROM dolt_merge_status WHERE is_merging = 1")?;
    let merge_known = merging.is_empty()
        || matches!(&i.plan,
        Plan::Accept { proposal_revision, .. } if merging.len() == 1
            && merging[0]["source_commit"] == *proposal_revision
            && merging[0]["target"] == format!("refs/heads/{branch}")
            && merging[0]["unmerged_tables"] == "");
    let dirty = !s.sql("SELECT * FROM dolt_status")?.is_empty() || !merging.is_empty();
    let state = if !staged_known || !merge_known {
        "unknown"
    } else if !dirty && head == *base && hash == *base_snapshot {
        "base"
    } else if head == *base && hash == *before {
        "merged"
    } else if head == *base && hash == *after {
        "ready"
    } else if !dirty && hash == *after {
        "candidate"
    } else {
        "unknown"
    };
    s.select_shared(&previous)?;
    Ok(state.into())
}
fn publish_write(s: &Store, mut i: Intent) -> Result<Value> {
    match &i.stage {
        Stage::Failed { message } => bail!("operation_failed: {message}"),
        Stage::Abandoning { .. } | Stage::Abandoned { .. } => {
            bail!("operation_abandoned: inspect recover {}", i.op_id)
        }
        _ => {}
    }
    let Plan::Write {
        kind,
        branch,
        base,
        before,
        after,
        changes,
        message,
    } = &i.plan
    else {
        bail!("invalid write intent")
    };
    if matches!(kind, WriteKind::Reject) {
        let p = s
            .proposals()?
            .into_iter()
            .find(|p| p.branch == *branch)
            .context("proposal disappeared")?;
        ensure!(
            p.status == "pending",
            "stale_operation: proposal is already {}",
            p.status
        );
    }
    check_import_publication(s, changes, &i.op_id)?;
    let state = branch_state(s, &i)?;
    if state == "absent" {
        ensure!(
            matches!(kind, WriteKind::Propose),
            "recovery_required: existing proposal disappeared"
        );
        s.sql(&format!(
            "CALL DOLT_BRANCH({}, {})",
            literal(branch),
            literal(base)
        ))?;
    } else {
        ensure!(
            ["base", "ready"].contains(&state.as_str()),
            "stale_operation: owned proposal changed; preserve it"
        );
    }
    s.select_shared(branch)?;
    ensure!(
        s.head(branch)? == *base,
        "stale_operation: proposal revision changed"
    );
    let graph = s.graph_at(base)?;
    let hash = Snapshot::read(s, None)?.hash()?;
    ensure!(
        hash == *before || hash == *after,
        "dirty_state: unexpected proposal contents"
    );
    let apply = if hash == *after {
        &[][..]
    } else {
        changes.as_slice()
    };
    let revision = commit_changes(s, apply, message, &i.actor, hash == *after, || {
        ensure!(
            Snapshot::read(s, None)?.hash()? == *after,
            "dirty_state: candidate differs from durable intent"
        );
        let candidate = s.graph()?;
        crate::source_import::health(&candidate, &s.import_files()?)?;
        crate::policy::validate_change(&graph, &candidate, &BTreeSet::new())?;
        candidate.schema.validate()?;
        for pin in &candidate.schema.pins {
            candidate.resolve(pin)?;
        }
        Ok(())
    })?;
    i.stage = Stage::Complete { revision };
    save(s, &i)?;
    Ok(i.result)
}
pub(crate) fn publish_proposal(
    s: &Store,
    cli: &Cli,
    p: &Proposal,
    sql: &str,
    op: &str,
    hash: &str,
    result: &Value,
) -> Result<Value> {
    let before = Snapshot::read(s, Some(&p.base))?;
    let delta = changes(sql, op)?;
    check_import_publication(s, &delta, op)?;
    ensure!(
        !before.rows["operations"].contains_key(op),
        "operation already exists"
    );
    let i = Intent {
        format: 1,
        op_id: op.into(),
        args_hash: hash.into(),
        actor: cli.actor.clone().context("actor")?,
        role: cli.role,
        result: result.clone(),
        plan: Plan::Write {
            kind: WriteKind::Propose,
            branch: p.branch.clone(),
            base: p.base.clone(),
            before: before.hash()?,
            after: before.apply(&delta)?.hash()?,
            changes: delta,
            message: format!("propose {} by {}", p.branch, p.actor),
        },
        stage: Stage::Prepared,
    };
    ensure!(
        read(s, op)?.is_none(),
        "recovery_required: resume the reserved operation"
    );
    save(s, &i)?;
    publish_write(s, i)
}
pub(crate) fn reject(
    s: &Store,
    cli: &Cli,
    p: &Proposal,
    op: &str,
    hash: &str,
    result: &Value,
) -> Result<Value> {
    let base = s.head(&p.branch)?;
    let before = Snapshot::read(s, Some(&base))?;
    let actor = cli.actor.as_deref().context("actor")?;
    let sql = super::put("metadata", &p.branch, p)?
        + &crate::app::receipt(op, hash, actor, &cli.command, result)?;
    let delta = changes(&sql, op)?;
    let i = Intent {
        format: 1,
        op_id: op.into(),
        args_hash: hash.into(),
        actor: actor.into(),
        role: cli.role,
        result: result.clone(),
        plan: Plan::Write {
            kind: WriteKind::Reject,
            branch: p.branch.clone(),
            base,
            before: before.hash()?,
            after: before.apply(&delta)?.hash()?,
            changes: delta,
            message: format!("reject {} by {actor}", p.branch),
        },
        stage: Stage::Prepared,
    };
    save(s, &i)?;
    publish_write(s, i)
}

fn token(cli: &Cli) -> Result<&str> {
    let Cmd::Review {
        action,
        review_token,
        ..
    } = &cli.command
    else {
        bail!("recovery input is not a review decision")
    };
    ensure!(action == "accept", "recovery input is not acceptance");
    review_token
        .as_deref()
        .context("review acceptance requires its original token")
}
fn confirm_accept_request(s: &Store, cli: &Cli, i: &Intent) -> Result<()> {
    ensure!(
        cli.role == Role::Human && i.role == Role::Human,
        "proposal review decisions require --role human"
    );
    let Plan::Accept {
        base,
        proposal,
        proposal_revision,
        ..
    } = &i.plan
    else {
        bail!("not an acceptance intent")
    };
    let current = s.head("main")?;
    let proposed = s.head(proposal)?;
    let original_token = i.result["review_token"]
        .as_str()
        .context("missing original review token")?;
    if !matches!(cli.command, Cmd::Recover { resume: true, .. }) {
        ensure!(
            token(cli)? == original_token,
            "recovery review token changed"
        );
    }
    crate::review_token::ReviewToken::decode(original_token)?
        .check(proposal, &current, &proposed)?;
    ensure!(
        current == *base && proposed == *proposal_revision,
        "stale_review: original inspected revisions changed"
    );
    let p = s
        .proposals()?
        .into_iter()
        .find(|p| p.branch == *proposal)
        .context("proposal disappeared")?;
    ensure!(p.status == "pending", "proposal is already {}", p.status);
    Ok(())
}
fn candidate_ok(s: &Store, i: &Intent, revision: &str) -> Result<()> {
    let Plan::Accept {
        base,
        proposal_revision,
        after,
        ..
    } = &i.plan
    else {
        bail!("not acceptance")
    };
    ensure!(
        s.head(&format!("{revision}^1"))? == *base
            && s.head(&format!("{revision}^2"))? == *proposal_revision,
        "recovery_required: unexpected candidate parents"
    );
    ensure!(
        Snapshot::read(s, Some(revision))?.hash()? == *after,
        "dirty_state: committed candidate differs from durable intent"
    );
    let r = s
        .one_at::<Value>("operations", &i.op_id, revision)?
        .context("candidate has no receipt")?;
    ensure!(
        r["args_hash"] == i.args_hash && r["result"] == i.result,
        "candidate receipt mismatch"
    );
    Ok(())
}
fn reset_owned(s: &Store, i: &Intent) -> Result<()> {
    let (branch, base) = match &i.plan {
        Plan::Write { branch, base, .. } | Plan::Accept { branch, base, .. } => (branch, base),
        Plan::Abandon { .. } => bail!("cannot reset a control operation"),
    };
    let state = branch_state(s, i)?;
    ensure!(
        ["base", "merged", "ready", "absent"].contains(&state.as_str()),
        "dirty_state: contents are not the exact owned working set; preserve them"
    );
    if state == "absent" {
        return Ok(());
    }
    let previous = active(s)?;
    s.select_shared(branch)?;
    ensure!(
        s.head(branch)? == *base,
        "dirty_state: never reset an advanced branch"
    );
    // The entire schema and row inventory matched a recorded state, not merely
    // a branch-name prefix. This reset can never target main.
    ensure!(
        branch != "main" && branch != JOURNAL,
        "invalid reset target"
    );
    s.sql("CALL DOLT_RESET('--hard')")?;
    super::session::SqlSession::clean(s.shared.as_ref().unwrap())?;
    s.select_shared(&previous)
}
fn publish_accept(s: &Store, cli: &Cli, main: &Graph, mut i: Intent) -> Result<Value> {
    match &i.stage {
        Stage::Failed { message } => bail!("operation_failed: {message}"),
        Stage::Abandoning { .. } | Stage::Abandoned { .. } => {
            bail!("operation_abandoned: inspect recover {}", i.op_id)
        }
        _ => {}
    }
    confirm_accept_request(s, cli, &i)?;
    let Plan::Accept {
        branch,
        base,
        proposal_revision,
        before,
        after,
        changes,
        message,
        ..
    } = &i.plan
    else {
        bail!("invalid acceptance")
    };
    let branch = branch.clone();
    let base = base.clone();
    let proposal_revision = proposal_revision.clone();
    let before = before.clone();
    let after = after.clone();
    let changes = changes.clone();
    let message = message.clone();
    let state = branch_state(s, &i)?;
    if state == "absent" {
        s.sql(&format!(
            "CALL DOLT_BRANCH({}, {})",
            literal(&branch),
            literal(&base)
        ))?;
    }
    s.select_shared(&branch)?;
    let mut candidate = match &i.stage {
        Stage::Candidate { revision } | Stage::Complete { revision } => Some(revision.clone()),
        _ => None,
    };
    if candidate.is_none() && state == "candidate" {
        let revision = s.head(&branch)?;
        candidate_ok(s, &i, &revision)?;
        crate::app::validate_merged_review(s, main, &i.actor, i.role)?;
        candidate = Some(revision);
    }
    if candidate.is_none() {
        ensure!(
            ["absent", "base", "merged", "ready"].contains(&state.as_str()),
            "dirty_state: unknown temporary state; preserve it"
        );
        ensure!(
            s.head(&branch)? == base,
            "dirty_state: temporary branch advanced unexpectedly"
        );
        if state == "base" || state == "absent" {
            s.sql(&format!(
                "CALL DOLT_MERGE('--no-commit','--no-ff',{})",
                literal(&proposal_revision)
            ))?;
        }
        ensure!(
            s.sql("SELECT * FROM dolt_conflicts")?.is_empty(),
            "merge_conflict: preserve the temporary branch for inspection"
        );
        let observed = Snapshot::read(s, None)?.hash()?;
        ensure!(
            observed == before || observed == after,
            "dirty_state: merge differs from recorded inputs"
        );
        let delta = if observed == after {
            &[][..]
        } else {
            changes.as_slice()
        };
        let committed = commit_changes(s, delta, &message, &i.actor, true, || {
            ensure!(
                Snapshot::read(s, None)?.hash()? == after,
                "dirty_state: candidate differs from recorded changes"
            );
            crate::app::validate_merged_review(s, main, &i.actor, i.role)
        });
        match committed {
            Ok(revision) => {
                candidate_ok(s, &i, &revision)?;
                candidate = Some(revision);
            }
            Err(error) => {
                // Only roll back a provably uncommitted, fully known candidate.
                // If the connection or commit outcome is uncertain, preserve it.
                if s.head(&branch).ok().as_deref() == Some(base.as_str()) {
                    if let Ok(state) = branch_state(s, &i) {
                        if ["base", "merged", "ready"].contains(&state.as_str()) {
                            reset_owned(s, &i)?;
                            i.stage = Stage::Failed {
                                message: format!("{error:#}"),
                            };
                            save(s, &i)?;
                        }
                    }
                }
                return Err(error);
            }
        }
    }
    let candidate = candidate.context("missing candidate")?;
    ensure!(
        s.head(&branch)? == candidate,
        "dirty_state: candidate branch changed"
    );
    candidate_ok(s, &i, &candidate)?;
    super::session::SqlSession::clean(s.shared.as_ref().unwrap())?;
    crate::app::validate_merged_review(s, main, &i.actor, i.role)?;
    if !matches!(&i.stage,Stage::Candidate{revision}|Stage::Complete{revision} if revision==&candidate)
    {
        i.stage = Stage::Candidate {
            revision: candidate.clone(),
        };
        save(s, &i)?;
    }
    // This durable candidate pointer precedes advancement of main. A receipt
    // confined to the temporary branch is never acknowledged as publication.
    s.select_shared("main")?;
    confirm_accept_request(s, cli, &i)?;
    s.sql(&format!("CALL DOLT_MERGE('--ff-only',{})",literal(&candidate)))
        .context("indeterminate_publication: reconcile the same operation ID; never refresh its token automatically")?;
    verify_receipt(s, &i.op_id, &i.args_hash)?;
    super::session::SqlSession::clean(s.shared.as_ref().unwrap())?;
    Ok(i.result)
}
pub(crate) fn accept(
    s: &Store,
    cli: &Cli,
    main: &Graph,
    p: &Proposal,
    revisions: (&str, &str),
    op: &str,
    hash: &str,
) -> Result<Value> {
    let (base, proposal_revision) = revisions;
    let ancestor = s.merge_base(base, proposal_revision)?;
    let a = Snapshot::read(s, Some(&ancestor))?;
    let ours = Snapshot::read(s, Some(base))?;
    let theirs = Snapshot::read(s, Some(proposal_revision))?;
    let merged = merge(&a, &ours, &theirs)?;
    let actor = cli.actor.as_deref().context("actor")?;
    dry_candidate(s, main, base, proposal_revision, &ours, &merged, cli)?;
    if cli.dry_run {
        return Ok(
            json!({"dry_run":true,"validated":true,"proposal":p.branch,"base":base,"validated_base":base,"proposal_revision":proposal_revision,"review_token":token(cli)?}),
        );
    }
    let mut proposal = p.clone();
    proposal.status = "accepted".into();
    if let Cmd::Review { reason, .. } = &cli.command {
        proposal.reason = reason.clone();
    }
    let result = json!({"branch":"main","proposal":proposal,"op_id":op,"validated_base":base,"proposal_revision":proposal_revision,"review_token":token(cli)?});
    let sql = super::put("metadata", &p.branch, &proposal)?
        + &crate::app::receipt(op, hash, actor, &cli.command, &result)?;
    let delta = changes(&sql, op)?;
    ensure!(
        !merged.rows["operations"].contains_key(op),
        "operation already exists"
    );
    let i = Intent {
        format: 1,
        op_id: op.into(),
        args_hash: hash.into(),
        actor: actor.into(),
        role: cli.role,
        result,
        plan: Plan::Accept {
            branch: format!("review/{}", uuid::Uuid::new_v4().simple()),
            base: base.into(),
            proposal: p.branch.clone(),
            proposal_revision: proposal_revision.into(),
            base_snapshot: ours.hash()?,
            before: merged.hash()?,
            after: merged.apply(&delta)?.hash()?,
            changes: delta,
            message: format!("accept {} by {actor}", p.branch),
        },
        stage: Stage::Prepared,
    };
    ensure!(
        read(s, op)?.is_none(),
        "recovery_required: resume the reserved operation"
    );
    save(s, &i)?;
    publish_accept(s, cli, main, i)
}
pub(crate) fn resume(
    s: &Store,
    cli: &Cli,
    main: &Graph,
    op: &str,
    hash: &str,
) -> Result<Option<Value>> {
    let Some(i) = existing(s, cli, op, hash)? else {
        return Ok(None);
    };
    let result = match &i.plan {
        Plan::Write { .. } => publish_write(s, i)?,
        Plan::Accept { .. } => publish_accept(s, cli, main, i)?,
        Plan::Abandon { .. } => finish_abandon(s, i)?,
    };
    Ok(Some(result))
}
fn published(s: &Store, i: &Intent) -> Result<bool> {
    if let Plan::Accept { .. } = &i.plan {
        if let Stage::Candidate { revision } | Stage::Complete { revision } = &i.stage {
            return ancestor(s, revision, "main");
        }
    }
    Ok(s.receipt(&i.op_id, &i.args_hash)?.is_some())
}
fn finish_abandon(s: &Store, mut control: Intent) -> Result<Value> {
    let Plan::Abandon { operation } = &control.plan else {
        bail!("not an abandonment")
    };
    let mut original = read(s, operation)?.context("original recovery intent missing")?;
    ensure!(
        !published(s, &original)?,
        "already_published: abandonment cannot undo published changes"
    );
    ensure!(
        matches!(&original.stage,Stage::Abandoning{operation} if operation==&control.op_id),
        "recovery_required: abandonment ownership changed"
    );
    let state = branch_state(s, &original)?;
    if ["base", "merged", "ready", "absent"].contains(&state.as_str()) {
        reset_owned(s, &original)?;
    } else {
        // A clean branch may have advanced through another valid operation.
        // Cancel only our intent, never reset somebody else's committed work.
        let branch = match &original.plan {
            Plan::Write { branch, .. } | Plan::Accept { branch, .. } => branch,
            Plan::Abandon { .. } => bail!("cannot abandon a recovery control"),
        };
        let previous = active(s)?;
        s.select_shared(branch)?;
        super::session::SqlSession::clean(s.shared.as_ref().unwrap())?;
        s.select_shared(&previous)?;
    }
    original.stage = Stage::Abandoned {
        operation: control.op_id.clone(),
    };
    control.stage = Stage::Complete {
        revision: s.head("main")?,
    };
    let receipt = super::insert(
        "operations",
        &control.op_id,
        &json!({"op_id":control.op_id,"args_hash":control.args_hash,"actor":control.actor,"result":control.result,"committed_at":crate::model::now()}),
    )?;
    save_many(s, &[&original, &control], &receipt, &control.actor)?;
    s.select_shared(JOURNAL)?;
    Ok(control.result)
}
pub(crate) fn recover(s: &Store, cli: &Cli, op: &str, hash: &str) -> Result<Value> {
    let Cmd::Recover {
        operation,
        abandon,
        resume: requested_resume,
        reason,
    } = &cli.command
    else {
        bail!("not recovery")
    };
    let mut original = read(s, operation)?
        .context("unknown shared operation; inspect its original command/receipt")?;
    if *requested_resume && !cli.dry_run {
        ensure!(
            cli.op_id.is_none(),
            "resume uses the original operation ID; omit --op-id"
        );
        ensure!(
            Some(original.actor.as_str()) == cli.actor.as_deref() && original.role == cli.role,
            "recovery attribution must match the original actor and role"
        );
        if let Some(result) = s.receipt(operation, &original.args_hash)? {
            return receipt_response(s, result, "main");
        }
        let main = s.graph()?;
        let result = resume(s, cli, &main, operation, &original.args_hash)?
            .context("missing recovery intent")?;
        return receipt_response(s, result, "main");
    }
    let is_published = published(s, &original)?;
    let state = branch_state(s, &original)?;
    if !abandon || cli.dry_run {
        return crate::app::wrap(
            s,
            json!({"operation":operation,"stage":original.stage,"working_state":state,"published":is_published,"dry_run":cli.dry_run}),
            "main",
        );
    }
    ensure!(cli.role == Role::Human, "abandonment requires --role human");
    ensure!(
        !is_published,
        "already_published: abandonment cannot undo publication"
    );
    ensure!(
        !matches!(original.plan, Plan::Abandon { .. }),
        "inspect the original operation, not a recovery control"
    );
    let reason = reason
        .as_deref()
        .filter(|v| !v.trim().is_empty())
        .context("abandonment requires --reason")?;
    ensure!(
        !matches!(
            original.stage,
            Stage::Abandoning { .. } | Stage::Abandoned { .. }
        ),
        "recovery_required: resume the existing abandonment operation"
    );
    // Verify any dirty set before recording an abandonment. Unknown data stays
    // untouched, including unknown rows on an otherwise recognizable branch.
    if state == "unknown" {
        let branch = match &original.plan {
            Plan::Write { branch, .. } | Plan::Accept { branch, .. } => branch,
            _ => unreachable!(),
        };
        let previous = active(s)?;
        s.select_shared(branch)?;
        super::session::SqlSession::clean(s.shared.as_ref().unwrap())?;
        s.select_shared(&previous)?;
    }
    let control = Intent {
        format: 1,
        op_id: op.into(),
        args_hash: hash.into(),
        actor: cli.actor.clone().context("actor")?,
        role: cli.role,
        result: json!({"branch":JOURNAL,"abandoned":operation,"reason":reason,"op_id":op}),
        plan: Plan::Abandon {
            operation: operation.clone(),
        },
        stage: Stage::Prepared,
    };
    original.stage = Stage::Abandoning {
        operation: op.into(),
    };
    save_many(s, &[&original, &control], "", &control.actor)?;
    let result = finish_abandon(s, control)?;
    receipt_response(s, result, JOURNAL)
}
