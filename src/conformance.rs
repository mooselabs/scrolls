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

/// A proposal that changes a record, ontology, existing edge and a new edge.
/// Hosts commit this on PROPOSAL after exposing the seeded main revision there.
pub fn proposal_fixture(store: &dyn KnowledgeStore) -> Result<ChangeSet> {
    let mut graph = crate::store::graph(store, Some(&store.head("main")?))?;
    let record = graph.records.get_mut(RECORD).context("fixture record")?;
    record.body = "Proposed portable fixture".into();
    record.revision += 1;
    graph.schema.instructions = "Proposed review instructions".into();
    let mut changes = ChangeSet::default();
    changes.push(Change::put("records", RECORD, record)?);
    changes.push(Change::put("metadata", "ontology", &graph.schema)?);
    let edge = graph.edges.values_mut().next().context("fixture edge")?;
    edge.why = "Proposed relationship".into();
    changes.push(Change::put("edges", &edge.key(), edge)?);
    let mut added = edge.clone();
    std::mem::swap(&mut added.from, &mut added.to);
    changes.push(Change::put("edges", &added.key(), &added)?);
    Ok(changes)
}

// Validate the OpenAPI JSON Schema vocabulary used by the inspection payloads.
// Refuse unknown keywords so adding a constraint cannot silently weaken proof.
fn validate_schema(spec: &Value, schema: &Value, value: &Value) -> Result<()> {
    for key in schema.as_object().context("schema object")?.keys() {
        ensure!(
            [
                "$ref",
                "type",
                "oneOf",
                "required",
                "properties",
                "items",
                "additionalProperties",
                "const",
                "enum",
                "minLength",
                "minimum",
                "format",
                "description"
            ]
            .contains(&key.as_str()),
            "unsupported contract keyword {key}"
        );
    }
    if let Some(reference) = schema["$ref"].as_str() {
        let target = spec
            .pointer(
                reference
                    .strip_prefix('#')
                    .context("local schema reference")?,
            )
            .context("schema reference")?;
        validate_schema(spec, target, value)?;
    }
    if let Some(choices) = schema["oneOf"].as_array() {
        ensure!(
            choices
                .iter()
                .filter(|s| validate_schema(spec, s, value).is_ok())
                .count()
                == 1,
            "oneOf mismatch: {value}"
        );
    }
    if let Some(expected) = schema.get("type") {
        let matches = |kind: &str| match kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => false,
        };
        ensure!(
            expected.as_str().is_some_and(matches)
                || expected
                    .as_array()
                    .is_some_and(|kinds| kinds.iter().any(|k| k.as_str().is_some_and(matches))),
            "type mismatch: {value}"
        );
    }
    if let Some(expected) = schema.get("const") {
        ensure!(expected == value, "const mismatch");
    }
    if let Some(choices) = schema["enum"].as_array() {
        ensure!(choices.contains(value), "enum mismatch");
    }
    if let Some(min) = schema["minimum"].as_f64() {
        ensure!(value.as_f64().is_some_and(|n| n >= min), "minimum mismatch");
    }
    if let Some(min) = schema["minLength"].as_u64() {
        ensure!(
            value
                .as_str()
                .is_some_and(|s| s.chars().count() as u64 >= min),
            "empty revision"
        );
    }
    if let Some(format) = schema["format"].as_str() {
        ensure!(
            format == "date-time",
            "unsupported contract format {format}"
        );
        chrono::DateTime::parse_from_rfc3339(value.as_str().context("timestamp string")?)?;
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema["required"].as_array() {
            for key in required {
                ensure!(
                    object.contains_key(key.as_str().context("required key")?),
                    "missing field {key}"
                );
            }
        }
        for (key, value) in object {
            if let Some(property) = schema["properties"].get(key) {
                validate_schema(spec, property, value).with_context(|| format!("field {key}"))?;
            } else {
                ensure!(
                    schema["additionalProperties"] != false,
                    "unexpected field {key}"
                );
            }
        }
    }
    if let (Some(items), Some(values)) = (schema.get("items"), value.as_array()) {
        for value in values {
            validate_schema(spec, items, value)?;
        }
    }
    Ok(())
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
            if path.starts_with("/v0/history") || path == format!("/v0/proposals/{PROPOSAL}") {
                let schema = &spec["paths"][&template][method]["responses"]["200"]["content"]["application/json"]
                    ["schema"]["properties"]["data"];
                validate_schema(&spec, schema, &value["data"]).with_context(|| path.clone())?;
            }
            if path.starts_with("/v0/history") {
                let items = value["data"]["items"].as_array().context("history items")?;
                ensure!(!items.is_empty(), "history proof needs populated entries");
                let schema = if path.contains("?id=") {
                    "HistoryEntry"
                } else {
                    "LogEntry"
                };
                for entry in items {
                    validate_schema(&spec, &spec["components"]["schemas"][schema], entry)?;
                }
                ensure!(
                    items
                        .windows(2)
                        .all(|pair| pair[0]["time"].as_str() <= pair[1]["time"].as_str()),
                    "history chronology"
                );
            }
            if path == format!("/v0/proposals/{PROPOSAL}") {
                ensure!(
                    value["data"]["records"]
                        .as_array()
                        .is_some_and(|v| !v.is_empty()),
                    "record changes fixture"
                );
                ensure!(
                    value["data"]["graph_changes"]
                        .as_array()
                        .is_some_and(|v| v.len() == 2),
                    "edge changes fixture"
                );
                ensure!(
                    value["data"]["configuration_changes"]
                        .as_array()
                        .is_some_and(|v| v
                            .iter()
                            .any(|c| c["id"] == "ontology" && c["before"] != c["after"])),
                    "ontology change fixture"
                );
            }
            if path == "/v0/proposals" {
                for proposal in value["data"].as_array().context("proposal collection")? {
                    ensure!(
                        proposal["store_revision"]
                            .as_str()
                            .is_some_and(|r| !r.is_empty())
                            && proposal.get("revision").is_none(),
                        "proposal collection revision"
                    );
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_reject_legacy_and_malformed_inspection_payloads() -> Result<()> {
        let spec: Value = serde_json::from_str(OPENAPI)?;
        let schemas = &spec["components"]["schemas"];
        for (name, bad) in [
            (
                "HistoryEntry",
                json!({"commit_hash":"abc","committer":"alice","commit_date":"today","payload":"{}"}),
            ),
            (
                "LogEntry",
                json!({"store_revision":"x","actor":"alice","time":"today","message":""}),
            ),
            (
                "HistoryEntry",
                json!({"store_revision":"x","actor":"alice","time":"2026-01-01T00:00:00Z","record":{}}),
            ),
            ("GraphChange", json!("backend diff text")),
            (
                "GraphChange",
                json!({"table":"edges","id":"a","before":null,"after":{"from":"a"}}),
            ),
            (
                "ConfigurationChange",
                json!({"table":"metadata","id":"ontology","after":{}}),
            ),
        ] {
            ensure!(
                validate_schema(&spec, &schemas[name], &bad).is_err(),
                "accepted malformed {name}"
            );
        }
        Ok(())
    }
}
