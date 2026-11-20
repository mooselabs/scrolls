//! Typed knowledge reads shared by CLI and HTTP. No audit, recovery or mutation.
use crate::{
    model::*,
    query::{self, Filters},
    schema::Direction,
    store::{self, KnowledgeStore},
    work_ref::Adapter,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub enum Operation<'a> {
    Schema,
    Graph,
    Records {
        filters: &'a Filters,
        terms: Option<&'a str>,
        limit: usize,
    },
    Document {
        id: &'a str,
        provenance: bool,
    },
    Context {
        id: &'a str,
        budget: usize,
    },
    Walk {
        id: &'a str,
        rel: Option<&'a str>,
        depth: usize,
        direction: Direction,
        full: bool,
    },
    Prime {
        budget: usize,
    },
    Query {
        expression: &'a query::Expr,
        actor: &'a str,
        paths: &'a [String],
        at: Option<&'a str>,
    },
}

pub struct View<'a> {
    pub graph: &'a Graph,
    pub routing: &'a Value,
    pub work: &'a dyn Adapter,
    pub store_id: Option<&'a str>,
    pub actor: &'a str,
}

pub(crate) struct LabelledDocument {
    pub id: String,
    pub revision: String,
    pub graph: Graph,
}

impl View<'_> {
    pub fn resolve(&self, name: &str) -> Result<String> {
        let name = if let Some(uri) = name.strip_prefix("scrolls://") {
            let (store, record) = uri.split_once('/').context("invalid Scrolls URI")?;
            ensure!(
                Some(store) == self.store_id,
                "Scrolls store identity mismatch"
            );
            record
        } else {
            name
        };
        self.graph.resolve(name)
    }
    pub(crate) fn at_label(
        &self,
        store: &dyn KnowledgeStore,
        at: &str,
        record: &str,
        name: &str,
    ) -> Result<LabelledDocument> {
        let id = self.resolve(record)?;
        let revision = crate::label::resolve(store, &id, name, at)?;
        let graph = store::graph(store, Some(&revision))?;
        ensure!(
            graph.records.contains_key(&id),
            "label_target: record {id} label {name:?} has no record at revision {revision}"
        );
        Ok(LabelledDocument {
            id,
            revision,
            graph,
        })
    }

    pub fn execute(&self, operation: Operation<'_>) -> Result<Value> {
        let g = self.graph;
        match operation {
            Operation::Graph => {
                // Keep graph keys and all ordinary record fields, but never
                // transmit captured originals or source-file payloads.
                let records = g
                    .records
                    .iter()
                    .map(|(id, record)| {
                        let mut value = serde_json::to_value(record)?;
                        value
                            .as_object_mut()
                            .expect("record object")
                            .remove("original");
                        Ok((id.clone(), value))
                    })
                    .collect::<Result<serde_json::Map<String, Value>>>()?;
                Ok(json!({"records": records, "edges": g.edges}))
            }
            Operation::Schema => {
                Ok(json!({"schema":g.schema,"schema_revision":g.schema.revision()}))
            }
            Operation::Records {
                filters,
                terms,
                limit,
            } => Ok(json!(query::select(g, filters, terms, limit, self.actor)?)),
            Operation::Document { id, provenance } => {
                let id = self.resolve(id)?;
                Ok(
                    json!({"uri":self.store_id.map(|s|crate::work_ref::document_uri(s,&id)),"record":g.records[&id],"references":query::references(g,&id),"edges":g.edges.values().filter(|e|e.from==id||e.to==id).collect::<Vec<_>>(),"related_work":crate::work_ref::related(g,&id,self.work),"provenance":if provenance {crate::source_import::provenance(g,&id)} else {Value::Null}}),
                )
            }
            Operation::Context { id, budget } => query::context(g, &self.resolve(id)?, budget),
            Operation::Walk {
                id,
                rel,
                depth,
                direction,
                full,
            } => query::traverse(g, &self.resolve(id)?, rel, depth, direction, full),
            Operation::Prime { budget } => query::prime(g, budget),
            Operation::Query {
                expression,
                actor,
                paths,
                at,
            } => query::evaluate_expression(g, self.routing, expression, actor, paths, at),
        }
    }
}

