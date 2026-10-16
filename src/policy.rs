use crate::model::*;
use anyhow::{Result, bail};
use std::collections::BTreeSet;

pub fn edge_valid(g: &Graph, e: &Edge) -> Result<()> {
    let a = g
        .records
        .get(&e.from)
        .ok_or_else(|| anyhow::anyhow!("missing source {}", e.from))?;
    let b = g.records.get(&e.to);
    if e.rel == "blocks" && a.kind == "question" && !e.to.is_empty() {
        return Ok(());
    }
    if e.rel == "applies_to" && a.kind == "doctrine" && e.to.starts_with("glob:") {
        globset::Glob::new(&e.to[5..])?;
        return Ok(());
    }
    let b = b.ok_or_else(|| anyhow::anyhow!("missing target {}", e.to))?;
    let ak = if a.kind == "primer" {
        "knowledge"
    } else {
        &a.kind
    };
    let bk = if b.kind == "primer" {
        "knowledge"
    } else {
        &b.kind
    };
    let valid = match e.rel.as_str() {
        "supports" => ak == "evidence" && matches!(bk, "knowledge" | "decision"),
        "derived_from" => ak == "knowledge" && matches!(bk, "knowledge" | "evidence"),
        "supersedes" => a.kind == b.kind && a.id != b.id,
        "contradicts" => {
            matches!(ak, "knowledge" | "decision") && matches!(bk, "knowledge" | "decision")
        }
        "about" => bk == "entity",
        "applies_to" => ak == "doctrine" && bk == "entity",
        "asks_about" => ak == "question",
        "answers" => ak == "answer" && bk == "question",
        "promoted_to" => {
            matches!(ak, "answer" | "evidence") && matches!(bk, "decision" | "knowledge")
        }
        "depends_on" => ak == "decision" && bk == "decision",
        "related" => true,
        _ => false,
    };
    if !valid {
        bail!("invalid {} relation {} -> {}", e.rel, a.kind, b.kind);
    }
    if matches!(e.rel.as_str(), "supersedes" | "depends_on" | "contradicts")
        && e.why.trim().is_empty()
    {
        bail!("{} requires --why", e.rel);
    }
    Ok(())
}
pub fn lint(g: &Graph) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let supported: BTreeSet<_> = g
        .edges
        .values()
        .filter(|e| e.rel == "supports")
        .map(|e| e.to.as_str())
        .collect();
    let re = regex::Regex::new(r"\]\((sc-[a-zA-Z0-9-]+)(?:#[^)]*)?\)").unwrap();
    for r in g.records.values() {
        if !lifecycle(&r.kind).is_ok_and(|l| l.contains(&r.status.as_str())) {
            out.insert(format!("{}:invalid-status", r.id));
        }
        if r.owner.trim().is_empty() || r.title.trim().is_empty() || r.review_every == 0 {
            out.insert(format!("{}:required-fields", r.id));
        }
        if !["high", "medium", "low"].contains(&r.confidence.as_str()) {
            out.insert(format!("{}:confidence", r.id));
        }
        if matches!(r.kind.as_str(), "knowledge" | "primer" | "decision")
            && !supported.contains(r.id.as_str())
        {
            out.insert(format!("{}:missing-support", r.id));
        }
        if r.kind == "answer"
            && g.edges
                .values()
                .filter(|e| e.rel == "answers" && e.from == r.id)
                .count()
                != 1
        {
            out.insert(format!("{}:answer-question", r.id));
        }
        if r.kind == "question" && r.status == "answered" && !r.frozen {
            let n = g
                .edges
                .values()
                .filter(|e| {
                    e.rel == "answers"
                        && e.to == r.id
                        && g.records
                            .get(&e.from)
                            .is_some_and(|a| a.status == "accepted")
                })
                .count();
            if n != 1 {
                out.insert(format!("{}:accepted-answer", r.id));
            }
        }
        for c in re.captures_iter(&r.body) {
            if !g.records.contains_key(&c[1]) {
                out.insert(format!("{}:body-link:{}", r.id, &c[1]));
            }
        }
    }
    for (key, e) in &g.edges {
        if let Err(err) = edge_valid(g, e) {
            out.insert(format!("edge:{key}:{err}"));
        }
    }
    for rel in ["depends_on", "supersedes"] {
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
        for id in g.edges.values().filter(|e| e.rel == rel).map(|e| &e.from) {
            if visit(id, rel, g, &mut BTreeSet::new(), &mut done) {
                out.insert(format!("{id}:{rel}-cycle"));
            }
        }
    }
    out
}
pub fn validate_change(before: &Graph, after: &Graph, _debt: &BTreeSet<String>) -> Result<()> {
    let errors = lint(after);
    let baseline = lint(before);
    let new: Vec<_> = errors.difference(&baseline).cloned().collect();
    if !new.is_empty() {
        bail!("structural validation failed: {}", new.join("; "));
    }
    Ok(())
}
pub fn extract_links(g: &mut Graph, id: &str, actor: &str) {
    let re = regex::Regex::new(r"\]\((sc-[a-zA-Z0-9-]+)(?:#[^)]*)?\)").unwrap();
    let body = g.records[id].body.clone();
    for cap in re.captures_iter(&body) {
        let target = &cap[1];
        if g.records.contains_key(target)
            && !g
                .edges
                .values()
                .any(|e| (e.from == id && e.to == target) || (e.from == target && e.to == id))
        {
            g.add_edge(id, "related", target, "body link", actor);
        }
    }
}
