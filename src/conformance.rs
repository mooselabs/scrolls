//! Backend-neutral read-contract proof, run through the public HTTP router.
use crate::{
    http::{OPENAPI, router},
    model::{Edge, Graph, Proposal, Record},
    read::SnapshotSource,
    store::{Change, ChangeSet, KnowledgeStore},
};
use anyhow::{Context, Result, ensure};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tower::ServiceExt;

pub const RECORD: &str = "sc-conformance-a";
pub const PROPOSAL: &str = "proposal/conformance";

/// Fixed after-images for a fresh initialized backend. Backend-owned identity
/// and format metadata are deliberately left to the host.
pub fn fixture() -> Result<ChangeSet> {
    let mut graph = Graph::default();
    for (id, title) in [
        (RECORD, "Read contract"),
        ("sc-conformance-b", "Related note"),
    ] {
        let mut record = Record::new("note", title, "Portable fixture", "reader")?;
        record.id = id.into();
        record.created = "2026-01-01T00:00:00Z".into();
        record.last_reviewed = "2026-01-01T00:00:00Z".into();
        graph.records.insert(id.into(), record);
    }
    let edge = Edge {
        from: RECORD.into(),
        rel: "related".into(),
        to: "sc-conformance-b".into(),
        why: "Conformance".into(),
        actor: "reader".into(),
        created: "2026-01-01T00:00:00Z".into(),
    };
    graph.edges.insert(edge.key(), edge);
    let mut changes = ChangeSet::graph(&Graph::default(), &graph)?;
    changes.push(Change::put("metadata", "ontology", &graph.schema)?);
    changes.push(Change::put("metadata", "routing", &json!({}))?);
    changes.push(Change::put(
        "metadata",
        PROPOSAL,
        &Proposal {
            branch: PROPOSAL.into(),
            base: "main".into(),
            actor: "reader".into(),
            status: "pending".into(),
            reason: None,
        },
    )?);
    Ok(changes)
}

/// Seed through the same typed commit seam used by engine callers. The host
/// also exposes `proposal/conformance` at the seeded revision.
pub fn seed(store: &dyn KnowledgeStore) -> Result<()> {
    store.commit(&fixture()?, "Read conformance fixture", "reader")
}

/// Row-read payloads normalized only for the backend-owned store identity.
#[derive(Debug, PartialEq)]
pub struct Report(pub BTreeMap<String, Value>);

fn normalize(value: &mut Value, store_id: &str) {
    match value {
        Value::String(s) => {
            if let Some(id) = s.strip_prefix(&format!("scrolls://{store_id}/")) {
                *s = format!("scrolls://STORE/{id}");
            }
        }
        Value::Array(values) => values.iter_mut().for_each(|v| normalize(v, store_id)),
        Value::Object(values) => values.values_mut().for_each(|v| normalize(v, store_id)),
        _ => {}
    }
}

/// Run every route against an already seeded backend without opening a socket.
/// Compare reports from different backends for row-read parity.
pub async fn check(source: Arc<dyn SnapshotSource>) -> Result<Report> {
    let address = "127.0.0.1:18765".parse()?;
    let app = router(source, address)?;
    let spec: Value = serde_json::from_str(OPENAPI)?;
    let mut report = BTreeMap::new();
    let routes = [
        ("/healthz".to_string(), false),
        ("/readyz".into(), false),
        ("/v0/context".into(), false),
        ("/v0/schema".into(), true),
        ("/v0/graph".into(), true),
        ("/v0/records".into(), true),
        (format!("/v0/records/{RECORD}"), true),
        (format!("/v0/records/{RECORD}/context"), true),
        (format!("/v0/records/{RECORD}/walk?full=true"), true),
        ("/v0/prime".into(), true),
        ("/v0/query".into(), true),
        ("/v0/proposals".into(), false),
        (format!("/v0/proposals/{PROPOSAL}"), false),
        ("/v0/history".into(), false),
        (format!("/v0/history?id={RECORD}"), false),
        ("/v0/openapi.json".into(), false),
    ];
    for (path, parity) in routes {
        let query = path == "/v0/query";
        let request = Request::builder().method(if query {"POST"} else {"GET"})
            .uri(&path).header("host", address.to_string())
            .header("content-type", "application/json")
            .body(if query {Body::from(json!({"expression":{"op":"compare","field":"owner","cmp":"eq","value":"$actor"},"at":"2026-11-01T00:00:00Z"}).to_string())} else {Body::empty()})?;
        let response = app.clone().oneshot(request).await?;
        ensure!(
            response.status() == StatusCode::OK,
            "{path}: {}",
            response.status()
        );
        let etag = response
            .headers()
            .get("etag")
            .context("missing ETag")?
            .clone();
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        let value: Value = serde_json::from_slice(&bytes)?;
        if path == "/v0/openapi.json" {
            ensure!(
                bytes.as_ref() == serde_json::to_vec(&spec)?,
                "OpenAPI differs"
            );
            ensure!(value == spec, "OpenAPI differs");
        } else if path != "/healthz" {
            let template = path
                .split('?')
                .next()
                .unwrap()
                .replace(RECORD, "{id}")
                .replace(PROPOSAL, "{id}");
            let method = if query { "post" } else { "get" };
            let required =
                spec["paths"][&template][method]["responses"]["200"]["content"]["application/json"]
                    ["schema"]["required"]
                    .as_array()
                    .context("envelope required fields")?;
            let expected: BTreeSet<_> = required.iter().filter_map(Value::as_str).collect();
            let actual: BTreeSet<_> = value
                .as_object()
                .context("envelope object")?
                .keys()
                .map(String::as_str)
                .collect();
            ensure!(actual == expected, "{path}: envelope fields differ");
            ensure!(
                value["schema_version"] == 3 && value["ok"] == true,
                "{path}: envelope version"
            );
            ensure!(
                value["store_revision"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty()),
                "{path}: empty revision"
            );
            ensure!(
                !actual.iter().any(|k| ["branch", "commit", "hash", "dolt"]
                    .iter()
                    .any(|b| k.to_lowercase().contains(b))),
                "backend field outside data"
            );
            if path == "/v0/context" {
                ensure!(value["data"]["view"] == value["view"], "discovery view");
            }
            if parity {
                let mut data = value["data"].clone();
                normalize(
                    &mut data,
                    value["store_id"].as_str().context("store identity")?,
                );
                report.insert(path.clone(), data);
            }
        }
        if !query {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(&path)
                        .header("host", address.to_string())
                        .header("if-none-match", etag)
                        .body(Body::empty())?,
                )
                .await?;
            ensure!(
                response.status() == StatusCode::NOT_MODIFIED,
                "{path}: conditional read"
            );
        }
    }
    for (header, value, code) in [
        ("host", "example.org", "host_rejected"),
        ("origin", "http://example.org", "origin_rejected"),
    ] {
        let mut request = Request::builder()
            .uri("/v0/context")
            .header("host", address.to_string())
            .body(Body::empty())?;
        request.headers_mut().insert(header, value.parse()?);
        let response = app.clone().oneshot(request).await?;
        ensure!(
            response.status() == StatusCode::BAD_REQUEST,
            "security status"
        );
        let value: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await?)?;
        ensure!(
            value
                == json!({"schema_version":3,"ok":false,"error":{"code":code,"message":if code == "host_rejected" {"Host must match the loopback listener"} else {"Browser Origin requests are disabled"}}}),
            "security envelope"
        );
    }
    Ok(Report(report))
}
