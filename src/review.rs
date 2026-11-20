//! Engine review validation and operation input, independent of CLI parsing.
use crate::{
    model::{Graph, Role, now},
    policy,
    store::{Change, ChangeSet, KnowledgeStore},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

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
pub(crate) fn receipt(context: &OperationContext, result: &Value) -> Result<String> {
    Ok(receipt_changes(context, result)?.to_sql())
}

pub(crate) fn receipt_payload(
    op_id: &str,
    args_hash: &str,
    actor: &str,
    role: Role,
    result: &Value,
) -> Value {
    json!({
        "op_id": op_id,
        "args_hash": args_hash,
        "actor": actor,
        "role": role,
        "result": result,
        "committed_at": now(),
    })
}

pub(crate) fn receipt_changes(context: &OperationContext, result: &Value) -> Result<ChangeSet> {
    let actor = context
        .actor
        .as_deref()
        .context("operation receipt requires an actor")?;
    let mut payload = receipt_payload(
        &context.op_id,
        &context.args_hash,
        actor,
        context.role,
        result,
    );
    payload["command"] = context.command.clone();
    Ok(ChangeSet::from(Change::put(
        "operations",
        &context.op_id,
        &payload,
    )?))
}
pub(crate) fn wrap(store: &dyn KnowledgeStore, data: Value, branch: &str) -> Result<Value> {
    Ok(wrap_at(data, branch, &store.revision()?))
}

pub(crate) fn wrap_at(data: Value, branch: &str, revision: &str) -> Value {
    json!({"schema_version":2,"ok":true,"revision":revision,"branch":branch,"data":data})
}
pub(crate) fn validate_review_candidate(
    main_graph: &Graph,
    merged: &Graph,
    requirements: &[crate::mutation::Requirement],
    routing: &Value,
    authority: (&str, Role),
    work: &dyn crate::work_ref::Adapter,
) -> Result<()> {
    let (who, role) = authority;
    for (id, source) in &merged.import_sources {
        if !main_graph.import_sources.contains_key(id)
            && (source.schema_hash != main_graph.schema.revision()
                || source.schema_hash != merged.schema.revision())
        {
            bail!("stale_plan: import schema changed; reject proposal and create a new plan");
        }
    }
    policy::validate_change(main_graph, merged, &Default::default(), work)?;
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
