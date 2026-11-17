use crate::model::*;
use crate::schema::neighbor;
use anyhow::{Context, Result, ensure};
use std::collections::BTreeSet;
pub fn edge_valid(g: &Graph, e: &Edge, work: &dyn crate::work_ref::Adapter) -> Result<()> {
    let rule = g
        .schema
        .relations
        .get(&e.rel)
        .with_context(|| format!("unknown relation {}", e.rel))?;
    let a = g
        .records
        .get(&e.from)
        .with_context(|| format!("missing source {}", e.from))?;
    ensure!(
        rule.from.iter().any(|k| k == "*" || k == &a.kind),
        "invalid {} source {}",
        e.rel,
        a.kind
    );
    ensure!(
        !rule.reason || !e.why.trim().is_empty(),
        "{} requires --why",
        e.rel
    );
    ensure!(
        rule.self_link || e.from != e.to,
        "{} forbids self links",
        e.rel
    );
    if let Some(b) = g.records.get(&e.to) {
        ensure!(
            rule.to.iter().any(|k| k == "*" || k == &b.kind),
            "invalid {} target {}",
            e.rel,
            b.kind
        );
        ensure!(
            !rule.same_kind || a.kind == b.kind,
            "{} requires same kind",
            e.rel
        );
    } else {
        let ty = if let Some(canonical) = work.canonicalize(&e.to) {
            ensure!(canonical? == e.to, "non-canonical work reference");
            g.schema.work_target()
        } else if let Some(glob) = e.to.strip_prefix("glob:") {
            crate::triggers::glob(&e.from, &e.rel, glob)?;
            "glob"
        } else if e.to.starts_with("https://") || e.to.starts_with("http://") {
            "url"
        } else {
            "text"
        };
        ensure!(
            !e.to.trim().is_empty() && rule.external.iter().any(|t| t == ty),
            "missing or invalid target {} for {}",
            e.to,
            e.rel
        );
    }
    Ok(())
}
pub fn lint(g: &Graph, work: &dyn crate::work_ref::Adapter) -> BTreeSet<String> {
    lint_graph(g, work, false)
}
fn lint_graph(
    g: &Graph,
    work: &dyn crate::work_ref::Adapter,
    all_fields: bool,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let re = regex::Regex::new(r"\]\((sc-[a-zA-Z0-9-]+)(?:#[^)]*)?\)").unwrap();
    for r in g.records.values() {
        let errors = if all_fields {
            g.schema.record_errors(r)
        } else {
            g.schema.validate_record(r).err().into_iter().collect()
        };
        for error in errors {
            let text = format!("{error:#}");
            out.insert(if all_fields {
                record_violation(&r.id, text)
            } else {
                text
            });
        }
        if let Some(k) = g.schema.kinds.get(&r.kind) {
            for c in &k.cardinality {
                if (c.skip_frozen && r.frozen) || c.status.as_ref().is_some_and(|s| s != &r.status)
                {
                    continue;
                }
                let n = g
                    .edges
                    .values()
                    .filter(|e| e.rel == c.relation)
                    .filter_map(|e| neighbor(e, &r.id, c.direction))
                    .filter(|id| {
                        c.related_status
                            .as_ref()
                            .is_none_or(|s| g.records.get(*id).is_some_and(|r| &r.status == s))
                    })
                    .count();
                if n < c.min || c.max.is_some_and(|m| n > m) {
                    out.insert(format!("{}:{}", r.id, c.name));
                }
            }
        }
        for invariant in &g.schema.invariants {
            if invariant.violated(g, r) {
                out.insert(format!("{}:{}", r.id, invariant.name));
            }
        }
        for cap in re
            .captures_iter(&r.body)
            .filter(|_| !g.import_members.contains_key(&r.id))
        {
            if !g.records.contains_key(&cap[1]) {
                out.insert(format!("{}:body-link:{}", r.id, &cap[1]));
            }
        }
    }
    for (key, e) in &g.edges {
        if let Err(err) = edge_valid(g, e, work) {
            out.insert(format!("edge:{key}:{err}"));
        }
    }
    for (rel, rule) in &g.schema.relations {
        if rule.cycles {
            continue;
        }
        fn visit(
            id: &str,
            rel: &str,
            g: &Graph,
            active: &mut BTreeSet<String>,
            done: &mut BTreeSet<String>,
        ) -> bool {
            if active.contains(id) {
                return true;
            }
            if done.contains(id) {
                return false;
            }
            active.insert(id.into());
            for e in g.edges.values().filter(|e| e.from == id && e.rel == rel) {
                if visit(&e.to, rel, g, active, done) {
                    return true;
                }
            }
            active.remove(id);
            done.insert(id.into());
            false
        }
        let mut done = BTreeSet::new();
        for id in g.edges.values().filter(|e| &e.rel == rel).map(|e| &e.from) {
            if visit(id, rel, g, &mut BTreeSet::new(), &mut done) {
                out.insert(format!("{id}:{rel}-cycle"));
            }
        }
    }
    out
}
fn record_violation(id: &str, text: String) -> String {
    if text.starts_with(&format!("{id}:")) {
        text
    } else {
        format!("{id}:record:{text}")
    }
}

