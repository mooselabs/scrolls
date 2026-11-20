//! Typed knowledge changes, reads and graph assembly, independent of the backend.
pub use crate::review::{OperationContext, RecoveryRequest, ReviewRequest};
use crate::{model::*, source_import};
use anyhow::Result;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::BTreeMap;

/// Newly allocated alias by record id, in deterministic record-id order.
pub type AliasAllocations = BTreeMap<String, String>;

/// A record snapshot at an immutable store revision, oldest first.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryEntry {
    pub store_revision: String,
    pub actor: String,
    /// UTC RFC3339 timestamp.
    pub time: String,
    pub record: Record,
}

/// An attributed store publication, oldest first. Revisions are opaque.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogEntry {
    pub store_revision: String,
    pub actor: String,
    /// UTC RFC3339 timestamp.
    pub time: String,
    pub message: String,
}

/// A decoded row change. A missing side denotes insertion or deletion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowChange {
    pub table: String,
    pub id: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
}

impl RowChange {
    /// Compare keyed JSON rows deterministically, independent of serialization
    /// whitespace, key ordering, or backend row ordering.
    pub fn between(
        table: &str,
        before: Vec<(String, String)>,
        after: Vec<(String, String)>,
    ) -> Result<Vec<Self>> {
        fn decode(rows: Vec<(String, String)>) -> Result<BTreeMap<String, Value>> {
            let mut decoded = BTreeMap::new();
            for (id, payload) in rows {
                anyhow::ensure!(
                    decoded
                        .insert(id, serde_json::from_str(&payload)?)
                        .is_none(),
                    "duplicate row identity in proposal changes"
                );
            }
            Ok(decoded)
        }
        let before = decode(before)?;
        let after = decode(after)?;
        let ids: std::collections::BTreeSet<_> = before.keys().chain(after.keys()).collect();
        Ok(ids
            .into_iter()
            .filter(|id| before.get(*id) != after.get(*id))
            .map(|id| Self {
                table: table.into(),
                id: id.clone(),
                before: before.get(id).cloned(),
                after: after.get(id).cloned(),
            })
            .collect())
    }
}

pub(crate) fn alias_result(mut result: Value, allocated: &AliasAllocations) -> Value {
    if !allocated.is_empty() {
        result["allocated_aliases"] = serde_json::json!(allocated);
    }
    result
}

