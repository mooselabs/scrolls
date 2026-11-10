//! Typed knowledge changes, reads and graph assembly, independent of the backend.
use crate::{model::*, source_import, storage::Store};
use anyhow::Result;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

/// A row after-image or edge deletion. Payloads are serialized once, preserving
/// struct field order for existing stores and proposal protocols.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Put {
        table: String,
        id: String,
        payload: String,
    },
    Insert {
        table: String,
        id: String,
        payload: String,
    },
    DeleteEdge {
        id: String,
    },
}

impl Change {
    pub fn put<T: Serialize>(table: &str, id: &str, value: &T) -> Result<Self> {
        Ok(Self::Put {
            table: table.into(),
            id: id.into(),
            payload: serde_json::to_string(value)?,
        })
    }
    pub fn insert<T: Serialize>(table: &str, id: &str, value: &T) -> Result<Self> {
        Ok(Self::Insert {
            table: table.into(),
            id: id.into(),
            payload: serde_json::to_string(value)?,
        })
    }
}

/// An ordered, atomic batch. Order matters for graph writes, immutable imports
/// and the receipt that accompanies them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeSet {
    pub changes: Vec<Change>,
}

impl From<Change> for ChangeSet {
    fn from(change: Change) -> Self {
        Self {
            changes: vec![change],
        }
    }
}

impl ChangeSet {
    pub fn push(&mut self, change: Change) {
        self.changes.push(change);
    }
    pub fn extend(&mut self, other: Self) {
        self.changes.extend(other.changes);
    }
    pub fn graph(before: &Graph, after: &Graph) -> Result<Self> {
        let mut changes = Self::default();
        for (id, r) in &after.records {
            if before.records.get(id) != Some(r) {
                changes.push(Change::put("records", id, r)?);
            }
        }
        for (id, e) in &after.edges {
            if before.edges.get(id) != Some(e) {
                changes.push(Change::put("edges", id, e)?);
            }
        }
        for id in before.edges.keys() {
            if !after.edges.contains_key(id) {
                changes.push(Change::DeleteEdge { id: id.clone() });
            }
        }
        Ok(changes)
    }
}

/// The read and write surface of the knowledge engine. Revisions and payloads keep their
/// backend representation until the later ADR-0280 format-neutrality cut.
///
/// `at: None` always selects the session's current view, including uncommitted
/// writes in its open transaction. `Some(at)` selects that ref or revision.
/// Rows are `(id, JSON payload)` pairs; their order is supplied by the backend.
pub trait KnowledgeStore {
    fn commit(&self, changes: &ChangeSet, message: &str, actor: &str) -> Result<()>;
    /// Commit on the shared session, validating its uncommitted view before
    /// persistence. `owned` retains the existing temporary-working-set policy.
    fn commit_checked(
        &self,
        changes: &ChangeSet,
        message: &str,
        actor: &str,
        owned: bool,
        validate: &mut dyn FnMut() -> Result<()>,
    ) -> Result<String>;
    /// The shared Markdown bulk transport keeps the same atomic validation.
    fn commit_markdown_checked(
        &self,
        changes: &ChangeSet,
        message: &str,
        actor: &str,
        validate: &mut dyn FnMut() -> Result<()>,
    ) -> Result<String>;
    fn receipt(&self, op: &str, hash: &str) -> Result<Option<Value>>;
    fn head(&self, reference: &str) -> Result<String>;
    fn revision(&self) -> Result<String>;
    fn branches(&self) -> Result<Vec<String>>;
    fn merge_base(&self, from: &str, to: &str) -> Result<String>;
    fn rows(&self, table: &str, at: Option<&str>) -> Result<Vec<(String, String)>>;
    fn one(&self, table: &str, id: &str, at: Option<&str>) -> Result<Option<String>>;
    fn proposals(&self) -> Result<Vec<Proposal>>;
    fn history(&self, at: &str, id: &str) -> Result<Vec<Value>>;
    fn log(&self, at: &str) -> Result<Vec<Value>>;
    fn diff(&self, from: &str, to: &str, table: &str) -> Result<String>;
    fn request_limit(&self) -> Option<usize>;
    fn shared(&self) -> bool;
}

