use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
pub fn digest(bytes: impl AsRef<[u8]>) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}
pub fn id() -> String {
    format!("sc-{}", uuid::Uuid::new_v4().simple())
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Record {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub status: String,
    pub body: String,
    pub owner: String,
    pub confidence: String,
    pub created: String,
    pub last_reviewed: String,
    pub review_every: u32,
    pub actor: String,
    pub visibility: String,
    pub sync: String,
    pub fields: Value,
    pub revision: u64,
    pub path: Option<String>,
    pub frozen: bool,
    pub original: Option<Value>,
    pub aliases: Vec<String>,
}
impl Record {
    pub fn new(kind: &str, title: &str, body: &str, actor: &str) -> Result<Self> {
        let lifecycle = lifecycle(kind)?;
        Ok(Self {
            id: id(),
            kind: kind.into(),
            title: title.into(),
            status: lifecycle[0].into(),
            body: body.into(),
            owner: actor.into(),
            confidence: "medium".into(),
            created: now(),
            last_reviewed: now(),
            review_every: 90,
            actor: actor.into(),
            visibility: "internal".into(),
            sync: "always".into(),
            fields: serde_json::json!({}),
            revision: 1,
            path: None,
            frozen: false,
            original: None,
            aliases: vec![],
        })
    }
    pub fn protected(&self, role: &str) -> bool {
        matches!(
            self.kind.as_str(),
            "knowledge" | "primer" | "decision" | "doctrine" | "entity"
        ) || (self.kind == "answer" && role != "human")
    }
    pub fn check_edit(&self, expected: u64) -> Result<()> {
        if self.frozen {
            bail!("{} is immutable imported compatibility history", self.id);
        }
        if self.revision != expected {
            bail!(
                "stale edit: expected revision {expected}, current {}; read show and retry with a fresh --op-id",
                self.revision
            );
        }
        Ok(())
    }
}
pub fn lifecycle(kind: &str) -> Result<&'static [&'static str]> {
    Ok(match kind {
        "decision" => &["proposed", "accepted", "superseded", "retired"],
        "knowledge" | "primer" => &["draft", "current", "stale", "retired"],
        "evidence" => &["captured", "archived"],
        "question" => &["open", "answered", "withdrawn"],
        "answer" => &["offered", "accepted", "declined"],
        "doctrine" => &["proposed", "active", "retired"],
        "entity" => &["active", "retired"],
        "collection" => &["current", "stale"],
        "note" => &["recorded"],
        _ => bail!("unknown kind {kind}; registry editing is deferred"),
    })
}
pub fn transition(kind: &str, from: &str, to: &str) -> Result<()> {
    lifecycle(kind)?;
    let valid = from == to
        || match kind {
            "decision" => matches!(
                (from, to),
                ("proposed", "accepted")
                    | ("proposed", "retired")
                    | ("accepted", "superseded")
                    | ("accepted", "retired")
            ),
            "knowledge" | "primer" => matches!(
                (from, to),
                ("draft", "current")
                    | ("current", "stale")
                    | ("stale", "current")
                    | ("draft" | "current" | "stale", "retired")
            ),
            "evidence" => (from, to) == ("captured", "archived"),
            "question" => matches!((from, to), ("open", "answered" | "withdrawn")),
            "answer" => matches!((from, to), ("offered", "accepted" | "declined")),
            "doctrine" => matches!(
                (from, to),
                ("proposed", "active" | "retired") | ("active", "retired")
            ),
            "entity" => (from, to) == ("active", "retired"),
            "collection" => matches!((from, to), ("current", "stale") | ("stale", "current")),
            _ => false,
        };
    if !valid {
        bail!("invalid {kind} transition {from} -> {to}");
    }
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Edge {
    pub from: String,
    pub rel: String,
    pub to: String,
    pub why: String,
    pub actor: String,
    pub created: String,
}
impl Edge {
    pub fn key(&self) -> String {
        digest(format!("{}\0{}\0{}", self.from, self.rel, self.to))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SourceFile {
    pub path: String,
    pub sha256: String,
    pub git_oid: String,
    pub mode: u32,
    pub bytes: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ImportInfo {
    pub repository: String,
    pub commit: String,
    pub prefix: String,
    pub files: usize,
    pub markdown: usize,
    pub untyped: usize,
    pub lint_debt: BTreeSet<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Proposal {
    pub branch: String,
    pub base: String,
    pub actor: String,
    pub status: String,
    pub reason: Option<String>,
}
#[derive(Clone, Default, Debug)]
pub struct Graph {
    pub records: BTreeMap<String, Record>,
    pub edges: BTreeMap<String, Edge>,
}
impl Graph {
    pub fn resolve(&self, name: &str) -> Result<String> {
        if self.records.contains_key(name) {
            return Ok(name.into());
        }
        let ids: Vec<_> = self
            .records
            .values()
            .filter(|r| {
                r.path.as_deref() == Some(name)
                    || (r.kind == "entity" && r.title.eq_ignore_ascii_case(name))
                    || r.aliases.iter().any(|a| a == name)
            })
            .map(|r| r.id.clone())
            .collect();
        match ids.as_slice() {
            [id] => Ok(id.clone()),
            [] => bail!("unknown record {name}; use search or list"),
            _ => bail!("ambiguous alias {name}: {}", ids.join(", ")),
        }
    }
    pub fn add_edge(&mut self, from: &str, rel: &str, to: &str, why: &str, actor: &str) {
        let e = Edge {
            from: from.into(),
            rel: rel.into(),
            to: to.into(),
            why: why.into(),
            actor: actor.into(),
            created: now(),
        };
        self.edges.insert(e.key(), e);
    }
}
pub const KINDS: &[&str] = &[
    "decision",
    "knowledge",
    "primer",
    "evidence",
    "question",
    "answer",
    "doctrine",
    "entity",
    "collection",
    "note",
];
pub const RELATIONS: &[&str] = &[
    "supports",
    "derived_from",
    "supersedes",
    "contradicts",
    "about",
    "applies_to",
    "asks_about",
    "answers",
    "promoted_to",
    "depends_on",
    "blocks",
    "related",
];