fn allocate_aliases(main: &Graph, candidate: &mut Graph) -> Result<AliasAllocations> {
    candidate.schema.validate()?;
    let mut allocated = AliasAllocations::new();
    for id in candidate.records.keys().cloned().collect::<Vec<_>>() {
        let record = &candidate.records[&id];
        if main.records.contains_key(&id)
            || record.original.is_some()
            || record.path.is_some()
            || candidate.import_members.contains_key(&id)
        {
            continue;
        }
        let Some(sequence) = candidate
            .schema
            .kinds
            .get(&record.kind)
            .and_then(|k| k.alias_sequence.as_ref())
        else {
            continue;
        };
        if record.aliases.iter().any(|a| sequence.number(a).is_some()) {
            continue;
        }
        // Decimal strings avoid an artificial upper bound on imported numbers.
        let highest = main
            .records
            .values()
            .chain(candidate.records.values())
            .flat_map(|r| &r.aliases)
            .filter_map(|a| sequence.number(a))
            .map(|n| n.trim_start_matches('0'))
            .max_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)))
            .unwrap_or("");
        let mut digits = highest.as_bytes().to_vec();
        let mut carry = true;
        for digit in digits.iter_mut().rev() {
            if *digit == b'9' {
                *digit = b'0';
            } else {
                *digit += 1;
                carry = false;
                break;
            }
        }
        if carry {
            digits.insert(0, b'1');
        }
        let number = String::from_utf8(digits)?;
        let alias = format!(
            "{}{:0>width$}",
            sequence.prefix,
            number,
            width = sequence.width
        );
        candidate
            .records
            .get_mut(&id)
            .expect("candidate record")
            .aliases
            .push(alias.clone());
        allocated.insert(id, alias);
    }
    Ok(allocated)
}

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
    pub(crate) fn changes_graph(&self) -> bool {
        self.changes.iter().any(|c| match c {
            Change::Put { table, id, .. } | Change::Insert { table, id, .. } => {
                matches!(
                    table.as_str(),
                    "records" | "edges" | "import_sources" | "import_members"
                ) || (table == "metadata" && id == "ontology")
            }
            Change::DeleteEdge { .. } => true,
        })
    }
    /// Apply typed after-images under the local store lock, avoiding another set
    /// of Dolt processes. Identity checks match graph assembly from stored rows.
    pub(crate) fn candidate(&self, before: &Graph) -> Result<Graph> {
        let mut graph = before.clone();
        for change in &self.changes {
            let (table, id, payload) = match change {
                Change::Put { table, id, payload } | Change::Insert { table, id, payload } => {
                    (table.as_str(), id, payload)
                }
                Change::DeleteEdge { id } => {
                    graph.edges.remove(id);
                    continue;
                }
            };
            match table {
                "records" => {
                    let mut r: Record = serde_json::from_str(payload)?;
                    anyhow::ensure!(
                        r.id == *id,
                        "stored_row_identity: invalid keyed row in records"
                    );
                    r.normalize_metadata();
                    graph.records.insert(id.clone(), r);
                }
                "edges" => {
                    let e: Edge = serde_json::from_str(payload)?;
                    anyhow::ensure!(
                        e.key() == *id,
                        "stored_row_identity: invalid keyed row in edges"
                    );
                    graph.edges.insert(id.clone(), e);
                }
                "metadata" if id == "ontology" => graph.schema = serde_json::from_str(payload)?,
                "import_sources" => {
                    let source: source_import::Source = serde_json::from_str(payload)?;
                    anyhow::ensure!(
                        source.source_id == *id,
                        "source_ownership: invalid keyed row in import_sources"
                    );
                    graph.import_sources.insert(id.clone(), source);
                }
                "import_members" => {
                    let member: source_import::Member = serde_json::from_str(payload)?;
                    anyhow::ensure!(
                        member.row_key() == *id,
                        "source_ownership: invalid keyed row in import_members"
                    );
                    graph
                        .import_members
                        .insert(member.record_id.clone(), member);
                }
                _ => {}
            }
        }
        Ok(graph)
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

/// Inputs for publishing a new proposal. Existing-proposal writes use `commit`.
pub struct ProposalWrite<'a> {
    pub changes: &'a ChangeSet,
    pub message: &'a str,
    pub target: &'a str,
    pub start: Option<&'a str>,
    pub proposal: &'a Proposal,
    pub result: &'a Value,
}