impl KnowledgeStore for Store {
    fn commit(&self, changes: &ChangeSet, message: &str, actor: &str) -> Result<()> {
        self.transaction(&changes.to_sql(), message, actor)
    }
    fn commit_checked(
        &self,
        changes: &ChangeSet,
        message: &str,
        actor: &str,
        owned: bool,
        validate: &mut dyn FnMut() -> Result<()>,
    ) -> Result<String> {
        self.commit_shared_checked(&changes.to_sql(), message, actor, owned, validate)
    }
    fn commit_markdown_checked(
        &self,
        changes: &ChangeSet,
        message: &str,
        actor: &str,
        validate: &mut dyn FnMut() -> Result<()>,
    ) -> Result<String> {
        self.commit_shared_markdown_checked(&changes.to_sql(), message, actor, validate)
    }
    fn receipt(&self, op: &str, hash: &str) -> Result<Option<Value>> {
        Store::receipt(self, op, hash)
    }
    fn head(&self, reference: &str) -> Result<String> {
        Store::head(self, reference)
    }
    fn revision(&self) -> Result<String> {
        Store::revision(self)
    }
    fn branches(&self) -> Result<Vec<String>> {
        Store::branches(self)
    }
    fn merge_base(&self, from: &str, to: &str) -> Result<String> {
        Store::merge_base(self, from, to)
    }
    fn rows(&self, table: &str, at: Option<&str>) -> Result<Vec<(String, String)>> {
        Store::read_rows(self, table, at)
    }
    fn one(&self, table: &str, id: &str, at: Option<&str>) -> Result<Option<String>> {
        Store::read_one(self, table, id, at)
    }
    fn proposals(&self) -> Result<Vec<Proposal>> {
        Store::proposals(self)
    }
    fn history(&self, at: &str, id: &str) -> Result<Vec<Value>> {
        Store::history(self, at, id)
    }
    fn log(&self, at: &str) -> Result<Vec<Value>> {
        Store::log(self, at)
    }
    fn diff(&self, from: &str, to: &str, table: &str) -> Result<String> {
        Store::diff_text(self, from, to, table)
    }
    fn request_limit(&self) -> Option<usize> {
        Store::shared_query_limit(self)
    }
    fn shared(&self) -> bool {
        Store::is_shared(self)
    }
}

fn one<T: DeserializeOwned>(
    store: &dyn KnowledgeStore,
    table: &str,
    id: &str,
    at: Option<&str>,
) -> Result<Option<T>> {
    store
        .one(table, id, at)?
        .map(|payload| Ok(serde_json::from_str(&payload)?))
        .transpose()
}

fn keyed_rows<T: DeserializeOwned>(
    store: &dyn KnowledgeStore,
    table: &str,
    at: Option<&str>,
    key: fn(&T) -> String,
) -> Result<Vec<T>> {
    store
        .rows(table, at)?
        .into_iter()
        .map(|(id, payload)| {
            let value: T = serde_json::from_str(&payload)?;
            anyhow::ensure!(
                id == key(&value),
                "{}: invalid keyed row in {table}",
                if table.starts_with("import_") {
                    "source_ownership"
                } else {
                    "stored_row_identity"
                }
            );
            Ok(value)
        })
        .collect()
}

pub fn import_files(
    store: &dyn KnowledgeStore,
    at: Option<&str>,
) -> Result<Vec<source_import::File>> {
    if one::<u32>(store, "metadata", "schema", at)?.unwrap_or(1) < 4 {
        return Ok(vec![]);
    }
    keyed_rows(store, "import_files", at, source_import::File::key)
}

