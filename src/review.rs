//! Engine review validation and operation input, independent of CLI parsing.
use crate::{
    model::{Graph, Role, now},
    policy,
    store::{Change, ChangeSet, KnowledgeStore},
};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub struct OperationContext {
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

pub struct ReviewRequest {
    pub action: String,
    pub token: Option<String>,
    pub reason: Option<String>,
}

pub struct RecoveryRequest {
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
pub(crate) fn wrap(store: &dyn KnowledgeStore, data: Value, branch: &str) -> Result<Value> {
    Ok(
        json!({"schema_version":2,"ok":true,"revision":store.revision()?,"branch":branch,"data":data}),
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
