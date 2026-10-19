//! Loopback-only read transport. Blocking Dolt work never runs on the executor.
use crate::{
    cli::{Cli, Filters},
    model::digest,
    read::{Config, Operation, Snapshot},
    schema::Direction,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

pub const OPENAPI: &str = include_str!("../docs/openapi.json");
pub const BODY_LIMIT: usize = 65_536;
const OPERATIONS: &[&str] = &[
    "context",
    "schema",
    "records",
    "document",
    "document_context",
    "walk",
    "prime",
    "query",
    "proposals",
    "proposal",
    "history",
    "openapi",
];
#[derive(Clone)]
struct Server {
    config: Config,
    address: SocketAddr,
    admission: Arc<Semaphore>,
}

#[derive(Debug)]
struct Error {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}
type Result<T> = std::result::Result<T, Error>;
impl Error {
    fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
        }
    }
    fn invalid() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Malformed or unsupported request",
        )
    }
    fn busy() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "busy",
            "Store is busy; retry later",
        )
    }
    fn backend(error: anyhow::Error) -> Self {
        // Existing storage diagnostics are internal only; never echo SQL,
        // credentials, filesystem paths, document text or raw driver errors.
        // Keep typed transport identity even when account names in a driver
        // message happen to resemble a store-state diagnostic.
        if error
            .chain()
            .any(|cause| cause.is::<mysql::Error>() || cause.is::<std::io::Error>())
        {
            return Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "backend_unavailable",
                "Store backend unavailable",
            );
        }
        let text = format!("{error:#}");
        if text.contains("read_lock_timeout") || text.contains("publication_lock_timeout") {
            return Self::busy();
        }
        if [
            "dirty",
            "recovery_required",
            "unsupported",
            "store format",
            "selected proposal",
            "identity",
            "ontology",
            "schema",
        ]
        .iter()
        .any(|s| text.contains(s))
        {
            Self::new(
                StatusCode::CONFLICT,
                "store_state_conflict",
                "Store needs explicit maintenance or branch selection",
            )
        } else {
            Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "backend_unavailable",
                "Store backend unavailable",
            )
        }
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.status,axum::Json(json!({"schema_version":2,"ok":false,"error":{"code":self.code,"message":self.message}}))).into_response()
    }
}

pub fn serve(cli: &Cli, bind: SocketAddr) -> anyhow::Result<()> {
    anyhow::ensure!(
        bind.ip().is_loopback(),
        "serve requires a numeric loopback address"
    );
    let config = Config::from_cli(cli);
    // Validate before binding; the snapshot/session is released immediately.
    drop(Snapshot::open(&config)?);
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(async {
        // Register signals before publishing the startup record.
        #[cfg(unix)]
        let shutdown = {
            use tokio::signal::unix::{SignalKind, signal};
            let mut int = signal(SignalKind::interrupt())?;
            let mut term = signal(SignalKind::terminate())?;
            let mut hup = signal(SignalKind::hangup())?;
            async move { tokio::select! { _ = int.recv() => {}, _ = term.recv() => {}, _ = hup.recv() => {} } }
        };
        #[cfg(not(unix))]
        let shutdown = async { let _ = tokio::signal::ctrl_c().await; };
        let listener = tokio::net::TcpListener::bind(bind).await?;
        let address = listener.local_addr()?;
        let server = Server { config, address, admission: Arc::new(Semaphore::new(1)) };
        let app = Router::new().fallback(handle).with_state(server);
        if cli.json { println!("{}",json!({"schema_version":2,"ok":true,"event":"listening","address":address.to_string(),"api_version":"v0","read_only":true})); }
        else { println!("Listening on http://{address}"); }
        use std::io::Write;
        std::io::stdout().flush()?;
        axum::serve(listener,app).with_graceful_shutdown(shutdown).await?;
        Ok(())
    })
}