/// Stable refusal payload shared by all engine write paths and CLI diagnostics.
#[derive(Debug)]
pub struct WriteRefusal {
    pub violations: BTreeSet<String>,
}
impl std::fmt::Display for WriteRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "structural validation failed: {}",
            self.violations
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        )
    }
}
impl std::error::Error for WriteRefusal {}

// The identity is the lint identity, while the definition excludes unrelated
// authority, presentation and lifecycle settings on the same kind.
fn same_rule(before: &Graph, after: &Graph, violation: &str) -> bool {
    if let Some(edge) = violation.strip_prefix("edge:") {
        return before
            .edges
            .iter()
            .find(|(key, _)| edge.starts_with(&format!("{key}:")))
            .is_some_and(|(_, e)| {
                before.schema.relations.get(&e.rel) == after.schema.relations.get(&e.rel)
            });
    }
    let Some((id, rule)) = violation.split_once(':') else {
        return false;
    };
    let (Some(old), Some(new)) = (before.records.get(id), after.records.get(id)) else {
        return false;
    };
    if old.kind != new.kind {
        return false;
    }
    let old_invariant = before.schema.invariants.iter().find(|i| i.name == rule);
    let new_invariant = after.schema.invariants.iter().find(|i| i.name == rule);
    if (old_invariant.is_some() || new_invariant.is_some())
        && (old_invariant.is_none() || old_invariant != new_invariant)
    {
        return false;
    }
    if rule.starts_with("body-link:") {
        return true;
    }
    if rule.starts_with("record:") && !after.schema.kinds.contains_key(&new.kind) {
        return !before.schema.kinds.contains_key(&old.kind);
    }
    if let Some(rel) = rule.strip_suffix("-cycle") {
        return before.schema.relations.get(rel) == after.schema.relations.get(rel);
    }
    let (Some(a), Some(b)) = (
        before.schema.kinds.get(&old.kind),
        after.schema.kinds.get(&new.kind),
    ) else {
        return false;
    };
    if let Some(c) = b.cardinality.iter().find(|c| c.name == rule) {
        return a.cardinality.contains(c);
    }
    if rule == "invalid-status" {
        return a.states == b.states;
    }
    if rule == "required-fields" {
        return true;
    }
    if let Some(field) = rule
        .strip_prefix("field:")
        .or_else(|| rule.strip_prefix("required-field:"))
    {
        let field = field.split(':').next().unwrap();
        return a.fields.get(field) == b.fields.get(field);
    }
    if rule.starts_with("unknown-field:") {
        return a.additional_fields == b.additional_fields && a.fields.keys().eq(b.fields.keys());
    }
    before
        .schema
        .envelope_fields
        .get(rule.split(':').next().unwrap())
        == after
            .schema
            .envelope_fields
            .get(rule.split(':').next().unwrap())
}

/// Preserve the legacy persisted lint-debt identities while tolerating the
/// complete imported field baseline. Migration and the public lint output retain
/// their original first-field-error behavior; write refusals report every field.
pub(crate) fn validate_bootstrap(
    before: &Graph,
    after: &Graph,
    recorded_debt: &BTreeSet<String>,
    work: &dyn crate::work_ref::Adapter,
) -> Result<()> {
    ensure!(
        lint(after, work) == *recorded_debt,
        "bootstrap lint debt differs from imported graph"
    );
    validate_change(before, after, &lint_graph(after, work, true), work)
}

