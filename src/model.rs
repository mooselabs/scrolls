use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub enum Role {
    Human,
    Agent,
}
impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
        }
    }
}

pub(crate) mod import_types {
    use super::{Edge, Record, SourceFile, digest};
    use serde::{Deserialize, Serialize};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    pub struct Source {
        pub source_id: String,
        pub repository: String,
        pub commit: String,
        pub tree: String,
        pub root: String,
        pub profile: String,
        pub profile_hash: String,
        pub compiled_hash: String,
        pub schema_hash: String,
        pub contract: String,
        pub candidate_digest: String,
        pub actor: String,
        pub role: String,
        pub imported_at: String,
        pub op_id: String,
        pub coverage: Value,
    }
    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    pub struct File {
        pub source_id: String,
        pub size: u64,
        pub file: SourceFile,
        pub classification: String,
    }
    impl File {
        pub fn key(&self) -> String {
            tuple("scrolls.import.file.v1", &self.source_id, &self.file.path)
        }
    }
    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    pub struct Member {
        pub source_id: String,
        pub key: String,
        pub record_id: String,
        pub path: String,
        pub codec: String,
        pub rule: String,
        pub metadata: Value,
        pub spans: Vec<Value>,
        pub baseline: Record,
        pub references: Vec<Value>,
        pub edges: BTreeMap<String, Edge>,
    }
    impl Member {
        pub fn row_key(&self) -> String {
            tuple("scrolls.import.member.v1", &self.source_id, &self.key)
        }
    }
    pub(crate) fn tuple(version: &str, source: &str, key: &str) -> String {
        digest(serde_json::to_vec(&json!([version, source, key])).unwrap())
    }
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<RecordMetadata>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordMetadata {
    pub number: String,
}
impl Record {
    pub fn new(kind: &str, title: &str, body: &str, actor: &str) -> Result<Self> {
        crate::schema::Schema::default().new_record(kind, title, body, actor)
    }
    pub fn blank(kind: &str, title: &str, body: &str, actor: &str) -> Self {
        Self {
            id: id(),
            kind: kind.into(),
            title: title.into(),
            status: String::new(),
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
            metadata: None,
        }
    }
    /// Backward-compatible read normalization; it never changes revision or attribution.
    pub fn normalize_metadata(&mut self) {
        if self.metadata.is_none() && self.kind == "decision" && self.original.is_some() {
            let number = self
                .aliases
                .iter()
                .find_map(|a| a.strip_prefix("ADR-"))
                .or_else(|| self.path.as_deref()?.rsplit('/').next()?.split('-').next());
            if let Some(number) =
                number.filter(|n| n.len() == 4 && n.bytes().all(|b| b.is_ascii_digit()))
            {
                self.metadata = Some(RecordMetadata {
                    number: number.into(),
                });
            }
        }
    }
    pub fn check_edit(&self, expected: u64) -> Result<()> {
        if self.frozen {
            bail!("{} is immutable imported compatibility history", self.id);
        }
        self.check_revision(expected)
    }
    pub fn check_revision(&self, expected: u64) -> Result<()> {
        if self.revision != expected {
            bail!(
                "stale edit: expected revision {expected}, current {}; read show and retry with a fresh --op-id",
                self.revision
            );
        }
        Ok(())
    }
}
pub fn lifecycle(kind: &str) -> Result<Vec<String>> {
    Ok(crate::schema::Schema::default().kind(kind)?.states.clone())
}
pub fn transition(kind: &str, from: &str, to: &str) -> Result<()> {
    crate::schema::Schema::default().transition(kind, from, to)
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
    pub import_sources: BTreeMap<String, import_types::Source>,
    pub import_members: BTreeMap<String, import_types::Member>,
    pub schema: crate::schema::Schema,
    pub records: BTreeMap<String, Record>,
    pub edges: BTreeMap<String, Edge>,
}
impl Graph {
    pub fn resolve(&self, name: &str) -> Result<String> {
        if self.records.contains_key(name) {
            return Ok(name.into());
        }
        let aliases: Vec<_> = self
            .records
            .values()
            .filter(|r| r.path.as_deref() == Some(name) || r.aliases.iter().any(|a| a == name))
            .map(|r| r.id.clone())
            .collect();
        let ids = if aliases.is_empty() {
            self.records
                .values()
                .filter(|r| r.title.eq_ignore_ascii_case(name))
                .map(|r| r.id.clone())
                .collect()
        } else {
            aliases
        };
        match ids.as_slice() {
            [id] => Ok(id.clone()),
            [] => bail!("unknown record {name}; use search or list"),
            _ => bail!(
                "ambiguous alias {name}: {}",
                ids.iter()
                    .map(|id| self
                        .import_members
                        .get(id)
                        .map(|m| format!("{}:{}", m.source_id, m.key))
                        .unwrap_or(id.clone()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
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
    "related_work",
    "implemented_by",
    "produced_by",
    "related",
];