/// Proposal inspection uses immutable revisions and never checks out a branch.
/// Both transports call this while holding their operation lock.
pub fn proposal_detail(store: &dyn KnowledgeStore, name: &str) -> Result<Value> {
    let proposal = store
        .proposals()?
        .into_iter()
        .find(|p| p.branch == name)
        .context("unknown proposal")?;
    let main = store.head("main")?;
    let revision = store.head(name)?;
    let ancestor = store.base_revision(&main, &revision)?;
    let proposed = store::graph(store, Some(&revision))?;
    crate::markdown_import::health(store, &store::graph(store, Some(&main))?, Some(&main))?;
    crate::markdown_import::health(store, &proposed, Some(&revision))?;
    let old = store::graph(store, Some(&ancestor))?;
    let ids: BTreeSet<_> = old.records.keys().chain(proposed.records.keys()).collect();
    let changes: Vec<_> = ids
        .into_iter()
        .filter(|id| old.records.get(*id) != proposed.records.get(*id))
        .map(|id| json!({"id":id,"before":old.records.get(id),"after":proposed.records.get(id)}))
        .collect();
    let review_token = crate::review_token::ReviewToken::issue(&proposal.branch, &main, &revision)?;
    Ok(
        json!({"proposal":proposal,"main_revision":main,"proposal_revision":revision,"review_token":review_token,"base_revision":ancestor,"records":changes,"graph_changes":store.diff(&main,&revision,"edges")?,"configuration_changes":store.diff(&main,&revision,"metadata")?,"imports":crate::source_import::summary(&proposed,&store::import_files(store, Some(&revision))?)}),
    )
}

/// Collection rows include their immutable proposal-head revision without
/// changing the persisted proposal metadata.
pub fn proposals(store: &dyn KnowledgeStore) -> Result<Vec<Value>> {
    store
        .proposals()?
        .into_iter()
        .map(|proposal| {
            let revision = store.head(&proposal.branch)?;
            let mut value = json!(proposal);
            value["store_revision"] = json!(revision);
            Ok(value)
        })
        .collect()
}

pub fn history(store: &dyn KnowledgeStore, revision: &str, id: Option<&str>) -> Result<Vec<Value>> {
    if let Some(id) = id {
        store
            .history(revision, id)?
            .into_iter()
            .map(|entry| Ok(serde_json::to_value(entry)?))
            .collect()
    } else {
        store
            .log(revision)?
            .into_iter()
            .map(|entry| Ok(serde_json::to_value(entry)?))
            .collect()
    }
}

/// Backend failures safe to expose through the stable read transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendFailure {
    Unavailable,
    Busy,
    Conflict,
}

/// A host's read-snapshot factory. No concrete backend types cross this seam.
/// `open` pins the selected view, including its graph, configuration and work
/// cache. Reads through the handle must remain consistent until snapshot drop.
/// Backend-specific errors from later handle reads are classified here too.
pub trait SnapshotSource: Send + Sync + 'static {
    fn open(&self) -> std::result::Result<Snapshot, BackendFailure>;
    fn classify(&self, error: anyhow::Error) -> BackendFailure;
}

/// Owns one operation's immutable read view and any required lock/session.
pub struct Snapshot {
    pub store: Box<dyn KnowledgeStore>,
    pub graph: Graph,
    pub work: Box<dyn Adapter>,
    pub routing: Value,
    pub store_id: String,
    pub selection: String,
    pub revision: String,
    pub actor: String,
}
impl Snapshot {
    pub fn view(&self) -> View<'_> {
        View {
            graph: &self.graph,
            routing: &self.routing,
            work: self.work.as_ref(),
            store_id: Some(&self.store_id),
            actor: &self.actor,
        }
    }
    pub fn envelope(&self, data: Value) -> Value {
        json!({"schema_version":3,"ok":true,"store_id":self.store_id,"view":self.selection,"store_revision":self.revision,"schema_revision":self.graph.schema.revision(),"data":data})
    }
}