/// Backend-neutral full-graph write check. Only a bootstrap store write supplies
/// `debt`; ordinary writes tolerate violations of unchanged rules on the same rows.
pub fn validate_change(
    before: &Graph,
    after: &Graph,
    debt: &BTreeSet<String>,
    work: &dyn crate::work_ref::Adapter,
) -> Result<()> {
    crate::source_import::ownership(before, after)?;
    let mut violations = BTreeSet::new();
    if let Err(error) = after.schema.validate() {
        violations.insert(format!("ontology:schema:{error:#}"));
    }
    for pin in &after.schema.pins {
        if after.resolve(pin).is_err() {
            violations.insert(format!("ontology:pin:{pin}"));
        }
    }
    for (id, old) in &before.records {
        if old.frozen && after.records.get(id) != Some(old) {
            violations.insert(format!("{id}:immutable imported compatibility history"));
        }
    }
    for (key, edge) in before.edges.iter().chain(after.edges.iter()) {
        if before.edges.get(key) == after.edges.get(key) {
            continue;
        }
        let both = before
            .schema
            .relations
            .get(&edge.rel)
            .or_else(|| after.schema.relations.get(&edge.rel))
            .is_some_and(|r| r.affects == crate::schema::Affects::Both);
        for id in std::iter::once(&edge.from).chain(both.then_some(&edge.to)) {
            if before.records.get(id).is_some_and(|r| r.frozen) {
                violations.insert(format!("{id}:immutable imported compatibility history"));
            }
        }
    }
    // Preserve the stricter existing check on authored field edits.
    for (id, r) in &after.records {
        if before.records.get(id) != Some(r) {
            let old = before.records.get(id);
            if let Some(kind) = after.schema.kinds.get(&r.kind) {
                for (field, rule) in &kind.fields {
                    if let Some(error) = r
                        .fields
                        .get(field)
                        .filter(|_| rule.glob)
                        .and_then(|value| rule.validate(value).err())
                    {
                        let identity = format!("{id}:field:{field}: {error:#}");
                        if !debt.contains(&identity) {
                            violations.insert(identity);
                        }
                    }
                }
            }
            if old.is_none_or(|old| {
                old.fields != r.fields
                    || old.kind != r.kind
                    || old.title != r.title
                    || old.owner != r.owner
                    || old.confidence != r.confidence
                    || old.review_every != r.review_every
                    || old.last_reviewed != r.last_reviewed
            }) {
                for error in after.schema.record_errors(r) {
                    let identity = record_violation(id, format!("{error:#}"));
                    if !debt.contains(&identity) {
                        violations.insert(identity);
                    }
                }
            }
        }
        if let Some(kind) = after.schema.kinds.get(&r.kind) {
            for c in &kind.cardinality {
                if (c.skip_frozen && r.frozen) || c.status.as_ref().is_some_and(|s| s != &r.status)
                {
                    continue;
                }
                let count = |g: &Graph| {
                    g.edges
                        .values()
                        .filter(|e| e.rel == c.relation)
                        .filter_map(|e| neighbor(e, &r.id, c.direction))
                        .filter(|id| {
                            c.related_status
                                .as_ref()
                                .is_none_or(|s| g.records.get(*id).is_some_and(|r| &r.status == s))
                        })
                        .count()
                };
                let n = count(after);
                let identity = format!("{}:{}", r.id, c.name);
                if (n < c.min || c.max.is_some_and(|m| n > m))
                    && !debt.contains(&identity)
                    && !(before.records.contains_key(&r.id)
                        && same_rule(before, after, &identity)
                        && count(before) == n)
                {
                    violations.insert(identity);
                }
            }
        }
    }
    let baseline = lint_graph(before, work, true);
    for violation in lint_graph(after, work, true) {
        if !debt.contains(&violation)
            && !(baseline.contains(&violation) && same_rule(before, after, &violation))
        {
            violations.insert(violation);
        }
    }
    if !violations.is_empty() {
        return Err(WriteRefusal { violations }.into());
    }
    Ok(())
}
pub fn extract_links(g: &mut Graph, id: &str, actor: &str, work: &dyn crate::work_ref::Adapter) {
    if g.import_members.contains_key(id) {
        return;
    }
    let re = regex::Regex::new(r"\]\((sc-[a-zA-Z0-9-]+)(?:#[^)]*)?\)").unwrap();
    let body = g.records[id].body.clone();
    for target in work.body_references(&body) {
        if g.schema.body_links.contains_key(g.schema.work_target())
            && !g.edges.values().any(|e| e.from == id && e.to == target)
        {
            let rel = g.schema.body_links[g.schema.work_target()].clone();
            g.add_edge(id, &rel, &target, "body link", actor);
        }
    }
    for cap in re.captures_iter(&body) {
        let target = &cap[1];
        if g.schema.body_links.contains_key("local")
            && g.records.contains_key(target)
            && !g
                .edges
                .values()
                .any(|e| (e.from == id && e.to == target) || (e.from == target && e.to == id))
        {
            let rel = g.schema.body_links["local"].clone();
            g.add_edge(id, &rel, target, "body link", actor);
        }
    }
}

pub fn lint_routing(routing: &serde_json::Value) -> BTreeSet<String> {
    let mut errors = BTreeSet::new();
    match crate::triggers::routing_entries(routing) {
        Err(err) => {
            errors.insert(err.to_string());
        }
        Ok(entries) => {
            for entry in entries {
                if let Err(err) = crate::triggers::imported(entry) {
                    errors.insert(format!("{err:#}"));
                }
            }
        }
    }
    errors
}
