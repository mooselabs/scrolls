//! Compatibility oracle from 2af8f05; exercise every bundled endpoint pair.
use anyhow::{Result, bail};
use scrolls::model::*;
fn baseline_edge_valid(g: &Graph, e: &Edge) -> Result<()> {
    let a = g
        .records
        .get(&e.from)
        .ok_or_else(|| anyhow::anyhow!("missing source {}", e.from))?;
    let b = g.records.get(&e.to);
    if e.to.starts_with("beads://") {
        let target = scrolls::work_ref::BeadRef::parse(&e.to)?;
        if target.uri() != e.to {
            bail!("non-canonical Beads reference");
        }
        let valid = match e.rel.as_str() {
            "related_work" => true,
            "implemented_by" => matches!(a.kind.as_str(), "decision" | "knowledge" | "primer"),
            "produced_by" => matches!(
                a.kind.as_str(),
                "evidence" | "knowledge" | "primer" | "note"
            ),
            "blocks" => a.kind == "question",
            _ => false,
        };
        if !valid {
            bail!("invalid Beads relation {} from {}", e.rel, a.kind);
        }
        return Ok(());
    }
    if e.rel == "blocks" && a.kind == "question" && !e.to.is_empty() {
        return Ok(());
    }
    if e.rel == "applies_to" && a.kind == "doctrine" && e.to.starts_with("glob:") {
        scrolls::triggers::glob(&a.id, "applies_to", &e.to[5..])?;
        return Ok(());
    }
    if matches!(
        e.rel.as_str(),
        "related_work" | "implemented_by" | "produced_by"
    ) {
        bail!("work relation requires a qualified Beads reference");
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

#[test]
fn bundled_relation_policy_matches_the_verified_baseline() {
    let mut graph = Graph::default();
    for kind in KINDS {
        let mut r = Record::new(kind, kind, "", "alice").unwrap();
        r.id = kind.to_string();
        graph.records.insert(r.id.clone(), r);
    }
    let mut targets = KINDS.to_vec();
    targets.extend([
        "external-work",
        "glob:**/*.rs",
        "beads://00000000-0000-4000-8000-000000000001/bd-a",
    ]);
    for from in KINDS {
        for to in &targets {
            for rel in RELATIONS {
                for why in ["", "reason"] {
                    let edge = Edge {
                        from: from.to_string(),
                        rel: rel.to_string(),
                        to: to.to_string(),
                        why: why.to_string(),
                        actor: "alice".into(),
                        created: now(),
                    };
                    assert_eq!(
                        baseline_edge_valid(&graph, &edge).is_ok(),
                        scrolls::policy::edge_valid(&graph, &edge).is_ok(),
                        "{from} --{rel}--> {to}, why={why}"
                    );
                }
            }
        }
    }
}
