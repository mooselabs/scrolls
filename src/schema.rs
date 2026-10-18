//! Versioned, data-defined ontology. No domain kind names are interpreted here.
use crate::model::{Graph, Record};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
fn yes() -> bool {
    true
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Schema {
    pub format: u32,
    #[serde(default)]
    pub body_links: BTreeMap<String, String>,
    pub kinds: BTreeMap<String, Kind>,
    #[serde(default)]
    pub relations: BTreeMap<String, Relation>,
    #[serde(default)]
    pub envelope_fields: BTreeMap<String, Field>,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub pins: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Kind {
    #[serde(default)]
    pub description: String,
    pub initial: String,
    pub states: Vec<String>,
    #[serde(default)]
    pub transitions: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub fields: BTreeMap<String, Field>,
    #[serde(default)]
    pub additional_fields: bool,
    pub write: BTreeMap<String, WritePolicy>,
    #[serde(default)]
    pub guards: Vec<Guard>,
    #[serde(default)]
    pub cardinality: Vec<Cardinality>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WritePolicy {
    Direct,
    Proposal,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Field {
    #[serde(rename = "type")]
    pub ty: FieldType,
    #[serde(default)]
    pub required: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_default"
    )]
    pub default: Option<Value>,
    #[serde(default)]
    pub values: Vec<Value>,
    #[serde(default)]
    pub items: Option<Box<Field>>,
    #[serde(default)]
    pub glob: bool,
    #[serde(default)]
    pub nonempty: bool,
    #[serde(default)]
    pub minimum: Option<f64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    String,
    Number,
    Integer,
    Boolean,
    Enum,
    Date,
    Timestamp,
    List,
    Json,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Relation {
    pub from: Vec<String>,
    pub to: Vec<String>,
    #[serde(default)]
    pub external: Vec<String>,
    #[serde(default)]
    pub reason: bool,
    #[serde(default = "yes")]
    pub self_link: bool,
    #[serde(default = "yes")]
    pub cycles: bool,
    #[serde(default)]
    pub same_kind: bool,
    pub affects: Affects,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Affects {
    Source,
    Both,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Guard {
    pub to: String,
    #[serde(default)]
    pub human: bool,
    #[serde(default)]
    pub relation: Option<String>,
    #[serde(default)]
    pub direction: Direction,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Incoming,
    #[default]
    Outgoing,
    Both,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Cardinality {
    pub name: String,
    pub relation: String,
    pub direction: Direction,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub related_status: Option<String>,
    #[serde(default)]
    pub min: usize,
    #[serde(default)]
    pub max: Option<usize>,
    #[serde(default)]
    pub skip_frozen: bool,
}
impl Default for Schema {
    fn default() -> Self {
        serde_yaml::from_str(include_str!("../schemas/knowledge.yaml")).expect("bundled schema")
    }
}
fn present_default<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}
impl Field {
    fn definition(&self) -> Result<()> {
        ensure!(
            self.minimum.is_none_or(f64::is_finite),
            "minimum must be finite"
        );
        ensure!(
            self.ty != FieldType::Enum || !self.values.is_empty(),
            "enum requires values"
        );
        if let Some(items) = &self.items {
            ensure!(self.ty == FieldType::List, "items requires list type");
            items.definition()?;
        }
        if let Some(value) = &self.default {
            self.validate(value)?;
        }
        Ok(())
    }
    pub fn validate(&self, value: &Value) -> Result<()> {
        let valid = match self.ty {
            FieldType::String => value.is_string(),
            FieldType::Number => value.is_number(),
            FieldType::Integer => value.is_i64() || value.is_u64(),
            FieldType::Boolean => value.is_boolean(),
            FieldType::Enum => self.values.contains(value),
            FieldType::Date => value
                .as_str()
                .is_some_and(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()),
            FieldType::Timestamp => value
                .as_str()
                .is_some_and(|s| chrono::DateTime::parse_from_rfc3339(s).is_ok()),
            FieldType::List => value.is_array(),
            FieldType::Json => true,
        };
        ensure!(valid, "expected {:?}, got {value}", self.ty);
        if self.nonempty {
            ensure!(
                value.as_str().is_some_and(|s| !s.trim().is_empty()),
                "must be nonempty"
            );
        }
        if let Some(min) = self.minimum {
            ensure!(value.as_f64().is_some_and(|n| n >= min), "must be >= {min}");
        }
        if let (Some(items), Some(values)) = (&self.items, value.as_array()) {
            for v in values {
                items.validate(v)?;
            }
        }
        if self.glob {
            let values = value
                .as_array()
                .map(|a| a.iter().collect::<Vec<_>>())
                .unwrap_or(vec![value]);
            for v in values {
                crate::triggers::glob(
                    "field",
                    "glob",
                    v.as_str()
                        .context("glob must be a string or list of strings")?,
                )?;
            }
        }
        Ok(())
    }
}
impl Schema {
    pub fn parse(text: &str) -> Result<Self> {
        let s: Self = serde_yaml::from_str(text)?;
        s.validate()?;
        Ok(s)
    }
    pub fn revision(&self) -> String {
        crate::model::digest(serde_json::to_vec(self).expect("schema serializes"))
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.format == 1,
            "unsupported ontology format {}",
            self.format
        );
        ensure!(!self.kinds.is_empty(), "schema requires kinds");
        for rel in self.body_links.values() {
            ensure!(
                self.relations.contains_key(rel),
                "unknown body link relation {rel}"
            );
            ensure!(
                self.reference(rel)?,
                "body link relations must affect only their source"
            );
        }
        for (name, k) in &self.kinds {
            ensure!(
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'),
                "invalid kind {name}"
            );
            ensure!(
                k.states.contains(&k.initial),
                "{name}: initial state missing"
            );
            for (from, tos) in &k.transitions {
                ensure!(
                    k.states.contains(from) && tos.iter().all(|t| k.states.contains(t)),
                    "{name}: unknown transition state"
                );
            }
            for role in ["human", "agent"] {
                ensure!(
                    k.write.contains_key(role),
                    "{name}: missing {role} write policy"
                );
            }
            for g in &k.guards {
                ensure!(k.states.contains(&g.to), "{name}: unknown guard state");
                if let Some(r) = &g.relation {
                    ensure!(
                        self.relations.contains_key(r),
                        "{name}: unknown guard relation {r}"
                    );
                }
            }
            for c in &k.cardinality {
                ensure!(
                    self.relations.contains_key(&c.relation),
                    "{name}: unknown cardinality relation {}",
                    c.relation
                );
                ensure!(
                    c.max.is_none_or(|m| m >= c.min),
                    "{name}: invalid cardinality bounds"
                );
                ensure!(
                    c.status.as_ref().is_none_or(|s| k.states.contains(s)),
                    "{name}: unknown cardinality status"
                );
            }
            for (f, def) in &k.fields {
                def.definition().with_context(|| format!("{name}.{f}"))?;
                if let Some(v) = &def.default {
                    def.validate(v)
                        .with_context(|| format!("{name}.{f} default"))?;
                }
            }
        }
        for (name, r) in &self.relations {
            for k in r.from.iter().chain(&r.to) {
                ensure!(
                    k == "*" || self.kinds.contains_key(k),
                    "{name}: unknown kind {k}"
                );
            }
            for e in &r.external {
                ensure!(
                    ["beads", "glob", "text", "url"].contains(&e.as_str()),
                    "{name}: unknown external type {e}"
                );
            }
        }
        for (f, def) in &self.envelope_fields {
            def.definition().with_context(|| format!("envelope.{f}"))?;
            ensure!(
                ["confidence", "review_every", "last_reviewed"].contains(&f.as_str()),
                "unsupported optional envelope field {f}"
            );
            if let Some(v) = &def.default {
                def.validate(v)?;
            }
        }
        Ok(())
    }
    pub fn resolve_pins(&mut self, g: &Graph) -> Result<()> {
        let mut seen = BTreeSet::new();
        let mut pins = vec![];
        for pin in &self.pins {
            let id = g.resolve(pin)?;
            if seen.insert(id.clone()) {
                pins.push(id);
            }
        }
        self.pins = pins;
        Ok(())
    }
    pub fn kind(&self, name: &str) -> Result<&Kind> {
        self.kinds
            .get(name)
            .with_context(|| format!("unknown configured kind {name}"))
    }
    pub fn transition(&self, kind: &str, from: &str, to: &str) -> Result<()> {
        let k = self.kind(kind)?;
        ensure!(
            k.states.iter().any(|s| s == to)
                && (from == to
                    || k.transitions
                        .get(from)
                        .is_some_and(|ts| ts.iter().any(|s| s == to))),
            "invalid {kind} transition {from} -> {to}"
        );
        Ok(())
    }
    pub fn new_record(&self, kind: &str, title: &str, body: &str, actor: &str) -> Result<Record> {
        let k = self.kind(kind)?;
        let mut r = Record::blank(kind, title, body, actor);
        r.status = k.initial.clone();
        for (f, def) in &k.fields {
            if let Some(v) = &def.default {
                r.fields[f] = v.clone();
            }
        }
        r.confidence = self
            .envelope_fields
            .get("confidence")
            .and_then(|f| f.default.as_ref())
            .and_then(Value::as_str)
            .unwrap_or("")
            .into();
        if let Some(value) = self
            .envelope_fields
            .get("last_reviewed")
            .and_then(|f| f.default.as_ref())
            .and_then(Value::as_str)
        {
            r.last_reviewed = value.into();
        }
        r.review_every = self
            .envelope_fields
            .get("review_every")
            .and_then(|f| f.default.as_ref())
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .try_into()?;
        Ok(r)
    }
    pub fn protected(&self, r: &Record, role: &str) -> bool {
        self.kinds.get(&r.kind).and_then(|k| k.write.get(role)) != Some(&WritePolicy::Direct)
    }
    pub fn reference(&self, rel: &str) -> Result<bool> {
        Ok(self
            .relations
            .get(rel)
            .with_context(|| format!("unknown relation {rel}"))?
            .affects
            == Affects::Source)
    }
    pub fn validate_record(&self, r: &Record) -> Result<()> {
        let k = self
            .kind(&r.kind)
            .with_context(|| format!("record {}", r.id))?;
        ensure!(k.states.contains(&r.status), "{}:invalid-status", r.id);
        ensure!(
            !r.title.trim().is_empty() && !r.owner.trim().is_empty(),
            "{}:required-fields",
            r.id
        );
        let fields = r.fields.as_object().context("fields must be an object")?;
        for (name, def) in &k.fields {
            match fields.get(name) {
                Some(v) => def
                    .validate(v)
                    .with_context(|| format!("{}:field:{name}", r.id))?,
                None => ensure!(!def.required, "{}:required-field:{name}", r.id),
            }
        }
        if !k.additional_fields {
            for f in fields.keys() {
                ensure!(k.fields.contains_key(f), "{}:unknown-field:{f}", r.id);
            }
        }
        let envelope = json!({"confidence":r.confidence,"review_every":r.review_every,"last_reviewed":r.last_reviewed});
        for (name, def) in &self.envelope_fields {
            def.validate(&envelope[name])
                .with_context(|| format!("{}:{name}", r.id))?;
        }
        Ok(())
    }
    /// Authority is always read from the original graph, never the proposed graph.
    pub fn guards(&self, before: &Graph, after: &Graph, actor: &str, role: &str) -> Result<()> {
        for r in after.records.values() {
            // An old configuration has no guard for a newly introduced kind;
            // acceptance also evaluates the merged configuration's guards.
            let Some(k) = self.kinds.get(&r.kind) else {
                continue;
            };
            for guard in k
                .guards
                .iter()
                .filter(|g| g.to == r.status && guard_changed(before, after, r, g))
            {
                ensure!(
                    !guard.human || role == "human",
                    "{}: lifecycle guard requires human",
                    r.id
                );
                for owners in guard_owners(before, after, &r.id, guard) {
                    if !owners.contains(actor) {
                        bail!("{}: lifecycle guard requires pre-change owner", r.id);
                    }
                }
            }
        }
        Ok(())
    }
}
pub fn neighbor<'a>(e: &'a crate::model::Edge, id: &str, direction: Direction) -> Option<&'a str> {
    if e.from == id && direction != Direction::Incoming {
        Some(&e.to)
    } else if e.to == id && direction != Direction::Outgoing {
        Some(&e.from)
    } else {
        None
    }
}

fn guard_targets(g: &Graph, id: &str, guard: &Guard) -> BTreeSet<String> {
    if let Some(rel) = &guard.relation {
        g.edges
            .values()
            .filter(|e| &e.rel == rel)
            .filter_map(|e| neighbor(e, id, guard.direction))
            .map(str::to_string)
            .collect()
    } else {
        BTreeSet::from([id.into()])
    }
}
pub fn guard_changed(before: &Graph, after: &Graph, r: &Record, guard: &Guard) -> bool {
    before
        .records
        .get(&r.id)
        .is_none_or(|o| o.status != r.status)
        || guard_targets(before, &r.id, guard) != guard_targets(after, &r.id, guard)
}
pub fn guard_owners(
    before: &Graph,
    after: &Graph,
    id: &str,
    guard: &Guard,
) -> Vec<BTreeSet<String>> {
    let previous = guard_targets(before, id, guard);
    let next = guard_targets(after, id, guard);
    let mut targets = vec![previous.clone()];
    if previous != next {
        targets.push(next);
    }
    targets
        .into_iter()
        .map(|ids| {
            ids.iter()
                .filter_map(|id| before.records.get(id))
                .map(|r| r.owner.clone())
                .collect()
        })
        .collect()
}
