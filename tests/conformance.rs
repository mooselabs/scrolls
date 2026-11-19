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
    store::{
        self, Change, ChangeSet, HistoryEntry, KnowledgeStore, LogEntry, OperationContext,
        ProposalWrite, RowChange,
    },
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
#[derive(Clone, Default)]
struct MemoryState {
    main: Tables,
    proposal: Tables,
    selected: String,
    log: Vec<LogEntry>,
    history: Vec<HistoryEntry>,
}
#[derive(Default)]
struct Memory(Mutex<MemoryState>);
const MEMORY_REVISION: &str = "memory:fixture:1";
const PROPOSAL_REVISION: &str = "memory:proposal:2";
impl KnowledgeStore for Memory {
    fn select_branch(&self, view: &str) -> Result<()> {
        anyhow::ensure!(["main", PROPOSAL].contains(&view), "unknown view");
        self.0.lock().unwrap().selected = view.into();
        Ok(())
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
    fn commit(&self, changes: &ChangeSet, message: &str, actor: &str) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        let proposal = state.selected == PROPOSAL;
        let mut candidate = if proposal {
            state.proposal.clone()
        } else {
            state.main.clone()
        };
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
        let entry = LogEntry {
            store_revision: if proposal {
                PROPOSAL_REVISION
            } else {
                MEMORY_REVISION
            }
            .into(),
            actor: actor.into(),
            time: if proposal {
                "2026-01-02T00:00:00Z"
            } else {
                "2026-01-01T00:00:00Z"
            }
            .into(),
            message: message.into(),
        };
        let history = candidate
            .get("records")
            .into_iter()
            .flat_map(|t| t.values())
            .map(|record| {
                Ok(HistoryEntry {
                    store_revision: entry.store_revision.clone(),
                    actor: entry.actor.clone(),
                    time: entry.time.clone(),
                    record: serde_json::from_str(record)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if proposal {
            state.proposal = candidate;
        } else {
            state.main = candidate.clone();
            state.proposal = candidate;
        }
        state.log.push(entry);
        state.history.extend(history);
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
        let state = self.0.lock().unwrap();
        let tables = match at {
            Some(PROPOSAL_REVISION) => &state.proposal,
            Some(MEMORY_REVISION) => &state.main,
            None if state.selected == PROPOSAL => &state.proposal,
            None => &state.main,
            _ => anyhow::bail!("unknown revision"),
        };
        Ok(tables
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
        match reference {
            "main" | MEMORY_REVISION => Ok(MEMORY_REVISION.into()),
            PROPOSAL | PROPOSAL_REVISION => Ok(PROPOSAL_REVISION.into()),
            _ => anyhow::bail!("unknown view"),
        }
    }
    fn revision(&self) -> Result<String> {
        let selected = self.0.lock().unwrap().selected.clone();
        self.head(if selected.is_empty() {
            "main"
        } else {
            &selected
        })
    }
    fn branches(&self) -> Result<Vec<String>> {
        Ok(vec!["main".into(), PROPOSAL.into()])
    }
    fn base_revision(&self, main: &str, proposal: &str) -> Result<String> {
        self.head(proposal)?;
        self.head(main)
    }
    fn proposals(&self) -> Result<Vec<Proposal>> {
        Ok(vec![serde_json::from_str(
            &self.one("metadata", PROPOSAL, None)?.unwrap(),
        )?])
    }
    fn history(&self, at: &str, id: &str) -> Result<Vec<HistoryEntry>> {
        let at = self.head(at)?;
        Ok(self
            .0
            .lock()
            .unwrap()
            .history
            .iter()
            .filter(|e| {
                e.record.id == id
                    && (at == PROPOSAL_REVISION || e.store_revision == MEMORY_REVISION)
            })
            .cloned()
            .collect())
    }
    fn log(&self, at: &str) -> Result<Vec<LogEntry>> {
        let at = self.head(at)?;
        Ok(self
            .0
            .lock()
            .unwrap()
            .log
            .iter()
            .filter(|e| at == PROPOSAL_REVISION || e.store_revision == MEMORY_REVISION)
            .cloned()
            .collect())
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
    memory.select_branch(PROPOSAL).unwrap();
    memory
        .commit(
            &conformance::proposal_fixture(memory.as_ref()).unwrap(),
            "Proposed fixture",
            "reviewer",
        )
        .unwrap();
    memory.select_branch("main").unwrap();
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
        store.select_branch(PROPOSAL).unwrap();
        store
            .commit(
                &conformance::proposal_fixture(&store).unwrap(),
                "Proposed fixture",
                "reviewer",
            )
            .unwrap();
        store.select_branch("main").unwrap();
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

#[test]
fn typed_row_changes_preserve_empty_values_and_deletions() {
    let before = vec![
        ("updated".into(), "{\"x\":1}".into()),
        ("deleted".into(), "[]".into()),
        ("same".into(), "{\"a\":0,\"b\":false}".into()),
    ];
    let after = vec![
        ("same".into(), "{ \"b\": false, \"a\": 0 }".into()),
        ("updated".into(), "{\"x\":0}".into()),
        ("inserted".into(), "\"\"".into()),
    ];
    let changes = RowChange::between("metadata", before, after).unwrap();
    assert_eq!(
        serde_json::to_value(changes).unwrap(),
        json!([
            {"table":"metadata","id":"deleted","before":[],"after":null},
            {"table":"metadata","id":"inserted","before":null,"after":""},
            {"table":"metadata","id":"updated","before":{"x":1},"after":{"x":0}}
        ])
    );
    assert!(
        RowChange::between(
            "metadata",
            vec![],
            vec![("bad".into(), "invalid JSON".into())]
        )
        .is_err()
    );
}

#[test]
fn memory_history_pins_snapshots_and_review_renders_typed_changes() {
    let store = memory();
    let main = store.history(MEMORY_REVISION, conformance::RECORD).unwrap();
    let proposed = store
        .history(PROPOSAL_REVISION, conformance::RECORD)
        .unwrap();
    assert_eq!(main.len(), 1);
    assert_eq!(proposed.len(), 2);
    assert_eq!(main[0].actor, "reader");
    assert_eq!(proposed[1].actor, "reviewer");
    assert_eq!(proposed[1].record.body, "Proposed portable fixture");
    assert_eq!(
        store.log(PROPOSAL_REVISION).unwrap()[1].message,
        "Proposed fixture"
    );
    let detail = scrolls::read::proposal_detail(store.as_ref(), PROPOSAL).unwrap();
    let text = scrolls::presentation::human(&json!({"data":detail}));
    for expected in [
        "Configuration changes:",
        "Graph changes:",
        "metadata ontology",
        "before:",
        "after:",
        "Proposed relationship",
    ] {
        assert!(text.contains(expected), "{text}");
    }
}