/// The read and write surface of the knowledge engine. Store revisions are opaque;
/// history and inspection payloads have backend-neutral types.
///
/// `at: None` always selects the session's current view, including uncommitted
/// writes in its open transaction. `Some(at)` selects that ref or revision.
/// Rows are `(id, JSON payload)` pairs; their order is supplied by the backend.
///
/// Every write changing records, edges or the ontology MUST call
/// `policy::validate_change` against its destination's before and committed graphs
/// inside the same exclusive lock or transaction. This includes proposal publication,
/// acceptance and recovery. Refusal must preserve all branch heads. Authority and
/// import health checks remain additional caller obligations. Receipt-only writes
/// may skip validation. Only `bootstrap` may establish new import lint debt.
pub trait KnowledgeStore {
    /// The one tenant served by this opened handle. Calls without a separate
    /// tenant argument use this scope. Implementations must check any supplied
    /// operation context against it before accessing store data.
    fn tenant(&self) -> &TenantId;
    /// Prepare aliases for NEW native records when landing on main. The caller
    /// MUST retain the same exclusive lock/transaction from reading `main`
    /// through committing `candidate`, proposal status and the receipt together.
    /// Accept implementations allocate after merging, before full validation;
    /// recovery persists and replays these exact rows, never reallocating.
    /// Preview may use this operation without persisting its candidate. Imports
    /// and proposal writes must not allocate. Existing main records are untouched.
    fn allocate_aliases(&self, main: &Graph, candidate: &mut Graph) -> Result<AliasAllocations> {
        allocate_aliases(main, candidate)
    }
    /// Select main or a public proposal as the session's current view.
    fn select_branch(&self, branch: &str) -> Result<()>;
    /// Preserve the shared receipt/recovery envelope for an importer response.
    fn receipt_response(&self, data: Value, branch: &str) -> Result<Value>;
    fn check_import_reservation(&self, source: &str, op: &str) -> Result<()>;
    fn check_identity(&self, op: &str, hash: &str) -> Result<()>;
    fn check_bootstrap_ready(&self) -> Result<()>;
    /// Size of this backend's encoded atomic request, before transport limits.
    fn encoded_request_size(&self, changes: &ChangeSet) -> usize;
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
    /// Initial legacy or Markdown import into an empty store. Validate with exactly
    /// the imported graph's lint as debt, recorded in the same atomic write. The
    /// shared backend uses its Markdown bulk transport; no other write can add debt.
    fn bootstrap(
        &self,
        changes: &ChangeSet,
        message: &str,
        actor: &str,
        validate: &mut dyn FnMut() -> Result<()>,
    ) -> Result<String>;
    fn propose(&self, context: &OperationContext, write: ProposalWrite<'_>) -> Result<Value>;
    fn reject(
        &self,
        context: &OperationContext,
        proposal: &Proposal,
        result: &Value,
    ) -> Result<Value>;
    fn accept(
        &self,
        context: &OperationContext,
        main: &Graph,
        proposal: &Proposal,
        revisions: (&str, &str),
        validate: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Value>;
    fn recover(&self, context: &OperationContext) -> Result<Value>;
    fn resume(
        &self,
        context: &OperationContext,
        main: &Graph,
        op: &str,
        hash: &str,
    ) -> Result<Option<Value>>;
    fn receipt(&self, op: &str, hash: &str) -> Result<Option<Value>>;
    /// Resolve a ref or a full immutable revision to its canonical revision.
    fn head(&self, reference: &str) -> Result<String>;
    fn revision(&self) -> Result<String>;
    fn branches(&self) -> Result<Vec<String>>;
    /// Revision from which the proposal's record changes are inspected. Dolt
    /// resolves the common ancestor; a change-set store returns its proposal base.
    fn base_revision(&self, main: &str, proposal: &str) -> Result<String>;
    fn rows(&self, table: &str, at: Option<&str>) -> Result<Vec<(String, String)>>;
    fn one(&self, table: &str, id: &str, at: Option<&str>) -> Result<Option<String>>;
    fn proposals(&self) -> Result<Vec<Proposal>>;
    fn history(&self, at: &str, id: &str) -> Result<Vec<HistoryEntry>>;
    fn log(&self, at: &str) -> Result<Vec<LogEntry>>;
    /// Proposal changes in row-id order. Snapshot stores compare main to the
    /// proposal head; change-set stores override this to list their own changes
    /// (ADR-0280 decision 5). Both return decoded before/after images.
    fn diff(&self, main: &str, proposal: &str, table: &str) -> Result<Vec<RowChange>> {
        anyhow::ensure!(
            matches!(table, "edges" | "metadata"),
            "unsupported inspection table"
        );
        RowChange::between(
            table,
            self.rows(table, Some(main))?,
            self.rows(table, Some(proposal))?,
        )
    }
    fn request_limit(&self) -> Option<usize>;
    fn shared(&self) -> bool;
}

pub(crate) fn one<T: DeserializeOwned>(
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

pub(crate) fn keyed_rows<T: DeserializeOwned>(
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

/// Read a graph for the requested tenant, at the selected view or an exact revision.
pub fn graph(store: &dyn KnowledgeStore, tenant: &TenantId, at: Option<&str>) -> Result<Graph> {
    tenant.check_served_by(store.tenant())?;
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
    use std::{cell::Cell, collections::BTreeMap};

    struct Fixture {
        tenant: TenantId,
        reads: Cell<u32>,
        writes: Cell<u32>,
        at: Option<&'static str>,
        format: Option<u32>,
        tables: BTreeMap<String, Vec<(String, String)>>,
    }
    impl KnowledgeStore for Fixture {
        fn tenant(&self) -> &TenantId {
            &self.tenant
        }
        fn select_branch(&self, _: &str) -> Result<()> {
            anyhow::bail!("unexpected branch selection")
        }
        fn receipt_response(&self, _: Value, _: &str) -> Result<Value> {
            anyhow::bail!("unexpected importer response")
        }
        fn check_import_reservation(&self, _: &str, _: &str) -> Result<()> {
            anyhow::bail!("unexpected import reservation")
        }
        fn check_identity(&self, _: &str, _: &str) -> Result<()> {
            anyhow::bail!("unexpected operation identity")
        }
        fn check_bootstrap_ready(&self) -> Result<()> {
            anyhow::bail!("unexpected bootstrap check")
        }
        fn encoded_request_size(&self, _: &ChangeSet) -> usize {
            panic!("unexpected request encoding")
        }
        fn commit(&self, _: &ChangeSet, _: &str, _: &str) -> Result<()> {
            self.writes.set(self.writes.get() + 1);
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
        fn bootstrap(
            &self,
            _: &ChangeSet,
            _: &str,
            _: &str,
            _: &mut dyn FnMut() -> Result<()>,
        ) -> Result<String> {
            anyhow::bail!("unexpected Markdown commit")
        }
        fn propose(&self, _: &OperationContext, _: ProposalWrite<'_>) -> Result<Value> {
            anyhow::bail!("unexpected proposal publication")
        }
        fn reject(&self, _: &OperationContext, _: &Proposal, _: &Value) -> Result<Value> {
            anyhow::bail!("unexpected proposal rejection")
        }
        fn accept(
            &self,
            _: &OperationContext,
            _: &Graph,
            _: &Proposal,
            _: (&str, &str),
            _: &mut dyn FnMut() -> Result<()>,
        ) -> Result<Value> {
            anyhow::bail!("unexpected proposal acceptance")
        }
        fn recover(&self, _: &OperationContext) -> Result<Value> {
            anyhow::bail!("unexpected recovery")
        }
        fn resume(
            &self,
            _: &OperationContext,
            _: &Graph,
            _: &str,
            _: &str,
        ) -> Result<Option<Value>> {
            anyhow::bail!("unexpected operation resume")
        }
        fn receipt(&self, _: &str, _: &str) -> Result<Option<Value>> {
            anyhow::bail!("unexpected receipt read")
        }
        fn rows(&self, table: &str, at: Option<&str>) -> Result<Vec<(String, String)>> {
            self.reads.set(self.reads.get() + 1);
            assert_eq!(at, self.at);
            Ok(self.tables.get(table).cloned().unwrap_or_default())
        }
        fn one(&self, table: &str, id: &str, at: Option<&str>) -> Result<Option<String>> {
            self.reads.set(self.reads.get() + 1);
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
        fn base_revision(&self, _: &str, _: &str) -> Result<String> {
            anyhow::bail!("unexpected merge-base read")
        }
        fn proposals(&self) -> Result<Vec<Proposal>> {
            anyhow::bail!("unexpected proposal read")
        }
        fn history(&self, _: &str, _: &str) -> Result<Vec<HistoryEntry>> {
            anyhow::bail!("unexpected history read")
        }
        fn log(&self, _: &str) -> Result<Vec<LogEntry>> {
            anyhow::bail!("unexpected log read")
        }
        fn diff(&self, _: &str, _: &str, _: &str) -> Result<Vec<RowChange>> {
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
    fn tenant_id_uses_the_documented_ascii_grammar() -> Result<()> {
        assert_eq!("default".parse::<TenantId>()?, DEFAULT_TENANT);
        for valid in ["t", "1", "t1-tenant", &"a".repeat(63)] {
            assert_eq!(valid.parse::<TenantId>()?.as_str(), valid);
        }
        for invalid in ["", "-t1", "T1", "t.1", "t_1", "t 1", "é", &"a".repeat(64)] {
            assert!(invalid.parse::<TenantId>().is_err(), "{invalid:?}");
        }
        Ok(())
    }

    #[test]
    fn graph_checks_operation_tenant_before_accessing_the_store() -> Result<()> {
        let tenant: TenantId = "t1".parse()?;
        let fixture = Fixture {
            tenant: tenant.clone(),
            reads: Cell::default(),
            writes: Cell::default(),
            at: None,
            format: Some(4),
            tables: BTreeMap::new(),
        };

        assert!(graph(&fixture, &tenant, None)?.records.is_empty());
        assert!(fixture.reads.get() > 0);
        let reads = fixture.reads.get();
        assert_eq!(
            graph(&fixture, &DEFAULT_TENANT, None)
                .unwrap_err()
                .to_string(),
            "tenant_mismatch: operation tenant default differs from store tenant t1"
        );
        assert_eq!(fixture.reads.get(), reads);
        assert_eq!(fixture.writes.get(), 0);
        Ok(())
    }

    #[test]
    fn graph_preserves_selected_view_and_normalizes_legacy_metadata() -> Result<()> {
        let mut record = Record::blank("decision", "Read view", "", "reader");
        record.original = Some(serde_json::json!({}));
        record.aliases.push("ADR-0280".into());
        for at in [None, Some("pinned-revision")] {
            let fixture = Fixture {
                tenant: DEFAULT_TENANT,
                reads: Cell::default(),
                writes: Cell::default(),
                at,
                format: None,
                tables: BTreeMap::from([(
                    "records".into(),
                    vec![(record.id.clone(), serde_json::to_string(&record)?)],
                )]),
            };
            let g = graph(&fixture, &DEFAULT_TENANT, at)?;
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
            tenant: DEFAULT_TENANT,
            reads: Cell::default(),
            writes: Cell::default(),
            at: None,
            format: Some(4),
            tables: BTreeMap::from([(
                "records".into(),
                vec![("wrong-key".into(), serde_json::to_string(&record)?)],
            )]),
        };
        assert!(
            graph(&fixture, &DEFAULT_TENANT, None)
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
            graph(&fixture, &DEFAULT_TENANT, None)
                .unwrap_err()
                .to_string()
                .contains("source_ownership: invalid membership")
        );
        Ok(())
    }
    #[test]
    fn alias_allocation_follows_highest_existing_number() -> Result<()> {
        let fixture = Fixture {
            tenant: DEFAULT_TENANT,
            reads: Cell::default(),
            writes: Cell::default(),
            at: None,
            format: None,
            tables: BTreeMap::new(),
        };
        let mut main = Graph::default();
        main.schema
            .kinds
            .get_mut("decision")
            .unwrap()
            .alias_sequence = Some(crate::schema::AliasSequence {
            prefix: "ADR-".into(),
            width: 4,
        });
        let add = |graph: &mut Graph, id: &str, aliases: &[&str]| {
            let mut r = Record::blank("decision", id, "", "alice");
            r.id = id.into();
            r.aliases = aliases.iter().map(|s| (*s).into()).collect();
            graph.records.insert(id.into(), r);
        };
        let mut candidate = main.clone();
        add(&mut candidate, "sc-first", &[]);
        assert_eq!(
            fixture.allocate_aliases(&main, &mut candidate)?["sc-first"],
            "ADR-0001"
        );
        add(
            &mut main,
            "sc-held",
            &["ADR-0007", "ADR-0280", "src:ADR-0999", "ADR-12a"],
        );
        add(&mut main, "sc-duplicate", &["ADR-0280"]);
        add(&mut main, "sc-old-unnumbered", &[]);
        let mut candidate = main.clone();
        add(&mut candidate, "sc-b", &[]);
        add(&mut candidate, "sc-a", &[]);
        add(&mut candidate, "sc-already", &["ADR-0003"]);
        add(&mut candidate, "sc-imported", &[]);
        candidate.records.get_mut("sc-imported").unwrap().original = Some(serde_json::json!({}));
        let allocated = fixture.allocate_aliases(&main, &mut candidate)?;
        assert_eq!(
            allocated,
            BTreeMap::from([
                ("sc-a".into(), "ADR-0281".into()),
                ("sc-b".into(), "ADR-0282".into())
            ])
        );
        assert!(candidate.records["sc-old-unnumbered"].aliases.is_empty());
        assert!(candidate.records["sc-imported"].aliases.is_empty());
        assert_eq!(candidate.records["sc-already"].aliases, ["ADR-0003"]);
        assert!(main.resolve("ADR-0280").is_err());
        add(&mut main, "sc-high", &["ADR-9999"]);
        let mut candidate = main.clone();
        add(&mut candidate, "sc-new", &[]);
        assert_eq!(
            fixture.allocate_aliases(&main, &mut candidate)?["sc-new"],
            "ADR-10000"
        );
        add(
            &mut main,
            "sc-huge",
            &["ADR-999999999999999999999999999999"],
        );
        let mut candidate = main.clone();
        add(&mut candidate, "sc-new", &[]);
        assert_eq!(
            fixture.allocate_aliases(&main, &mut candidate)?["sc-new"],
            "ADR-1000000000000000000000000000000"
        );
        Ok(())
    }
}
