//! Same transport and seeded rows on three adapters; the memory test has no
//! process or filesystem dependency.
use anyhow::Result;
use clap::Parser;
use mysql::{Conn, OptsBuilder};
use scrolls::{
    conformance::{self, PROPOSAL},
    model::{Graph, Proposal},
    read::{BackendFailure, Snapshot, SnapshotSource},
    storage::{
        Store,
        http::{Config, DoltSource},
    },
    store::{self, Change, ChangeSet, KnowledgeStore, OperationContext, ProposalWrite},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    net::TcpListener,
    path::Path,
    process::{Child, Command},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

type Tables = BTreeMap<String, BTreeMap<String, String>>;
#[derive(Default)]
struct Memory(Mutex<Tables>);
const MEMORY_REVISION: &str = "memory:fixture:1";
impl KnowledgeStore for Memory {
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
    fn commit(&self, changes: &ChangeSet, _: &str, _: &str) -> Result<()> {
        let mut rows = self.0.lock().unwrap();
        let mut candidate = rows.clone();
        for change in &changes.changes {
            match change {
                Change::Put { table, id, payload } => {
                    candidate
                        .entry(table.clone())
                        .or_default()
                        .insert(id.clone(), payload.clone());
                }
                Change::Insert { table, id, payload } => {
                    let table = candidate.entry(table.clone()).or_default();
                    anyhow::ensure!(!table.contains_key(id), "duplicate row");
                    table.insert(id.clone(), payload.clone());
                }
                Change::DeleteEdge { id } => {
                    candidate.entry("edges".into()).or_default().remove(id);
                }
            }
        }
        *rows = candidate;
        Ok(())
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
    fn resume(&self, _: &OperationContext, _: &Graph, _: &str, _: &str) -> Result<Option<Value>> {
        anyhow::bail!("unexpected operation resume")
    }
    fn receipt(&self, _: &str, _: &str) -> Result<Option<Value>> {
        anyhow::bail!("unexpected receipt read")
    }

    fn rows(&self, table: &str, at: Option<&str>) -> Result<Vec<(String, String)>> {
        anyhow::ensure!(at.is_none_or(|r| r == MEMORY_REVISION), "unknown revision");
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(table)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect())
    }
    fn one(&self, table: &str, id: &str, at: Option<&str>) -> Result<Option<String>> {
        Ok(self
            .rows(table, at)?
            .into_iter()
            .find(|(key, _)| key == id)
            .map(|(_, v)| v))
    }
    fn head(&self, reference: &str) -> Result<String> {
        anyhow::ensure!(
            ["main", PROPOSAL, MEMORY_REVISION].contains(&reference),
            "unknown view"
        );
        Ok(MEMORY_REVISION.into())
    }
    fn revision(&self) -> Result<String> {
        Ok(MEMORY_REVISION.into())
    }
    fn branches(&self) -> Result<Vec<String>> {
        Ok(vec!["main".into(), PROPOSAL.into()])
    }
    fn merge_base(&self, from: &str, to: &str) -> Result<String> {
        self.head(from)?;
        self.head(to)
    }
    fn proposals(&self) -> Result<Vec<Proposal>> {
        Ok(vec![serde_json::from_str(
            &self.one("metadata", PROPOSAL, None)?.unwrap(),
        )?])
    }
    fn history(&self, at: &str, id: &str) -> Result<Vec<Value>> {
        Ok(vec![
            json!({"payload":self.one("records",id,Some(at))?,"revision":at}),
        ])
    }
    fn log(&self, at: &str) -> Result<Vec<Value>> {
        self.head(at)?;
        Ok(vec![
            json!({"revision":at,"message":"Read conformance fixture"}),
        ])
    }
    fn diff(&self, from: &str, to: &str, _: &str) -> Result<String> {
        self.head(from)?;
        self.head(to)?;
        Ok(String::new())
    }
    fn request_limit(&self) -> Option<usize> {
        None
    }
    fn shared(&self) -> bool {
        false
    }
}
impl SnapshotSource for Memory {
    fn open(&self) -> std::result::Result<Snapshot, BackendFailure> {
        let store = Memory(Mutex::new(self.0.lock().unwrap().clone()));
        let graph = store::graph(&store, Some(MEMORY_REVISION)).map_err(|e| self.classify(e))?;
        Ok(Snapshot {
            store: Box::new(store),
            graph,
            work: Box::new(scrolls::beads::Local::default()),
            routing: json!({}),
            store_id: "00000000-0000-0000-0000-000000000001".into(),
            selection: "main".into(),
            revision: MEMORY_REVISION.into(),
            actor: "reader".into(),
        })
    }
    fn classify(&self, _: anyhow::Error) -> BackendFailure {
        BackendFailure::Unavailable
    }
}
fn memory() -> Arc<Memory> {
    let memory = Arc::new(Memory::default());
    conformance::seed(memory.as_ref()).unwrap();
    memory
}
#[tokio::test]
async fn memory_needs_no_backend_process() {
    let report = conformance::check(memory()).await.unwrap();
    assert_eq!(report.0.len(), 8);
}

struct Backend {
    dir: tempfile::TempDir,
    child: Option<Child>,
    config: Config,
}
impl Backend {
    fn new(shared: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let binary = Path::new(env!("CARGO_MANIFEST_DIR")).join(".tools/dolt");
        let mut backend = Self {
            config: Config {
                store: dir.path().join("store"),
                dolt: binary.clone(),
                connection: None,
                branch: "main".into(),
                actor: "reader".into(),
            },
            dir,
            child: None,
        };
        if shared {
            let database = backend.dir.path().join("db");
            fs::create_dir(&database).unwrap();
            let output = Command::new(&binary)
                .current_dir(&database)
                .args(["init", "--name", "Test", "--email", "test@localhost"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let port = TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            let log = fs::File::create(backend.dir.path().join("server.log")).unwrap();
            backend.child = Some(
                Command::new(&binary)
                    .current_dir(&database)
                    .env("DOLT_ROOT_PATH", backend.dir.path().join("home"))
                    .env("DOLT_DISABLE_EVENT_FLUSH", "1")
                    .args([
                        "sql-server",
                        "--host",
                        "127.0.0.1",
                        "--port",
                        &port.to_string(),
                    ])
                    .stdout(log.try_clone().unwrap())
                    .stderr(log)
                    .spawn()
                    .unwrap(),
            );
            let start = Instant::now();
            loop {
                let opts = OptsBuilder::new()
                    .ip_or_hostname(Some("127.0.0.1"))
                    .tcp_port(port)
                    .prefer_socket(false)
                    .user(Some("root"))
                    .db_name(Some("db"));
                if Conn::new(opts).is_ok() {
                    break;
                }
                assert!(start.elapsed() < Duration::from_secs(15));
                assert!(
                    backend
                        .child
                        .as_mut()
                        .unwrap()
                        .try_wait()
                        .unwrap()
                        .is_none()
                );
                std::thread::sleep(Duration::from_millis(30));
            }
            let path = backend.dir.path().join("connection.json");
            fs::write(&path,json!({"host":"127.0.0.1","port":port,"database":"db","user":"root","password":"","io_timeout_seconds":11}).to_string()).unwrap();
            backend.config.connection = Some(path);
        }
        let mut args = vec![
            "scrolls".to_string(),
            "--json".into(),
            "--op-id".into(),
            uuid::Uuid::new_v4().to_string(),
            "--actor".into(),
            "reader".into(),
            "--role".into(),
            "human".into(),
            "--store".into(),
            backend.config.store.display().to_string(),
            "--dolt".into(),
            binary.display().to_string(),
        ];
        if let Some(path) = &backend.config.connection {
            args.extend(["--connection".into(), path.display().to_string()]);
        }
        args.push("init".into());
        if shared {
            let mut plan = args.clone();
            plan.push("--dry-run".into());
            let result = scrolls::app::run(&scrolls::cli::Cli::parse_from(plan)).unwrap();
            args.extend([
                "--expect-base".into(),
                result["data"]["base_revision"].as_str().unwrap().into(),
                "--expect-schema".into(),
                result["data"]["ontology_digest"].as_str().unwrap().into(),
            ]);
        }
        scrolls::app::run(&scrolls::cli::Cli::parse_from(args)).unwrap();
        let store = if let Some(path) = &backend.config.connection {
            Store::open_shared(path, &backend.config.store)
        } else {
            Store::open(&backend.config.store, &binary, false)
        }
        .unwrap();
        conformance::seed(&store).unwrap();
        store
            .sql(&format!("CALL DOLT_BRANCH('{}', 'main')", PROPOSAL))
            .unwrap();
        drop(store);
        backend
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            unsafe {
                libc::kill(child.id() as i32, libc::SIGTERM);
            }
            child.wait().unwrap();
        }
    }
}
async fn backend_matches_memory(shared: bool) {
    let expected = conformance::check(memory()).await.unwrap();
    let backend = Backend::new(shared);
    let source = Arc::new(DoltSource(backend.config.clone()));
    assert_eq!(conformance::check(source).await.unwrap(), expected);
}
#[tokio::test(flavor = "multi_thread")]
async fn local_matches_memory() {
    backend_matches_memory(false).await;
}
#[tokio::test(flavor = "multi_thread")]
async fn shared_matches_memory() {
    backend_matches_memory(true).await;
}

struct FailedSource(BackendFailure);
impl SnapshotSource for FailedSource {
    fn open(&self) -> std::result::Result<Snapshot, BackendFailure> {
        Err(self.0)
    }
    fn classify(&self, _: anyhow::Error) -> BackendFailure {
        self.0
    }
}
#[tokio::test]
async fn classified_failures_keep_stable_http_errors() {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    for (failure, status, code, message) in [
        (
            BackendFailure::Unavailable,
            503,
            "backend_unavailable",
            "Store backend unavailable",
        ),
        (
            BackendFailure::Busy,
            503,
            "busy",
            "Store is busy; retry later",
        ),
        (
            BackendFailure::Conflict,
            409,
            "store_state_conflict",
            "Store needs explicit maintenance or branch selection",
        ),
    ] {
        let app = scrolls::http::router(
            Arc::new(FailedSource(failure)),
            "127.0.0.1:18765".parse().unwrap(),
        )
        .unwrap();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .header("host", "127.0.0.1:18765")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
        let value: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(
            value,
            json!({"schema_version":3,"ok":false,"error":{"code":code,"message":message}})
        );
    }
}
