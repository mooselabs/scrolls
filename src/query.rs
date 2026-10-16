use crate::model::*;
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};
pub fn walk(g: &Graph, start: &str, rel: Option<&str>, depth: usize) -> Vec<Record> {
    let mut seen = BTreeSet::from([start.to_string()]);
    let mut queue = VecDeque::from([(start.to_string(), 0)]);
    while let Some((id, d)) = queue.pop_front() {
        if d >= depth {
            continue;
        }
        for e in g.edges.values().filter(|e| rel.is_none_or(|r| e.rel == r)) {
            let next = if e.from == id {
                &e.to
            } else if e.to == id {
                &e.from
            } else {
                continue;
            };
            if g.records.contains_key(next) && seen.insert(next.clone()) {
                queue.push_back((next.clone(), d + 1));
            }
        }
    }
    seen.into_iter()
        .filter_map(|id| g.records.get(&id).cloned())
        .collect()
}
pub fn context(g: &Graph, id: &str, budget: usize) -> Result<Value> {
    let mut full = BTreeSet::from([id.to_string()]);
    let mut titles = BTreeSet::new();
    for e in g.edges.values() {
        if e.to == id {
            if let Some(r) = g.records.get(&e.from) {
                let chosen = match e.rel.as_str() {
                    "about" => {
                        (r.kind == "decision"
                            && r.status == "accepted"
                            && !g
                                .edges
                                .values()
                                .any(|x| x.rel == "supersedes" && x.to == r.id))
                            || (matches!(r.kind.as_str(), "knowledge" | "primer")
                                && r.status == "current")
                    }
                    "asks_about" => r.kind == "question" && r.status == "open",
                    "applies_to" => r.kind == "doctrine" && r.status == "active",
                    _ => false,
                };
                if chosen {
                    full.insert(r.id.clone());
                }
            }
        }
        if e.rel == "related" {
            let next = if e.from == id {
                Some(&e.to)
            } else if e.to == id {
                Some(&e.from)
            } else {
                None
            };
            if let Some(next) = next {
                if g.records.get(next).is_some_and(|r| r.kind == "entity") {
                    titles.insert(next.clone());
                }
            }
        }
    }
    for e in g
        .edges
        .values()
        .filter(|e| e.rel == "supports" && full.contains(&e.to))
    {
        titles.insert(e.from.clone());
    }
    packet(
        g,
        full.into_iter().collect(),
        titles.into_iter().collect(),
        budget,
    )
}
/// Context projections intentionally omit the duplicate import snapshot.
pub fn projection(r: &Record) -> Result<Value> {
    let mut value = serde_json::to_value(r)?;
    value.as_object_mut().unwrap().remove("original");
    Ok(value)
}
fn budget_packet(mandatory: Vec<Value>, groups: Vec<Vec<Value>>, budget: usize) -> Result<Value> {
    anyhow::ensure!(
        budget >= 2,
        "--budget must be at least 2 bytes for a JSON array"
    );
    let mandatory_bytes = if mandatory.is_empty() {
        0
    } else {
        serde_json::to_vec(&mandatory)?.len()
    };
    let mut items = mandatory;
    let mut used = serde_json::to_vec(&items)?.len();
    let mut omitted = vec![];
    for group in groups {
        let size = serde_json::to_vec(&group)?.len() - 2 + usize::from(!items.is_empty());
        if used + size <= budget {
            used += size;
            items.extend(group);
        } else {
            omitted.extend(group.iter().map(|v| v["id"].clone()));
        }
    }
    Ok(
        json!({"items":items,"omitted":omitted,"budget_bytes":budget,"used_bytes":used,
        "mandatory_bytes":mandatory_bytes,"over_budget_bytes":used.saturating_sub(budget),
        "budget_scope":"compact JSON items array; envelope and omission IDs excluded"}),
    )
}
pub fn packet(g: &Graph, full: Vec<String>, titles: Vec<String>, budget: usize) -> Result<Value> {
    let mut groups = vec![];
    let mut seen = BTreeSet::new();
    for (id, summary) in full
        .into_iter()
        .map(|id| (id, false))
        .chain(titles.into_iter().map(|id| (id, true)))
    {
        let Some(r) = g.records.get(&id) else {
            continue;
        };
        if r.kind == "note" || !seen.insert(id.clone()) {
            continue;
        }
        groups.push(vec![if summary {
            json!({"id":id,"title":r.title,"kind":r.kind,"status":r.status,"revision":r.revision,"summary_only":true})
        } else {
            projection(r)?
        }]);
    }
    budget_packet(vec![], groups, budget)
}
pub fn prime(
    g: &Graph,
    routing: &Value,
    paths: &[String],
    who: &str,
    budget: usize,
) -> Result<Value> {
    let doctrine = doctrine(g, routing, paths)?;
    let mandatory = doctrine["matched"].as_array().unwrap().clone();
    let mut groups = vec![];
    for r in g.records.values().filter(|r| {
        (r.kind == "primer" && r.status == "current")
            || (r.kind == "question"
                && !r.frozen
                && ((r.owner == who && r.status == "open")
                    || (r.fields["asked_by"] == who && r.status == "answered")))
    }) {
        let mut question = projection(r)?;
        let mut group = vec![];
        if r.kind == "question" && r.status == "answered" {
            let answers: Vec<_> = g
                .edges
                .values()
                .filter(|e| e.rel == "answers" && e.to == r.id)
                .filter_map(|e| g.records.get(&e.from))
                .filter(|a| a.kind == "answer" && a.status == "accepted")
                .collect();
            anyhow::ensure!(
                answers.len() == 1,
                "{}: answered local question requires exactly one accepted answer; found {}",
                r.id,
                answers.len()
            );
            question["accepted_answer_id"] = json!(answers[0].id);
            let mut answer = projection(answers[0])?;
            answer["question_id"] = json!(r.id);
            group.push(question);
            group.push(answer);
        } else {
            group.push(question);
        }
        groups.push(group);
    }
    let mut packet = budget_packet(mandatory, groups, budget)?;
    packet["undecided_doctrine"] = doctrine["undecided"].clone();
    Ok(packet)
}
pub fn doctrine(g: &Graph, routing: &Value, paths: &[String]) -> Result<Value> {
    let mut matched = BTreeSet::new();
    let mut undecided = BTreeSet::new();
    for t in crate::triggers::routing_entries(routing)? {
        let id = t["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("routing trigger requires record id"))?;
        if !g.records.get(id).is_some_and(|r| r.status == "active") {
            continue;
        }
        let parsed = crate::triggers::imported(t)?;
        if parsed.matches(paths) {
            matched.insert(id.to_string());
        }
        if let Some(scope) = parsed.scope {
            undecided.insert((id.to_string(), scope));
        }
    }
    for e in g.edges.values().filter(|e| e.rel == "applies_to") {
        if let Some(pattern) = e.to.strip_prefix("glob:") {
            if g.records.get(&e.from).is_some_and(|r| r.status == "active") {
                let glob = crate::triggers::glob(&e.from, "applies_to", pattern)?;
                if paths.iter().any(|p| glob.is_match(p)) {
                    matched.insert(e.from.clone());
                }
            }
        }
    }
    for r in g
        .records
        .values()
        .filter(|r| r.kind == "doctrine" && r.status == "active")
    {
        let parsed = crate::triggers::parse(&r.id, &r.fields, "glob")?;
        if parsed.matches(paths) {
            matched.insert(r.id.clone());
        }
        if let Some(scope) = parsed.scope {
            undecided.insert((r.id.clone(), scope));
        }
    }
    let matched = matched
        .into_iter()
        .map(|id| projection(&g.records[&id]))
        .collect::<Result<Vec<_>>>()?;
    let undecided: Vec<_> = undecided.into_iter().map(|(id, scope)| json!({"id":id,"scope_clause":scope,"reason":"scope clauses need human judgement; no model is called"})).collect();
    Ok(json!({"matched":matched,"undecided":undecided}))
}

/// A bead's accepted/current context, with historical links explicitly summarized.
pub fn bead_context(g: &Graph, reference: &str, budget: usize) -> Result<Value> {
    let linked: BTreeSet<_> = g
        .edges
        .values()
        .filter(|e| e.to == reference && g.records.get(&e.from).is_some_and(|r| r.kind != "note"))
        .map(|e| e.from.clone())
        .collect();
    let mut candidates = linked.clone();
    for id in &linked {
        // Include questions about the linked document itself as well as its entities.
        for item in context(g, id, usize::MAX)?["items"].as_array().unwrap() {
            if let Some(id) = item["id"].as_str() {
                candidates.insert(id.into());
            }
        }
        for e in g
            .edges
            .values()
            .filter(|e| e.from == *id && matches!(e.rel.as_str(), "about" | "applies_to"))
        {
            if g.records.get(&e.to).is_some_and(|r| r.kind == "entity") {
                let packet = context(g, &e.to, usize::MAX)?;
                for item in packet["items"].as_array().unwrap() {
                    if let Some(id) = item["id"].as_str() {
                        candidates.insert(id.into());
                    }
                }
            }
        }
        if g.records
            .get(id)
            .is_some_and(|r| r.kind == "question" && r.status == "answered")
        {
            for e in g
                .edges
                .values()
                .filter(|e| e.to == *id && e.rel == "answers")
            {
                if g.records
                    .get(&e.from)
                    .is_some_and(|r| r.status == "accepted")
                {
                    candidates.insert(e.from.clone());
                }
            }
        }
    }
    let mut full = vec![];
    let mut summaries = BTreeSet::new();
    for id in &candidates {
        let Some(r) = g.records.get(id) else {
            continue;
        };
        if r.kind == "note" {
            continue;
        }
        let rank = match (r.kind.as_str(), r.status.as_str()) {
            ("question", "open") => Some(0),
            ("decision", "accepted")
                if !g
                    .edges
                    .values()
                    .any(|e| e.rel == "supersedes" && e.to == *id) =>
            {
                Some(1)
            }
            ("knowledge" | "primer", "current") => Some(2),
            ("doctrine", "active") => Some(3),
            ("answer", "accepted") => Some(4),
            _ => None,
        };
        if let Some(rank) = rank {
            full.push((rank, id.clone()));
        } else {
            summaries.insert(id.clone());
        }
    }
    for e in g
        .edges
        .values()
        .filter(|e| e.rel == "supports" && candidates.contains(&e.to))
    {
        summaries.insert(e.from.clone());
    }
    full.sort();
    let full: Vec<_> = full.into_iter().map(|(_, id)| id).collect();
    for id in &full {
        summaries.remove(id);
    }
    let mut result = packet(g, full, summaries.into_iter().collect(), budget)?;
    result["linked_documents"] = serde_json::to_value(
        crate::beads::refs(g, reference)
            .into_iter()
            .filter(|r| r["kind"] != "note")
            .collect::<Vec<_>>(),
    )?;
    result["scope"] = json!(
        "linked documents and their entity context; not a complete path-based doctrine evaluation"
    );
    Ok(result)
}
