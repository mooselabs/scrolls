//! One parser for authored, imported and edge-based doctrine triggers.
use anyhow::{Context, Result, bail};
use globset::{Glob, GlobMatcher};
use serde_json::Value;

pub struct Triggers {
    globs: Vec<GlobMatcher>,
    pub scope: Option<String>,
}
impl Triggers {
    pub fn matches(&self, paths: &[String]) -> bool {
        self.globs
            .iter()
            .any(|g| paths.iter().any(|p| g.is_match(p)))
    }
}
pub fn glob(id: &str, field: &str, pattern: &str) -> Result<GlobMatcher> {
    if pattern.trim().is_empty() {
        bail!("{id}: {field} must be a nonempty string");
    }
    Ok(Glob::new(pattern)
        .with_context(|| format!("{id}: invalid {field} {pattern:?}"))?
        .compile_matcher())
}
pub fn parse(id: &str, fields: &Value, glob_key: &str) -> Result<Triggers> {
    let mut globs = vec![];
    if let Some(value) = fields.get(glob_key) {
        let values = match value {
            Value::String(_) => vec![value],
            Value::Array(values) => values.iter().collect(),
            _ => bail!("{id}: {glob_key} must be a string or array of strings"),
        };
        for (i, value) in values.into_iter().enumerate() {
            let field = format!("{glob_key}[{i}]");
            let pattern = value
                .as_str()
                .with_context(|| format!("{id}: {field} must be a nonempty string"))?;
            globs.push(glob(id, &field, pattern)?);
        }
    }
    let scope = fields
        .get("scope_clause")
        .map(|value| {
            value
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
                .with_context(|| format!("{id}: scope_clause must be a nonempty string"))
        })
        .transpose()?;
    Ok(Triggers { globs, scope })
}
/// Older imports encoded absent scope as null. Only this legacy adapter accepts it.
pub fn imported(entry: &Value) -> Result<Triggers> {
    let id = entry["id"]
        .as_str()
        .context("routing trigger requires record id")?;
    let mut fields = entry.clone();
    if fields.get("scope_clause").is_some_and(Value::is_null) {
        fields.as_object_mut().unwrap().remove("scope_clause");
    }
    parse(id, &fields, "globs")
}
pub fn routing_entries(routing: &Value) -> Result<&[Value]> {
    match routing.get("triggers") {
        None => Ok(&[]),
        Some(Value::Array(entries)) => Ok(entries),
        _ => bail!("routing.triggers must be an array"),
    }
}
