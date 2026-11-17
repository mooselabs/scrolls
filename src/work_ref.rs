//! Work identities supplied by the host. Local aliases never enter the graph.
use crate::{model::Graph, schema::Schema};
use anyhow::Result;
use serde_json::{Value, json};

/// The work-reference seam used by validation, mutations and reads.
///
/// All methods are offline: implementations must not start processes or make
/// network calls. A claimed malformed reference is `Some(Err(_))`, never `None`.
/// Canonicalization must be idempotent; stored targets must already be canonical.
pub trait Adapter {
    /// Claim canonical references and portable aliases, returning their canonical
    /// form. `None` means this adapter does not own the reference's syntax.
    fn canonicalize(&self, reference: &str) -> Option<Result<String>>;

    /// Resolve caller-local short forms at link time only. Return `None` for
    /// inputs outside the adapter's local syntax. Short forms are never stored.
    fn resolve_local(&self, input: &str) -> Option<Result<String>>;

    /// Recognise work targets in Markdown links, canonicalizing aliases. Keep
    /// malformed claimed targets so structural validation can report them.
    fn body_references(&self, body: &str) -> Vec<String> {
        let links = regex::Regex::new(r"\]\(([^)\s]+)\)").expect("link target pattern");
        links
            .captures_iter(body)
            .filter_map(|cap| {
                self.canonicalize(&cap[1])
                    .map(|result| result.unwrap_or_else(|_| cap[1].into()))
            })
            .collect()
    }

    /// Return an adapter-defined cached summary of a canonical reference.
    fn summary(&self, reference: &str) -> Value;
}

/// The ontology, rather than a fixed relation-name list, owns work relations.
pub fn relation(schema: &Schema, rel: &str) -> bool {
    schema
        .relations
        .get(rel)
        .is_some_and(|r| r.external.iter().any(|t| t == schema.work_target()))
}

/// Keep the standalone read envelope stable; the summary payload is adapter-defined.
pub fn related(g: &Graph, id: &str, work: &dyn Adapter) -> Vec<Value> {
    g.edges
        .values()
        .filter(|e| e.from == id && relation(&g.schema, &e.rel))
        .filter(|e| {
            work.canonicalize(&e.to)
                .is_some_and(|r| r.is_ok_and(|s| s == e.to))
        })
        .map(|e| json!({"relation":e.rel,"reference":e.to,"bead":work.summary(&e.to)}))
        .collect()
}

pub fn document_uri(store: &str, id: &str) -> String {
    format!("scrolls://{store}/{id}")
}
