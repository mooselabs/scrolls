//! Typed knowledge reads shared by CLI and HTTP. No audit, recovery or mutation.
use crate::{beads::Local, cli::Filters, model::*, query, schema::Direction, storage::Store};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::PathBuf, time::Duration};

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

#[derive(Clone)]
pub struct Config {
    pub store: PathBuf,
    pub dolt: PathBuf,
    pub connection: Option<PathBuf>,
    pub branch: String,
    pub actor: String,
}
impl Config {
    pub fn from_cli(cli: &crate::cli::Cli) -> Self {
        Self {
            store: cli.store.clone(),
            dolt: cli.dolt.clone(),
            connection: cli.connection.clone(),
            branch: cli.proposal.clone().unwrap_or_else(|| "main".into()),
            actor: cli.actor.clone().unwrap_or_else(|| "reader".into()),
        }
    }
}

/// Owns the operation's lock/session. Every application row is read at an
/// immutable commit; dropping this value releases admission to CLI writers.
pub struct Snapshot {
    pub store: Store,
    pub graph: Graph,
    pub local: Local,
    pub routing: Value,
    pub store_id: String,
    pub branch: String,
    pub revision: String,
    pub actor: String,
}
impl Snapshot {
    pub fn open(config: &Config) -> Result<Self> {
        let store = if let Some(path) = &config.connection {
            Store::open_shared(path, &config.store)?
        } else {
            Store::open_readonly_wait(&config.store, &config.dolt, Some(Duration::from_secs(5)))?
        };
        if store.is_shared() {
            crate::storage::shared_review::check_readable(&store)?;
        }
        ensure!(
            config.branch == "main" || config.branch.starts_with("proposal/"),
            "unknown selected proposal"
        );
        ensure!(
            store.branches()?.contains(&config.branch),
            "unknown selected proposal"
        );
        let revision = store.head(&config.branch)?;
        let format = store.one_at::<u32>("metadata", "schema", &revision)?;
        ensure!(
            format == Some(if store.is_shared() { 5 } else { 4 }),
            "unsupported store format; explicit migration required"
        );
        if store.is_shared() {
            store.check_versioned_inventory(&revision)?;
        }
        let store_id: String = store
            .one_at("metadata", "store_id", &revision)?
            .context("missing store identity")?;
        uuid::Uuid::parse_str(&store_id)?;
        let graph = store.graph_at(&revision)?;
        graph.schema.validate()?;
        let local = if store.is_shared() {
            Local::default()
        } else {
            Local::load(&store.root)?
        };
        let routing = store
            .one_at("metadata", "routing", &revision)?
            .unwrap_or(json!({}));
        Ok(Self {
            store,
            graph,
            local,
            routing,
            store_id,
            branch: config.branch.clone(),
            revision,
            actor: config.actor.clone(),
        })
    }
    pub fn view(&self) -> View<'_> {
        View {
            graph: &self.graph,
            routing: &self.routing,
            local: &self.local,
            store_id: Some(&self.store_id),
            actor: &self.actor,
        }
    }
    pub fn envelope(&self, data: Value) -> Value {
        json!({"schema_version":2,"ok":true,"store_id":self.store_id,"branch":self.branch,"revision":self.revision,"schema_revision":self.graph.schema.revision(),"data":data})
    }
}

/// Proposal inspection uses immutable revisions and never checks out a branch.
/// Both transports call this while holding their operation lock.
pub fn proposal_detail(store: &Store, name: &str) -> Result<Value> {
    let proposal = store
        .proposals()?
        .into_iter()
        .find(|p| p.branch == name)
        .context("unknown proposal")?;
    let main = store.head("main")?;
    let revision = store.head(name)?;
    let ancestor = store.merge_base(&main, &revision)?;
    let proposed = store.graph_at(&revision)?;
    crate::markdown_import::health(store, &store.graph_at(&main)?, Some(&main))?;
    crate::markdown_import::health(store, &proposed, Some(&revision))?;
    let old = store.graph_at(&ancestor)?;
    let ids: BTreeSet<_> = old.records.keys().chain(proposed.records.keys()).collect();
    let changes: Vec<_> = ids
        .into_iter()
        .filter(|id| old.records.get(*id) != proposed.records.get(*id))
        .map(|id| json!({"id":id,"before":old.records.get(id),"after":proposed.records.get(id)}))
        .collect();
    Ok(
        json!({"proposal":proposal,"main_revision":main,"proposal_revision":revision,"review_token":crate::review_token::ReviewToken::new(name,&main,&revision).encode()?,"merge_base":ancestor,"records":changes,"graph_diff":store.diff_text(&main,&revision,"edges")?,"configuration_diff":store.diff_text(&main,&revision,"metadata")?,"imports":crate::source_import::summary(&proposed,&store.import_files_at(&revision)?)}),
    )
}

pub fn history(store: &Store, revision: &str, id: Option<&str>) -> Result<Vec<Value>> {
    if let Some(id) = id {
        store.sql(&format!("SELECT commit_hash,committer,commit_date,payload FROM dolt_history_records AS OF {} WHERE id={} ORDER BY commit_date,commit_hash,payload",crate::storage::revision_literal(revision),crate::storage::literal(id)))
    } else {
        store.sql(&format!(
            "SELECT * FROM DOLT_LOG({}) ORDER BY date,commit_hash",
            crate::storage::literal(revision)
        ))
    }
}
