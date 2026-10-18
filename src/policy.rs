use crate::model::*;
use crate::schema::neighbor;
use anyhow::{Context, Result, bail, ensure};
use std::collections::BTreeSet;
pub fn edge_valid(g: &Graph, e: &Edge) -> Result<()> {
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
        let ty = if e.to.starts_with("beads://") {
            ensure!(
                crate::work_ref::BeadRef::parse(&e.to)?.uri() == e.to,
                "non-canonical Beads reference"
            );
            "beads"
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
pub fn lint(g: &Graph) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let re = regex::Regex::new(r"\]\((sc-[a-zA-Z0-9-]+)(?:#[^)]*)?\)").unwrap();
    for r in g.records.values() {
        if let Err(e) = g.schema.validate_record(r) {
            out.insert(format!("{e:#}"));
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
        if let Err(err) = edge_valid(g, e) {
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
pub fn validate_change(before: &Graph, after: &Graph, _debt: &BTreeSet<String>) -> Result<()> {
    crate::source_import::ownership(before, after)?;
    // Frozen rows and assertion owners remain immutable, including at review.
    for (id, old) in &before.records {
        if old.frozen {
            ensure!(
                after.records.get(id) == Some(old),
                "{id} is immutable imported compatibility history"
            );
        }
    }
    for (key, edge) in before.edges.iter().chain(after.edges.iter()) {
        if before.edges.get(key) == after.edges.get(key) {
            continue;
        }
        let source = before.records.get(&edge.from);
        ensure!(
            !source.is_some_and(|r| r.frozen),
            "{} is immutable imported compatibility history",
            edge.from
        );
        let both = before
            .schema
            .relations
            .get(&edge.rel)
            .or_else(|| after.schema.relations.get(&edge.rel))
            .is_some_and(|r| r.affects == crate::schema::Affects::Both);
        ensure!(
            !both || !before.records.get(&edge.to).is_some_and(|r| r.frozen),
            "{} is immutable imported compatibility history",
            edge.to
        );
    }
    // Historical debt can remain unchanged, but edits cannot create fresh field violations.
    for (id, r) in &after.records {
        if before.records.get(id) != Some(r) {
            let old = before.records.get(id);
            if let Some(kind) = after.schema.kinds.get(&r.kind) {
                for (field, rule) in &kind.fields {
                    if rule.glob {
                        if let Some(value) = r.fields.get(field) {
                            rule.validate(value)
                                .with_context(|| format!("{id}:field:{field}"))?;
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
                after.schema.validate_record(r)?;
            }
        }
    }
    for r in after.records.values() {
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
                if n < c.min || c.max.is_some_and(|m| n > m) {
                    let same_rule = before
                        .schema
                        .kinds
                        .get(&r.kind)
                        .is_some_and(|k| k.cardinality.contains(c));
                    ensure!(
                        same_rule && count(before) == n,
                        "{}:{}: relationship count {n} violates min {} max {:?}",
                        r.id,
                        c.name,
                        c.min,
                        c.max
                    );
                }
            }
        }
    }
    let baseline = lint(before);
    let errors = lint(after);
    let new: Vec<_> = errors.difference(&baseline).collect();
    if !new.is_empty() {
        bail!(
            "structural validation failed: {}",
            new.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(())
}
pub fn extract_links(g: &mut Graph, id: &str, actor: &str) {
    if g.import_members.contains_key(id) {
        return;
    }
    let re = regex::Regex::new(r"\]\((sc-[a-zA-Z0-9-]+)(?:#[^)]*)?\)").unwrap();
    let body = g.records[id].body.clone();
    let beads = regex::Regex::new(r"\]\((beads://[^)\s]+)\)").unwrap();
    for cap in beads.captures_iter(&body) {
        // Keep malformed links as edges so structural validation reports them.
        let target = crate::work_ref::BeadRef::parse(&cap[1])
            .map(|r| r.uri())
            .unwrap_or_else(|_| cap[1].into());
        if g.schema.body_links.contains_key("beads")
            && !g.edges.values().any(|e| e.from == id && e.to == target)
        {
            let rel = g.schema.body_links["beads"].clone();
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
