//! Engine review validation and operation input, independent of CLI parsing.
use crate::{
    model::{Graph, ImportInfo, Role, now},
    policy,
    storage::Store,
    store::{Change, ChangeSet},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(crate) struct OperationContext {
    pub op_id: String,
    pub args_hash: String,
    // Recovery distinguishes caller-supplied IDs from resolved IDs.
    pub explicit_op_id: bool,
    pub actor: Option<String>,
    pub role: Role,
    pub dry_run: bool,
    // Persist the original command JSON, never the normalized fingerprint.
    pub command: Value,
    pub review: Option<ReviewRequest>,
    pub recovery: Option<RecoveryRequest>,
}

pub(crate) struct ReviewRequest {
    pub action: String,
    pub token: Option<String>,
    pub reason: Option<String>,
}

pub(crate) struct RecoveryRequest {
    pub operation: String,
    pub abandon: bool,
    pub resume: bool,
    pub reason: Option<String>,
}

// BKLG-20261108-enit: storage's shared review protocol still uses the String API.
pub(crate) fn receipt(
    op: &str,
    hash: &str,
    actor: &str,
    command: &Value,
    result: &Value,
) -> Result<String> {
    Ok(receipt_changes(op, hash, actor, command, result)?.to_sql())
}

pub(crate) fn receipt_changes(
    op: &str,
    hash: &str,
    actor: &str,
    command: &Value,
    result: &Value,
) -> Result<ChangeSet> {
    Ok(ChangeSet::from(Change::put(
        "operations",
        op,
        &json!({"op_id":op,"args_hash":hash,"actor":actor,"command":command,"result":result,"committed_at":now()}),
    )?))
}
pub(crate) fn wrap(store: &Store, data: Value, branch: &str) -> Result<Value> {
    Ok(
        json!({"schema_version":2,"ok":true,"revision":store.revision()?,"branch":branch,"data":data}),
    )
}
pub(crate) fn validate_merged_review(
    s: &Store,
    main_graph: &Graph,
    who: &str,
    role: Role,
) -> Result<()> {
    let merged = s.graph()?;
    crate::markdown_import::health(s, &merged, None)?;
    crate::source_import::health(&merged, &s.import_files()?)?;
    let debt = s
        .one::<ImportInfo>("metadata", "import")?
        .map(|i| i.lint_debt)
        .unwrap_or_default();
    let mut requirements = vec![];
    for row in s.merged_review_guards()? {
        requirements.extend(serde_json::from_str::<Vec<crate::mutation::Requirement>>(
            row["payload"].as_str().context("guard payload")?,
        )?);
    }
    let routing = s.one::<Value>("metadata", "routing")?.unwrap_or(json!({}));
    validate_review_candidate(
        main_graph,
        &merged,
        &debt,
        &requirements,
        &routing,
        who,
        role,
    )
}

pub(crate) fn validate_review_candidate(
    main_graph: &Graph,
    merged: &Graph,
    debt: &BTreeSet<String>,
    requirements: &[crate::mutation::Requirement],
    routing: &Value,
    who: &str,
    role: Role,
) -> Result<()> {
    for (id, source) in &merged.import_sources {
        if !main_graph.import_sources.contains_key(id)
            && (source.schema_hash != main_graph.schema.revision()
                || source.schema_hash != merged.schema.revision())
        {
            bail!("stale_plan: import schema changed; reject proposal and create a new plan");
        }
    }
    policy::validate_change(main_graph, merged, debt)?;
    merged.schema.validate()?;
    for pin in &merged.schema.pins {
        merged.resolve(pin)?;
    }
    main_graph
        .schema
        .guards(main_graph, merged, who, role.name())?;
    merged.schema.guards(main_graph, merged, who, role.name())?;
    for req in requirements {
        req.check(who, role.name())?;
    }
    let errors = policy::lint_routing(routing);
    if !errors.is_empty() {
        bail!(
            "invalid routing triggers: {}",
            errors.into_iter().collect::<Vec<_>>().join("; ")
        );
    }
    Ok(())
}
