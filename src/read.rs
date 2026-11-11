//! Typed knowledge reads shared by CLI and HTTP. No audit, recovery or mutation.
use crate::{
    beads::Local,
    model::*,
    query::{self, Filters},
    schema::Direction,
    store::{self, KnowledgeStore},
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
    pub local: &'a Local,
    pub store_id: Option<&'a str>,
    pub actor: &'a str,
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
                    json!({"uri":self.store_id.map(|s|crate::work_ref::document_uri(s,&id)),"record":g.records[&id],"references":query::references(g,&id),"edges":g.edges.values().filter(|e|e.from==id||e.to==id).collect::<Vec<_>>(),"related_work":crate::beads::related(g,&id,self.local),"provenance":if provenance {crate::source_import::provenance(g,&id)} else {Value::Null}}),
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
    let ancestor = store.merge_base(&main, &revision)?;
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
    Ok(
        json!({"proposal":proposal,"main_revision":main,"proposal_revision":revision,"review_token":crate::review_token::ReviewToken::new(name,&main,&revision).encode()?,"merge_base":ancestor,"records":changes,"graph_diff":store.diff(&main,&revision,"edges")?,"configuration_diff":store.diff(&main,&revision,"metadata")?,"imports":crate::source_import::summary(&proposed,&store::import_files(store, Some(&revision))?)}),
    )
}

pub fn history(store: &dyn KnowledgeStore, revision: &str, id: Option<&str>) -> Result<Vec<Value>> {
    if let Some(id) = id {
        store.history(revision, id)
    } else {
        store.log(revision)
    }
}