pub fn graph(store: &dyn KnowledgeStore, at: Option<&str>) -> Result<Graph> {
    let format = one::<u32>(store, "metadata", "schema", at)?.unwrap_or(1);
    let mut g = Graph {
        schema: one(store, "metadata", "ontology", at)?.unwrap_or_default(),
        records: keyed_rows::<Record>(store, "records", at, |r| r.id.clone())?
            .into_iter()
            .map(|mut r| {
                r.normalize_metadata();
                (r.id.clone(), r)
            })
            .collect(),
        edges: keyed_rows::<Edge>(store, "edges", at, Edge::key)?
            .into_iter()
            .map(|e| (e.key(), e))
            .collect(),
        ..Graph::default()
    };
    if format >= 4 {
        g.import_sources = keyed_rows::<source_import::Source>(store, "import_sources", at, |s| {
            s.source_id.clone()
        })?
        .into_iter()
        .map(|s| (s.source_id.clone(), s))
        .collect();
        g.import_members = keyed_rows::<source_import::Member>(
            store,
            "import_members",
            at,
            source_import::Member::row_key,
        )?
        .into_iter()
        .map(|m| (m.record_id.clone(), m))
        .collect();
    }
    source_import::validate(&g)?;
    Ok(g)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct Fixture {
        at: Option<&'static str>,
        format: Option<u32>,
        tables: BTreeMap<String, Vec<(String, String)>>,
    }
    impl KnowledgeStore for Fixture {
        fn commit(&self, _: &ChangeSet, _: &str, _: &str) -> Result<()> {
            anyhow::bail!("unexpected commit")
        }
        fn commit_checked(
            &self,
            _: &ChangeSet,
            _: &str,
            _: &str,
            _: bool,
            _: &mut dyn FnMut() -> Result<()>,
        ) -> Result<String> {
            anyhow::bail!("unexpected checked commit")
        }
        fn commit_markdown_checked(
            &self,
            _: &ChangeSet,
            _: &str,
            _: &str,
            _: &mut dyn FnMut() -> Result<()>,
        ) -> Result<String> {
            anyhow::bail!("unexpected Markdown commit")
        }
        fn receipt(&self, _: &str, _: &str) -> Result<Option<Value>> {
            anyhow::bail!("unexpected receipt read")
        }
        fn rows(&self, table: &str, at: Option<&str>) -> Result<Vec<(String, String)>> {
            assert_eq!(at, self.at);
            Ok(self.tables.get(table).cloned().unwrap_or_default())
        }
        fn one(&self, table: &str, id: &str, at: Option<&str>) -> Result<Option<String>> {
            assert_eq!(at, self.at);
            assert_eq!(table, "metadata");
            Ok(match id {
                "schema" => self.format.map(|f| f.to_string()),
                "ontology" => None,
                _ => panic!("unexpected metadata key {id}"),
            })
        }
        fn head(&self, _: &str) -> Result<String> {
            anyhow::bail!("graph loader must not resolve the current view to a commit")
        }
        fn revision(&self) -> Result<String> {
            anyhow::bail!("graph loader must not resolve the current view to a commit")
        }
        fn branches(&self) -> Result<Vec<String>> {
            anyhow::bail!("unexpected branch read")
        }
        fn merge_base(&self, _: &str, _: &str) -> Result<String> {
            anyhow::bail!("unexpected merge-base read")
        }
        fn proposals(&self) -> Result<Vec<Proposal>> {
            anyhow::bail!("unexpected proposal read")
        }
        fn history(&self, _: &str, _: &str) -> Result<Vec<Value>> {
            anyhow::bail!("unexpected history read")
        }
        fn log(&self, _: &str) -> Result<Vec<Value>> {
            anyhow::bail!("unexpected log read")
        }
        fn diff(&self, _: &str, _: &str, _: &str) -> Result<String> {
            anyhow::bail!("unexpected diff read")
        }
        fn request_limit(&self) -> Option<usize> {
            None
        }
        fn shared(&self) -> bool {
            false
        }
    }

    #[test]
    fn graph_preserves_selected_view_and_normalizes_legacy_metadata() -> Result<()> {
        let mut record = Record::blank("decision", "Read view", "", "reader");
        record.original = Some(serde_json::json!({}));
        record.aliases.push("ADR-0280".into());
        for at in [None, Some("pinned-revision")] {
            let fixture = Fixture {
                at,
                format: None,
                tables: BTreeMap::from([(
                    "records".into(),
                    vec![(record.id.clone(), serde_json::to_string(&record)?)],
                )]),
            };
            let g = graph(&fixture, at)?;
            let loaded = &g.records[&record.id];
            assert_eq!(loaded.metadata.as_ref().unwrap().number, "0280");
            assert_eq!(loaded.revision, record.revision);
            assert_eq!(loaded.actor, record.actor);
            assert!(import_files(&fixture, at)?.is_empty());
        }
        Ok(())
    }

    #[test]
    fn graph_rejects_miskeyed_rows_and_invalid_import_ownership() -> Result<()> {
        let record = Record::blank("note", "Read integrity", "", "reader");
        let mut fixture = Fixture {
            at: None,
            format: Some(4),
            tables: BTreeMap::from([(
                "records".into(),
                vec![("wrong-key".into(), serde_json::to_string(&record)?)],
            )]),
        };
        assert!(
            graph(&fixture, None)
                .unwrap_err()
                .to_string()
                .contains("stored_row_identity: invalid keyed row in records")
        );
        fixture.tables.clear();
        let member = source_import::Member {
            source_id: "missing-source".into(),
            key: "file.md".into(),
            record_id: record.id.clone(),
            path: "file.md".into(),
            codec: "markdown".into(),
            rule: "note".into(),
            metadata: Value::Null,
            spans: vec![],
            baseline: record,
            references: vec![],
            edges: BTreeMap::new(),
        };
        fixture.tables.insert(
            "import_members".into(),
            vec![(member.row_key(), serde_json::to_string(&member)?)],
        );
        assert!(
            graph(&fixture, None)
                .unwrap_err()
                .to_string()
                .contains("source_ownership: invalid membership")
        );
        Ok(())
    }
}