fn security(headers: &HeaderMap, address: SocketAddr) -> Result<()> {
    if headers.contains_key("origin") {
        return Err(Error::new(
            StatusCode::BAD_REQUEST,
            "origin_rejected",
            "Browser Origin requests are disabled",
        ));
    }
    let mut hosts = headers.get_all("host").iter();
    let host = hosts
        .next()
        .and_then(|v| v.to_str().ok())
        .ok_or_else(Error::invalid)?;
    if hosts.next().is_some() {
        return Err(Error::invalid());
    }
    // Authority accepts URI userinfo, but a Host header must contain only
    // the listener host and port. Never normalize away a supplied prefix.
    if host.contains('@') {
        return Err(Error::new(
            StatusCode::BAD_REQUEST,
            "host_rejected",
            "Host must match the loopback listener",
        ));
    }
    let authority: axum::http::uri::Authority = host.parse().map_err(|_| Error::invalid())?;
    let name = authority.host().trim_matches(['[', ']']);
    let valid = name.eq_ignore_ascii_case("localhost")
        || name.parse::<IpAddr>().is_ok_and(|ip| {
            ip == address.ip()
                || ip == IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
                || ip == IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
        });
    if !valid || authority.port_u16().unwrap_or(80) != address.port() {
        return Err(Error::new(
            StatusCode::BAD_REQUEST,
            "host_rejected",
            "Host must match the loopback listener",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Route {
    Health,
    Ready,
    Discovery,
    Schema,
    Records,
    Document,
    Context,
    Walk,
    Prime,
    Query,
    Proposals,
    Proposal,
    History,
    Openapi,
}
impl Route {
    fn label(self) -> &'static str {
        match self {
            Self::Health => "/healthz",
            Self::Ready => "/readyz",
            Self::Discovery => "/v0/context",
            Self::Schema => "/v0/schema",
            Self::Records => "/v0/records",
            Self::Document => "/v0/records/{id}",
            Self::Context => "/v0/records/{id}/context",
            Self::Walk => "/v0/records/{id}/walk",
            Self::Prime => "/v0/prime",
            Self::Query => "/v0/query",
            Self::Proposals => "/v0/proposals",
            Self::Proposal => "/v0/proposals/{id}",
            Self::History => "/v0/history",
            Self::Openapi => "/v0/openapi.json",
        }
    }
}
fn route(path: &str) -> Result<(Route, Option<String>)> {
    let exact = match path {
        "/healthz" => Some(Route::Health),
        "/readyz" => Some(Route::Ready),
        "/v0/context" => Some(Route::Discovery),
        "/v0/schema" => Some(Route::Schema),
        "/v0/records" => Some(Route::Records),
        "/v0/prime" => Some(Route::Prime),
        "/v0/query" => Some(Route::Query),
        "/v0/proposals" => Some(Route::Proposals),
        "/v0/history" => Some(Route::History),
        "/v0/openapi.json" => Some(Route::Openapi),
        _ => None,
    };
    if let Some(route) = exact {
        return Ok((route, None));
    }
    let (route, id) = if let Some(rest) = path.strip_prefix("/v0/records/") {
        if let Some(id) = rest.strip_suffix("/context") {
            (Route::Context, id)
        } else if let Some(id) = rest.strip_suffix("/walk") {
            (Route::Walk, id)
        } else {
            (Route::Document, rest)
        }
    } else if let Some(id) = path.strip_prefix("/v0/proposals/") {
        (Route::Proposal, id)
    } else {
        return Err(Error::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "Unknown route",
        ));
    };
    if id.is_empty() {
        return Err(Error::invalid());
    }
    let id = decode(id)?;
    Ok((route, Some(id)))
}
fn decode(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    for (i, c) in bytes.iter().enumerate() {
        if *c == b'%'
            && (i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit())
        {
            return Err(Error::invalid());
        }
    }
    percent_encoding::percent_decode_str(value)
        .decode_utf8()
        .map(|s| s.into_owned())
        .map_err(|_| Error::invalid())
}
struct Params(Vec<(String, String)>);
impl Params {
    fn parse(uri: &Uri) -> Result<Self> {
        let mut pairs = vec![];
        for pair in uri
            .query()
            .unwrap_or("")
            .split('&')
            .filter(|s| !s.is_empty())
        {
            let (k, v) = pair.split_once('=').ok_or_else(Error::invalid)?;
            let k = decode(&k.replace('+', " "))?;
            if k != "field" && pairs.iter().any(|(key, _)| key == &k) {
                return Err(Error::invalid());
            }
            pairs.push((k, decode(&v.replace('+', " "))?));
        }
        Ok(Self(pairs))
    }
    fn allowed(&self, keys: &[&str]) -> Result<()> {
        if self.0.iter().any(|(k, _)| !keys.contains(&k.as_str())) {
            Err(Error::invalid())
        } else {
            Ok(())
        }
    }
    fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
    fn number(&self, key: &str, default: usize) -> Result<usize> {
        self.get(key)
            .map(str::parse)
            .transpose()
            .map(|v| v.unwrap_or(default))
            .map_err(|_| Error::invalid())
    }
    fn boolean(&self, key: &str) -> Result<bool> {
        self.get(key)
            .map(str::parse)
            .transpose()
            .map(|v| v.unwrap_or(false))
            .map_err(|_| Error::invalid())
    }
}

async fn handle(State(server): State<Server>, request: Request) -> Response {
    let start = Instant::now();
    let method = request.method().clone();
    let selected = route(request.uri().path());
    let label = selected
        .as_ref()
        .map(|(r, _)| r.label())
        .unwrap_or("unmatched");
    let result = respond(server, request, selected).await;
    let mut response = match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        "cache-control",
        HeaderValue::from_static("private, no-cache"),
    );
    // Log only the method, route template, status and elapsed time.
    let method = if method == Method::GET {
        "GET"
    } else if method == Method::POST {
        "POST"
    } else {
        "OTHER"
    };
    eprintln!(
        "method={method} route={label} status={} duration_ms={}",
        response.status().as_u16(),
        start.elapsed().as_millis()
    );
    response
}
async fn respond(
    server: Server,
    request: Request,
    selected: Result<(Route, Option<String>)>,
) -> Result<Response> {
    security(request.headers(), server.address)?;
    let (route, id) = selected?;
    let method = request.method();
    if *method
        != if matches!(route, Route::Query) {
            Method::POST
        } else {
            Method::GET
        }
    {
        return Err(Error::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "Unsupported method",
        ));
    }
    let params = Params::parse(request.uri())?;
    let conditional = request
        .headers()
        .get("if-none-match")
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);
    let get = *method == Method::GET;
    if matches!(route, Route::Health) {
        params.allowed(&[])?;
        return representation(json!({"ok":true}), conditional.as_deref());
    }
    if matches!(route, Route::Query)
        && !request
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(';').next() == Some("application/json"))
    {
        return Err(Error::invalid());
    }
    let body = tokio::time::timeout(
        Duration::from_secs(5),
        to_bytes(request.into_body(), BODY_LIMIT),
    )
    .await
    .map_err(|_| Error::invalid())?
    .map_err(body_error)?;
    if get && !body.is_empty() {
        return Err(Error::invalid());
    }
    let query = if matches!(route, Route::Query) {
        Some(serde_json::from_slice::<Query>(&body).map_err(|_| Error::invalid())?)
    } else {
        None
    };
    let permit = tokio::time::timeout(Duration::from_secs(5), server.admission.acquire_owned())
        .await
        .map_err(|_| Error::busy())?
        .map_err(|_| Error::busy())?;
    let value = tokio::task::spawn_blocking(move || {
        // The permit remains owned by the blocking task even if the client
        // disconnects. The Store is dropped before bytes go to the network.
        let _permit = permit;
        let snapshot = Snapshot::open(&server.config).map_err(Error::backend)?;
        execute(&snapshot, route, id.as_deref(), &params, query)
    })
    .await
    .map_err(|_| {
        Error::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "Unexpected server failure",
        )
    })??;
    representation(value, if get { conditional.as_deref() } else { None })
}
fn representation(value: Value, conditional: Option<&str>) -> Result<Response> {
    let bytes = serde_json::to_vec(&value).map_err(|_| {
        Error::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "Response encoding failed",
        )
    })?;
    let etag = format!("\"{}\"", digest(&bytes));
    let matched = conditional.is_some_and(|h| {
        h.split(',')
            .any(|v| v.trim() == "*" || v.trim().strip_prefix("W/").unwrap_or(v.trim()) == etag)
    });
    let mut response = if matched {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        ([("content-type", "application/json")], Body::from(bytes)).into_response()
    };
    response.headers_mut().insert(
        "etag",
        HeaderValue::from_str(&etag).map_err(|_| Error::invalid())?,
    );
    Ok(response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    expression: crate::query::Expr,
    actor: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
    at: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    request: String,
    snapshot: String,
    last: usize,
    item: String,
}
fn paginate(rows: Vec<Value>, params: &Params, snapshot: &str) -> Result<Value> {
    let limit = params.number("limit", 50)?;
    if !(1..=200).contains(&limit) {
        return Err(Error::invalid());
    }
    // Canonical key order; bind filters, page size, route, branch and store.
    let mut binding: Vec<_> = params.0.iter().filter(|(k, _)| k != "cursor").collect();
    binding.sort();
    let request = digest(serde_json::to_vec(&binding).map_err(|_| Error::invalid())?);
    let mut offset = 0;
    if let Some(cursor) = params.get("cursor") {
        let cursor: Cursor = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(cursor)
                .map_err(|_| Error::invalid())?,
        )
        .map_err(|_| Error::invalid())?;
        if cursor.version != 1 || cursor.request != request {
            return Err(Error::invalid());
        }
        if cursor.snapshot != snapshot {
            return Err(Error::new(
                StatusCode::CONFLICT,
                "snapshot_changed",
                "Snapshot changed; restart pagination",
            ));
        }
        offset = cursor.last;
        if offset == 0
            || offset > rows.len()
            || digest(serde_json::to_vec(&rows[offset - 1]).map_err(|_| Error::invalid())?)
                != cursor.item
        {
            return Err(Error::invalid());
        }
    }
    let end = offset.saturating_add(limit).min(rows.len());
    let next = if end < rows.len() {
        Some(
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&Cursor {
                    version: 1,
                    request,
                    snapshot: snapshot.into(),
                    last: end,
                    item: digest(serde_json::to_vec(&rows[end - 1]).map_err(|_| Error::invalid())?),
                })
                .map_err(|_| Error::invalid())?,
            ),
        )
    } else {
        None
    };
    Ok(json!({"items":rows[offset..end],"next_cursor":next}))
}
fn execute(
    s: &Snapshot,
    route: Route,
    id: Option<&str>,
    p: &Params,
    q: Option<Query>,
) -> Result<Value> {
    let view = s.view();
    let id = id.unwrap_or("");
    // Missing records are distinguished from malformed predicates and backends.
    let resolved = if matches!(route, Route::Document | Route::Context | Route::Walk) {
        Some(view.resolve(id).map_err(|_| {
            Error::new(
                StatusCode::NOT_FOUND,
                "record_not_found",
                "Record or alias not found",
            )
        })?)
    } else {
        None
    };
    let id = resolved.as_deref().unwrap_or(id);
    let read = |operation| {
        view.execute(operation).map_err(|_| {
            Error::new(
                StatusCode::BAD_REQUEST,
                "invalid_read",
                "Invalid query, traversal or document budget",
            )
        })
    };
    let data = match route {
        Route::Health => unreachable!(),
        Route::Ready => {
            p.allowed(&[])?;
            json!({"ready":true})
        }
        Route::Discovery => {
            p.allowed(&[])?;
            json!({"api_version":"v0","operations":OPERATIONS,"read_only":true,"store_id":s.store_id,"branch":s.branch})
        }
        Route::Schema => {
            p.allowed(&[])?;
            read(Operation::Schema)?
        }
        Route::Records => {
            p.allowed(&["kind", "status", "owner", "field", "q", "limit", "cursor"])?;
            let filters = Filters {
                kind: p.get("kind").map(str::to_owned),
                status: p.get("status").map(str::to_owned),
                owner: p.get("owner").map(str::to_owned),
                field: p
                    .0
                    .iter()
                    .filter(|(k, _)| k == "field")
                    .map(|(_, v)| v.clone())
                    .collect(),
            };
            let rows = read(Operation::Records {
                filters: &filters,
                terms: p.get("q"),
                limit: usize::MAX,
            })?;
            paginate(
                rows.as_array().unwrap().clone(),
                p,
                &digest(format!(
                    "records:{}:{}:{}:{}",
                    s.store_id, s.branch, s.revision, s.actor
                )),
            )?
        }
        Route::Document => {
            p.allowed(&["provenance"])?;
            read(Operation::Document {
                id,
                provenance: p.boolean("provenance")?,
            })?
        }
        Route::Context => {
            p.allowed(&["budget"])?;
            read(Operation::Context {
                id,
                budget: p.number("budget", 16000)?,
            })?
        }
        Route::Walk => {
            p.allowed(&["rel", "depth", "direction", "full"])?;
            let direction = match p.get("direction").unwrap_or("both") {
                "both" => Direction::Both,
                "incoming" => Direction::Incoming,
                "outgoing" => Direction::Outgoing,
                _ => return Err(Error::invalid()),
            };
            read(Operation::Walk {
                id,
                rel: p.get("rel"),
                depth: p.number("depth", 2)?,
                direction,
                full: p.boolean("full")?,
            })?
        }
        Route::Prime => {
            p.allowed(&["budget"])?;
            read(Operation::Prime {
                budget: p.number("budget", 16000)?,
            })?
        }
        Route::Query => {
            p.allowed(&[])?;
            let q = q.ok_or_else(Error::invalid)?;
            read(Operation::Query {
                expression: &q.expression,
                actor: q.actor.as_deref().unwrap_or(&s.actor),
                paths: &q.paths,
                at: q.at.as_deref(),
            })?
        }
        Route::Proposals => {
            p.allowed(&[])?;
            let mut rows = vec![];
            for proposal in s.store.proposals().map_err(Error::backend)? {
                let revision = s.store.head(&proposal.branch).map_err(Error::backend)?;
                let mut v = json!(proposal);
                v["revision"] = json!(revision);
                rows.push(v);
            }
            json!(rows)
        }
        Route::Proposal => {
            p.allowed(&[])?;
            let name = if id.starts_with("proposal/") {
                id.to_string()
            } else {
                format!("proposal/{id}")
            };
            if !s
                .store
                .proposals()
                .map_err(Error::backend)?
                .iter()
                .any(|p| p.branch == name)
            {
                return Err(Error::new(
                    StatusCode::NOT_FOUND,
                    "proposal_not_found",
                    "Proposal not found",
                ));
            }
            crate::read::proposal_detail(&s.store, &name).map_err(Error::backend)?
        }
        Route::History => {
            p.allowed(&["id", "limit", "cursor"])?;
            let id = p
                .get("id")
                .map(|id| {
                    view.resolve(id).map_err(|_| {
                        Error::new(
                            StatusCode::NOT_FOUND,
                            "record_not_found",
                            "Record or alias not found",
                        )
                    })
                })
                .transpose()?;
            let rows = crate::read::history(&s.store, &s.revision, id.as_deref())
                .map_err(Error::backend)?;
            paginate(
                rows,
                p,
                &digest(format!(
                    "history:{}:{}:{}",
                    s.store_id, s.branch, s.revision
                )),
            )?
        }
        Route::Openapi => {
            p.allowed(&[])?;
            return serde_json::from_str(OPENAPI).map_err(|_| {
                Error::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "API specification invalid",
                )
            });
        }
    };
    Ok(s.envelope(data))
}

fn body_error(error: axum::Error) -> Error {
    use std::error::Error as _;
    if error
        .source()
        .is_some_and(|source| source.is::<http_body_util::LengthLimitError>())
    {
        Error::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "body_too_large",
            "Request body exceeds 65536 bytes",
        )
    } else {
        Error::invalid()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn body_errors_distinguish_limit_from_malformed_transport() {
        let oversized = to_bytes(Body::from("too large"), 1).await.unwrap_err();
        assert_eq!(body_error(oversized).status, StatusCode::PAYLOAD_TOO_LARGE);
        let malformed = axum::Error::new(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "private body detail",
        ));
        let response = body_error(malformed);
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert!(!response.message.contains("private"));
    }
    #[test]
    fn typed_backend_failures_do_not_depend_on_account_names() {
        let error = mysql::Error::MySqlError(mysql::MySqlError {
            state: "28000".into(),
            code: 1045,
            message: "Access denied for schema_identity_user".into(),
        });
        let response =
            Error::backend(anyhow::Error::new(error).context("shared Dolt connection failed"));
        assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.code, "backend_unavailable");
        assert!(!response.message.contains("schema_identity_user"));
    }
}
