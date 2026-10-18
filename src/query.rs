//! Shared document projections and explicitly bound, declarative predicates.
use crate::{
    cli::Filters,
    model::*,
    schema::{Direction, neighbor},
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};
pub fn projection(r: &Record) -> Result<Value> {
    let mut v = serde_json::to_value(r)?;
    v.as_object_mut().unwrap().remove("original");
    Ok(v)
}
pub fn reference(r: &Record) -> Value {
    json!({"id":r.id,"title":r.title,"kind":r.kind,"status":r.status,"revision":r.revision,"summary_only":true})
}
pub fn references(g: &Graph, id: &str) -> Vec<Value> {
    let mut refs = vec![];
    for e in g.edges.values() {
        for (direction, target) in [
            ("outgoing", if e.from == id { Some(&e.to) } else { None }),
            ("incoming", if e.to == id { Some(&e.from) } else { None }),
        ] {
            if let Some(target) = target {
                let mut v=g.records.get(target).map(reference).unwrap_or(json!({"id":target,"title":null,"kind":"external","status":null,"revision":null,"summary_only":true}));
                v["direction"] = json!(direction);
                v["relation"] = json!(e.rel);
                refs.push(v);
            }
        }
    }
    refs.sort_by_key(|v| {
        (
            v["direction"].to_string(),
            v["relation"].to_string(),
            v["id"].to_string(),
        )
    });
    refs.dedup();
    refs
}
fn bounded(mandatory: Vec<Value>, optional: Vec<Value>, budget: usize) -> Result<Value> {
    let mut items = mandatory;
    let minimum = serde_json::to_vec(&items)?.len();
    ensure!(
        minimum <= budget,
        "document requires at least {minimum} bytes; retry with --budget {minimum} or use show"
    );
    let mut omitted = vec![];
    for item in optional {
        items.push(item);
        if serde_json::to_vec(&items)?.len() > budget {
            omitted.push(items.pop().unwrap());
        }
    }
    let used = serde_json::to_vec(&items)?.len();
    Ok(
        json!({"items":items,"omitted":omitted.iter().map(|v|&v["id"]).collect::<Vec<_>>(),"used_bytes":used,"budget_bytes":budget,"over_budget_bytes":0,"budget_scope":"compact JSON items array; protocol metadata and omitted IDs excluded"}),
    )
}
pub fn context(g: &Graph, id: &str, budget: usize) -> Result<Value> {
    bounded(vec![projection(&g.records[id])?], references(g, id), budget)
}
pub fn prime(g: &Graph, budget: usize) -> Result<Value> {
    let guide = json!({"kinds":g.schema.kinds,"relations":g.schema.relations,"envelope_fields":g.schema.envelope_fields});
    let mut seen = BTreeSet::new();
    let mut items = vec![];
    for id in &g.schema.pins {
        let id = g.resolve(id)?;
        if seen.insert(id.clone()) {
            items.push(projection(&g.records[&id])?);
        }
    }
    let content = json!({"guide":guide,"instructions":g.schema.instructions,"items":items});
    let used = serde_json::to_vec(&content)?.len();
    let mut packet = content;
    packet["used_bytes"] = json!(used);
    packet["budget_bytes"] = json!(budget);
    packet["over_budget_bytes"] = json!(used.saturating_sub(budget));
    packet["omitted"] = json!([]);
    packet["budget_scope"] = json!(
        "compact JSON object containing guide, instructions, items; protocol metadata excluded"
    );
    Ok(packet)
}
pub fn traverse(
    g: &Graph,
    start: &str,
    rel: Option<&str>,
    depth: usize,
    direction: Direction,
    full: bool,
) -> Result<Value> {
    ensure!(depth <= 100, "walk depth must be <= 100");
    if let Some(rel) = rel {
        ensure!(
            g.schema.relations.contains_key(rel),
            "unknown relation {rel}"
        );
    }
    let mut seen = BTreeSet::from([start.to_string()]);
    let mut edges = BTreeSet::new();
    let mut queue = VecDeque::from([(start.to_string(), 0)]);
    while let Some((id, d)) = queue.pop_front() {
        if d >= depth {
            continue;
        }
        for (key, e) in &g.edges {
            if rel.is_some_and(|r| e.rel != r) {
                continue;
            }
            if let Some(next) = neighbor(e, &id, direction) {
                edges.insert(key.clone());
                if seen.insert(next.to_string()) && g.records.contains_key(next) {
                    queue.push_back((next.to_string(), d + 1));
                }
            }
        }
    }
    let items = seen
        .iter()
        .filter_map(|id| g.records.get(id))
        .map(|r| {
            if full {
                projection(r)
            } else {
                Ok(reference(r))
            }
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({"items":items,"edges":edges.iter().map(|id|&g.edges[id]).collect::<Vec<_>>()}))
}
pub fn walk(g: &Graph, start: &str, rel: Option<&str>, depth: usize) -> Vec<Record> {
    let data = traverse(g, start, rel, depth, Direction::Both, false).expect("bounded walk");
    data["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| g.records.get(v["id"].as_str()?))
        .cloned()
        .collect()
}
pub fn bead_context(g: &Graph, reference: &str, budget: usize) -> Result<Value> {
    bounded(vec![], references(g, reference), budget)
}
pub fn bead_packet(
    g: &Graph,
    reference: &str,
    local: &crate::beads::Local,
    budget: usize,
) -> Result<Value> {
    bounded(
        vec![json!({"reference":reference,"bead":local.summary(reference)})],
        references(g, reference),
        budget,
    )
}
pub fn field(r: &Record, path: &str) -> Value {
    if let Some(path) = path.strip_prefix("fields.") {
        return path
            .split('.')
            .fold(&r.fields, |v, k| v.get(k).unwrap_or(&Value::Null))
            .clone();
    }
    serde_json::to_value(r)
        .unwrap()
        .get(path)
        .cloned()
        .unwrap_or(Value::Null)
}
pub fn select<'a>(
    g: &'a Graph,
    filters: &Filters,
    terms: Option<&str>,
    limit: usize,
    actor: &str,
) -> Result<Vec<&'a Record>> {
    let fields = crate::mutation::fields(&filters.field)?;
    let terms: Vec<_> = terms
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    Ok(g.records
        .values()
        .filter(|r| {
            filters.kind.as_ref().is_none_or(|k| k == &r.kind)
                && filters.status.as_ref().is_none_or(|s| s == &r.status)
                && filters
                    .owner
                    .as_ref()
                    .is_none_or(|o| r.owner == if o == "me" { actor } else { o })
                && fields
                    .iter()
                    .all(|(k, v)| field(r, &format!("fields.{k}")) == *v)
                && terms.iter().all(|t| {
                    format!("{}\n{}", r.title, r.body)
                        .to_lowercase()
                        .contains(t)
                })
        })
        .take(limit)
        .collect())
}
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Expr {
    All {
        args: Vec<Expr>,
    },
    Any {
        args: Vec<Expr>,
    },
    Not {
        arg: Box<Expr>,
    },
    Compare {
        field: String,
        cmp: Compare,
        value: Value,
    },
    Exists {
        field: String,
    },
    Relation {
        relation: String,
        #[serde(default)]
        direction: Direction,
        #[serde(default)]
        min: Option<usize>,
        #[serde(default)]
        max: Option<usize>,
        #[serde(default, rename = "where")]
        predicate: Option<Box<Expr>>,
    },
    PathGlob {
        field: String,
    },
    Applicability,
    DatePlus {
        field: String,
        days: Value,
        cmp: Compare,
        value: Value,
    },
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compare {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}
fn compare(a: &Value, b: &Value, cmp: Compare) -> Result<bool> {
    if matches!(cmp, Compare::Eq | Compare::Ne) {
        return Ok(if matches!(cmp, Compare::Eq) {
            a == b
        } else {
            a != b
        });
    }
    let ordering = if let (Some(a), Some(b)) = (a.as_f64(), b.as_f64()) {
        a.partial_cmp(&b)
    } else if let (Some(a), Some(b)) = (a.as_str(), b.as_str()) {
        Some(a.cmp(b))
    } else {
        None
    }
    .context("ordered comparison requires matching numbers or strings")?;
    Ok(match cmp {
        Compare::Lt => ordering.is_lt(),
        Compare::Le => !ordering.is_gt(),
        Compare::Gt => ordering.is_gt(),
        Compare::Ge => !ordering.is_lt(),
        _ => unreachable!(),
    })
}
struct Bindings<'a> {
    actor: &'a str,
    paths: &'a [String],
    at: chrono::DateTime<chrono::Utc>,
    routing: &'a Value,
}
fn bound(v: &Value, b: &Bindings<'_>) -> Value {
    match v.as_str() {
        Some("$actor") => json!(b.actor),
        Some("$now") => json!(b.at.to_rfc3339()),
        _ => v.clone(),
    }
}
fn date(v: &Value) -> Result<chrono::DateTime<chrono::Utc>> {
    let s = v.as_str().context("date must be a string")?;
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(d.with_timezone(&chrono::Utc));
    }
    Ok(chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")?
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc())
}
fn globs(id: &str, value: &Value, paths: &[String]) -> Result<bool> {
    let values = value
        .as_array()
        .map(|a| a.iter().collect::<Vec<_>>())
        .unwrap_or(vec![value]);
    let mut found = false;
    for v in values {
        let glob = crate::triggers::glob(
            id,
            "path_glob",
            v.as_str().context("path_glob requires strings")?,
        )?;
        found |= paths.iter().any(|p| glob.is_match(p));
    }
    Ok(found)
}
fn applies(
    g: &Graph,
    r: &Record,
    b: &Bindings<'_>,
    diagnostics: &mut BTreeSet<(String, String)>,
) -> Result<bool> {
    let mut found = false;
    if let Some(k) = g.schema.kinds.get(&r.kind) {
        for (f, def) in &k.fields {
            if def.glob {
                if let Some(v) = r.fields.get(f) {
                    found |= globs(&r.id, v, b.paths)?;
                }
            }
        }
    }
    if let Some(scope) = r.fields.get("scope_clause") {
        let s = scope
            .as_str()
            .filter(|s| !s.is_empty())
            .context("scope_clause must be a nonempty string")?;
        diagnostics.insert((r.id.clone(), s.into()));
    }
    for e in g.edges.values().filter(|e| e.from == r.id) {
        if let Some(pattern) = e.to.strip_prefix("glob:") {
            found |= globs(&r.id, &json!(pattern), b.paths)?;
        }
    }
    for entry in crate::triggers::routing_entries(b.routing)?
        .iter()
        .filter(|t| t["id"] == r.id)
    {
        let parsed = crate::triggers::imported(entry)?;
        found |= parsed.matches(b.paths);
        if let Some(scope) = parsed.scope {
            diagnostics.insert((r.id.clone(), scope));
        }
    }
    Ok(found)
}
fn matches(
    expr: &Expr,
    g: &Graph,
    r: &Record,
    b: &Bindings<'_>,
    diagnostics: &mut BTreeSet<(String, String)>,
) -> Result<bool> {
    Ok(match expr {
        Expr::All { args } | Expr::Any { args } => {
            let vals = args
                .iter()
                .map(|a| matches(a, g, r, b, diagnostics))
                .collect::<Result<Vec<_>>>()?;
            if matches!(expr, Expr::All { .. }) {
                vals.iter().all(|v| *v)
            } else {
                vals.iter().any(|v| *v)
            }
        }
        Expr::Not { arg } => !matches(arg, g, r, b, diagnostics)?,
        Expr::Compare {
            field: f,
            cmp,
            value,
        } => compare(&field(r, f), &bound(value, b), *cmp)?,
        Expr::Exists { field: f } => !field(r, f).is_null(),
        Expr::Relation {
            relation,
            direction,
            min,
            max,
            predicate,
        } => {
            ensure!(
                g.schema.relations.contains_key(relation),
                "unknown relation {relation}"
            );
            let mut count = 0;
            for e in g.edges.values().filter(|e| &e.rel == relation) {
                if let Some(id) = neighbor(e, &r.id, *direction) {
                    let matched = if let Some(p) = predicate {
                        if let Some(r) = g.records.get(id) {
                            matches(p, g, r, b, diagnostics)?
                        } else {
                            false
                        }
                    } else {
                        true
                    };
                    if matched {
                        count += 1;
                    }
                }
            }
            count >= min.unwrap_or(if max.is_some() { 0 } else { 1 })
                && max.is_none_or(|max| count <= max)
        }
        Expr::PathGlob { field: f } => {
            let v = field(r, f);
            if v.is_null() {
                false
            } else {
                globs(&r.id, &v, b.paths)?
            }
        }
        Expr::Applicability => applies(g, r, b, diagnostics)?,
        Expr::DatePlus {
            field: f,
            days,
            cmp,
            value,
        } => {
            let v = field(r, f);
            if v.is_null() || v == "" {
                false
            } else {
                let days = if let Some(f) = days.get("field").and_then(Value::as_str) {
                    field(r, f)
                } else {
                    days.clone()
                };
                let delta = chrono::Duration::try_days(
                    days.as_i64()
                        .context("days must be integer or {field: name}")?,
                )
                .context("duration overflow")?;
                let d = date(&v)?
                    .checked_add_signed(delta)
                    .context("date overflow")?;
                compare(
                    &json!(d.timestamp()),
                    &json!(date(&bound(value, b))?.timestamp()),
                    *cmp,
                )?
            }
        }
    })
}
fn validate_expression(expr: &Expr, schema: &crate::schema::Schema) -> Result<()> {
    match expr {
        Expr::All { args } | Expr::Any { args } => {
            for arg in args {
                validate_expression(arg, schema)?;
            }
        }
        Expr::Not { arg } => validate_expression(arg, schema)?,
        Expr::Relation {
            relation,
            min,
            max,
            predicate,
            ..
        } => {
            ensure!(
                schema.relations.contains_key(relation),
                "unknown relation {relation}"
            );
            ensure!(
                max.is_none_or(|m| m >= min.unwrap_or(0)),
                "invalid relationship count bounds"
            );
            if let Some(p) = predicate {
                validate_expression(p, schema)?;
            }
        }
        Expr::DatePlus { days, value, .. } => {
            ensure!(
                days.is_i64() || days.get("field").is_some_and(Value::is_string),
                "days must be integer or {{field: name}}"
            );
            if value != "$now" {
                date(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}
pub fn evaluate(
    g: &Graph,
    routing: &Value,
    text: &str,
    actor: &str,
    paths: &[String],
    at: Option<&str>,
) -> Result<Value> {
    let expr: Expr = serde_yaml::from_str(text)?;
    validate_expression(&expr, &g.schema)?;
    let at = at
        .map(|s| chrono::DateTime::parse_from_rfc3339(s).map(|d| d.with_timezone(&chrono::Utc)))
        .transpose()?
        .unwrap_or_else(chrono::Utc::now);
    let b = Bindings {
        actor,
        paths,
        at,
        routing,
    };
    let mut diagnostics = BTreeSet::new();
    let mut items = vec![];
    for r in g.records.values() {
        if matches(&expr, g, r, &b, &mut diagnostics)? {
            items.push(reference(r));
        }
    }
    Ok(
        json!({"items":items,"diagnostics":diagnostics.into_iter().map(|(id,scope)|json!({"id":id,"scope_clause":scope,"reason":"unresolved textual applicability; evaluate explicitly"})).collect::<Vec<_>>(),"bindings":{"actor":actor,"paths":paths,"at":at}}),
    )
}
// Format-adapter helper retained for imported routing qualification.
pub fn doctrine(g: &Graph, routing: &Value, paths: &[String]) -> Result<Value> {
    let b = Bindings {
        actor: "reader",
        paths,
        at: chrono::Utc::now(),
        routing,
    };
    let mut diagnostics = BTreeSet::new();
    let mut matched = vec![];
    for r in g.records.values() {
        if applies(g, r, &b, &mut diagnostics)? {
            matched.push(projection(r)?);
        }
    }
    Ok(
        json!({"matched":matched,"undecided":diagnostics.into_iter().map(|(id,scope)|json!({"id":id,"scope_clause":scope})).collect::<Vec<_>>()}),
    )
}
