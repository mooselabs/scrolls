//! Read and move record labels using versioned rows in the existing registry.
use crate::{
    model::digest,
    review::receipt_changes,
    store::{Change, ChangeSet, KnowledgeStore, OperationContext},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const TABLE: &str = "registry";

fn record_prefix(id: &str) -> String {
    // A digest leaves room for the label name even for a maximum-length record ID.
    format!("label:{}:", digest(id))
}

fn row_key(id: &str, name: &str) -> Result<String> {
    let mut bytes = name.bytes();
    ensure!(
        name.len() <= 32
            && bytes.next().is_some_and(|b| b.is_ascii_lowercase())
            && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "invalid label {name:?}: use 1–32 ASCII characters, starting with a lowercase letter, then lowercase letters, digits or hyphens"
    );
    Ok(format!("{}{name}", record_prefix(id)))
}

fn target(store: &dyn KnowledgeStore, key: &str, at: &str) -> Result<Option<String>> {
    store
        .one(TABLE, key, Some(at))?
        .map(|payload| Ok(serde_json::from_str(&payload)?))
        .transpose()
}

pub(crate) fn resolve(
    store: &dyn KnowledgeStore,
    id: &str,
    name: &str,
    at: &str,
) -> Result<String> {
    target(store, &row_key(id, name)?, at)?
        .with_context(|| format!("label_not_found: record {id} has no label {name:?}"))
}

pub(crate) fn list(
    store: &dyn KnowledgeStore,
    id: &str,
    at: &str,
) -> Result<BTreeMap<String, String>> {
    let prefix = record_prefix(id);
    store
        .rows(TABLE, Some(at))?
        .into_iter()
        .filter_map(|(key, payload)| {
            key.strip_prefix(&prefix)
                .map(|name| Ok((name.into(), serde_json::from_str(&payload)?)))
        })
        .collect()
}

/// The caller holds the store's exclusive lock with main selected and has
/// authorized the role and checked for an existing operation receipt.
pub(crate) fn move_to(
    store: &dyn KnowledgeStore,
    id: &str,
    name: &str,
    new_target: &str,
    expected: Option<&str>,
    context: &OperationContext,
) -> Result<Value> {
    let key = row_key(id, name)?;
    let main = store.revision()?;
    let previous = target(store, &key, &main)?;
    ensure!(
        previous.as_deref() == expected,
        "stale_label: record {id} label {name:?} expected {expected:?}, current target is {previous:?}"
    );
    let target_error =
        || format!("label_target: {new_target:?} must be an exact revision on main history");
    // Resolve first so only a canonical, immutable revision reaches ancestry checks.
    ensure!(
        store.head(new_target).with_context(target_error)? == new_target
            && store
                .base_revision(new_target, &main)
                .with_context(target_error)?
                == new_target,
        "{}",
        target_error()
    );
    ensure!(
        store.one("records", id, Some(new_target))?.is_some(),
        "label_target: record {id} does not exist at revision {new_target}"
    );
    let actor = context
        .actor
        .as_deref()
        .context("label move requires an actor")?;
    let result = json!({
        "op_id":context.op_id, "actor":actor, "role":context.role.name(),
        "id":id, "label":name, "previous_target":previous, "new_target":new_target
    });
    if context.dry_run {
        return Ok(json!({"dry_run":true,"result":result}));
    }
    let mut changes = ChangeSet::from(Change::put(TABLE, &key, &new_target)?);
    changes.extend(receipt_changes(context, &result)?);
    store.commit(&changes, &format!("move label {name} on {id}"), actor)?;
    Ok(result)
}
