//! Real-server qualification. Never falls back to a live or newer Dolt binary.
use mysql::{Conn, OptsBuilder, prelude::Queryable};
use scrolls::storage::{
    DOLT_VERSION, literal,
    session::{Connection, SharedSession, SqlSession},
};
use serde_json::json;
use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Server {
    dir: tempfile::TempDir,
    child: Option<Child>,
    port: u16,
}
impl Server {
    fn binary() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".tools/dolt")
    }
    fn command(&self) -> Command {
        let mut c = Command::new(Self::binary());
        c.current_dir(self.dir.path().join("knowledge"))
            .env("DOLT_ROOT_PATH", self.dir.path().join("home"))
            .env("DOLT_DISABLE_EVENT_FLUSH", "1");
        c
    }
    fn cli(&self, args: &[&str]) {
        let out = self.command().args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    fn new() -> Self {
        Self::create(false)
    }
    fn empty() -> Self {
        Self::create(true)
    }
    fn create(empty: bool) -> Self {
        let mut s = Self {
            dir: tempfile::tempdir().unwrap(),
            child: None,
            port: 0,
        };
        fs::create_dir(s.dir.path().join("knowledge")).unwrap();
        s.cli(&[
            "init",
            "--name",
            "Qualification",
            "--email",
            "test@localhost",
        ]);
        if !empty {
            s.cli(&["sql", "-q", "CREATE TABLE records (id INT PRIMARY KEY, payload TEXT); CREATE TABLE operations (id VARCHAR(128) PRIMARY KEY, payload TEXT); CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','fixture','--author','Test <test@localhost>'); CALL DOLT_BRANCH('proposal/test');"]);
        }
        s.start();
        let mut root = s.raw("main");
        root.query_drop("CREATE USER alice IDENTIFIED BY 'test-only'; GRANT ALL ON *.* TO alice")
            .unwrap();
        s
    }
    fn start(&mut self) {
        self.port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let log = fs::File::create(self.dir.path().join("server.log")).unwrap();
        self.child = Some(
            self.command()
                .args([
                    "sql-server",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &self.port.to_string(),
                ])
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        let start = Instant::now();
        while Conn::new(self.opts("main")).is_err() {
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "server failed: {}",
                fs::read_to_string(self.dir.path().join("server.log")).unwrap()
            );
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            child.kill().unwrap();
            child.wait().unwrap();
        }
    }
    fn opts(&self, branch: &str) -> OptsBuilder {
        OptsBuilder::new()
            .ip_or_hostname(Some("127.0.0.1"))
            .tcp_port(self.port)
            .prefer_socket(false)
            .user(Some("root"))
            .db_name(Some(format!("knowledge/{branch}")))
            .tcp_connect_timeout(Some(Duration::from_secs(1)))
            .read_timeout(Some(Duration::from_secs(5)))
    }
    fn raw(&self, branch: &str) -> Conn {
        Conn::new(self.opts(branch)).unwrap()
    }
    fn config(&self) -> Connection {
        Connection {
            host: "127.0.0.1".into(),
            port: self.port,
            database: "knowledge".into(),
            user: "alice".into(),
            password: "test-only".into(),
            connect_timeout: Duration::from_secs(2),
            io_timeout: Duration::from_secs(5),
            lock_timeout: Duration::ZERO,
        }
    }
    fn shared(&self, branch: &str) -> SharedSession {
        SharedSession::connect(&self.config(), branch).unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

#[test]
fn branches_are_session_local_and_transactions_are_repeatable_read() {
    let s = Server::new();
    let a = s.shared("main");
    let mut b = s.raw("proposal/test");
    let branch: String = b.query_first("SELECT active_branch()").unwrap().unwrap();
    assert_eq!(branch, "proposal/test");
    assert_eq!(
        a.sql("SELECT active_branch() AS branch").unwrap()[0]["branch"],
        "main"
    );
    b.query_drop("INSERT INTO records VALUES (1,'proposal')")
        .unwrap();
    assert!(a.sql("SELECT * FROM records").unwrap().is_empty());
    b.query_drop("CALL DOLT_RESET('--hard'); USE `knowledge/main`; START TRANSACTION")
        .unwrap();
    let count: u64 = b
        .query_first("SELECT COUNT(*) FROM records")
        .unwrap()
        .unwrap();
    assert_eq!(count, 0);
    a.commit("INSERT INTO records VALUES (2,'committed'); INSERT INTO operations VALUES ('op-1','receipt')", "first", "alice").unwrap();
    let count: u64 = b
        .query_first("SELECT COUNT(*) FROM records")
        .unwrap()
        .unwrap();
    assert_eq!(count, 0, "reader must retain the transaction snapshot");
    b.query_drop("COMMIT").unwrap();
    let count: u64 = b
        .query_first("SELECT COUNT(*) FROM records")
        .unwrap()
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn lock_spans_branches_is_bounded_and_disconnect_releases_it() {
    let s = Server::new();
    let a = s.shared("main");
    let start = Instant::now();
    let error = SharedSession::connect(&s.config(), "proposal/test")
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("publication_lock_timeout"),
        "{error:#}"
    );
    assert!(start.elapsed() < Duration::from_secs(3));
    let mut config = s.config();
    config.lock_timeout = Duration::from_secs(1);
    let start = Instant::now();
    assert!(SharedSession::connect(&config, "main").is_err());
    assert!(start.elapsed() >= Duration::from_millis(900));
    assert!(start.elapsed() < Duration::from_secs(3));
    drop(a);
    // Drop closes the TCP session, without an explicit RELEASE_LOCK.
    let start = Instant::now();
    loop {
        match SharedSession::connect(&s.config(), "proposal/test") {
            Ok(b) => {
                assert_eq!(
                    b.sql("SELECT active_branch() AS branch").unwrap()[0]["branch"],
                    "proposal/test"
                );
                break;
            }
            Err(e) => {
                assert!(start.elapsed() < Duration::from_secs(2), "{e:#}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[test]
fn waiting_writer_reads_the_base_after_lock_acquisition() {
    let s = Server::new();
    let a = s.shared("main");
    let mut config = s.config();
    config.lock_timeout = Duration::from_secs(2);
    let waiter = std::thread::spawn(move || {
        let b = SharedSession::connect(&config, "main").unwrap();
        (
            b.revision().unwrap(),
            b.sql("SELECT payload FROM records WHERE id=1").unwrap(),
        )
    });
    // Observe the request on the server, so this cannot pass by starting the
    // contender only after the publisher has already committed/disconnected.
    let mut observer = s.raw("main");
    let start = Instant::now();
    loop {
        let waiting: u64 = observer.query_first(
            "SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE INFO LIKE 'SELECT GET_LOCK(%'"
        ).unwrap().unwrap();
        if waiting > 0 {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "contender never waited for the publication lock"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let revision = a
        .commit(
            "INSERT INTO records VALUES (1,'new base')",
            "new base",
            "alice",
        )
        .unwrap();
    drop(a);
    let (seen, rows) = waiter.join().unwrap();
    assert_eq!(seen, revision);
    assert_eq!(rows[0]["payload"], "new base");
}

/// Forward protocol packets until the server returns a DOLT_COMMIT response,
/// then drop that response. This simulates lost acknowledgement after publish,
/// rather than a failure before a write was sent.
fn lose_commit_ack(target_port: u16) -> (u16, std::thread::JoinHandle<()>) {
    lose_matching_ack(target_port, "CALL DOLT_COMMIT(".into())
}
fn lose_matching_ack(target_port: u16, needle: String) -> (u16, std::thread::JoinHandle<()>) {
    use std::{
        io::{Read, Write},
        net::{Shutdown, TcpStream},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    fn packet(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
        let mut header = [0_u8; 4];
        stream.read_exact(&mut header)?;
        let len =
            usize::from(header[0]) | usize::from(header[1]) << 8 | usize::from(header[2]) << 16;
        let mut bytes = vec![0; len + 4];
        bytes[..4].copy_from_slice(&header);
        stream.read_exact(&mut bytes[4..])?;
        Ok(bytes)
    }
    let handle = std::thread::spawn(move || {
        let (mut client, _) = listener.accept().unwrap();
        let mut server = TcpStream::connect(("127.0.0.1", target_port)).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut client_in = client.try_clone().unwrap();
        let mut server_out = server.try_clone().unwrap();
        let discard = Arc::new(AtomicBool::new(false));
        let request_discard = discard.clone();
        let upstream = std::thread::spawn(move || {
            while let Ok(bytes) = packet(&mut client_in) {
                if bytes.get(4) == Some(&3)
                    && String::from_utf8_lossy(&bytes[5..]).contains(&needle)
                {
                    request_discard.store(true, Ordering::Release);
                }
                if server_out.write_all(&bytes).is_err() {
                    break;
                }
            }
            let _ = server_out.shutdown(Shutdown::Both);
        });
        while let Ok(bytes) = packet(&mut server) {
            if discard.load(Ordering::Acquire) {
                break;
            }
            if client.write_all(&bytes).is_err() {
                break;
            }
        }
        let _ = client.shutdown(Shutdown::Both);
        let _ = server.shutdown(Shutdown::Both);
        upstream.join().unwrap();
    });
    (port, handle)
}

#[test]
fn lost_commit_ack_is_indeterminate_and_receipt_is_recovered_on_another_client() {
    let s = Server::new();
    let (port, proxy) = lose_commit_ack(s.port);
    let mut config = s.config();
    config.port = port;
    let a = SharedSession::connect(&config, "main").unwrap();
    let base = a.revision().unwrap();
    let error = a.commit("INSERT INTO records VALUES (1,'published'); INSERT INTO operations VALUES ('lost-ack','original-input-digest')", "lost ack", "alice").unwrap_err();
    assert!(
        error.to_string().contains("indeterminate_commit"),
        "{error:#}"
    );
    drop(a);
    proxy.join().unwrap();
    let mut config = s.config();
    config.lock_timeout = Duration::from_secs(2);
    let b = SharedSession::connect(&config, "main").unwrap();
    assert_ne!(b.revision().unwrap(), base);
    assert_eq!(
        b.sql("SELECT payload FROM operations WHERE id='lost-ack'")
            .unwrap()[0]["payload"],
        "original-input-digest"
    );
    assert_eq!(
        b.sql("SELECT payload FROM records WHERE id=1").unwrap()[0]["payload"],
        "published"
    );
    b.clean().unwrap();
}

#[test]
fn rows_and_receipts_survive_restart_and_failed_changes_roll_back() {
    let mut s = Server::new();
    let a = s.shared("main");
    let base = a.revision().unwrap();
    let error = a
        .commit(
            "INSERT INTO records VALUES (1,'must rollback'); INSERT INTO absent VALUES (1)",
            "fail",
            "alice",
        )
        .unwrap_err();
    assert!(error.to_string().contains("reconcile operation receipt"));
    assert!(a.sql("SELECT * FROM records").unwrap().is_empty());
    assert_eq!(a.revision().unwrap(), base);
    a.clean().unwrap();
    let revision = a.commit("INSERT INTO records VALUES (1,'durable'); INSERT INTO operations VALUES ('stable-op','same-input-digest')", "publish", "alice").unwrap();
    assert_ne!(revision, base);
    drop(a);
    s.stop();
    s.start();
    let b = s.shared("main");
    assert_eq!(b.revision().unwrap(), revision);
    assert_eq!(
        b.sql("SELECT payload FROM records WHERE id=1").unwrap()[0]["payload"],
        "durable"
    );
    assert_eq!(
        b.sql("SELECT payload FROM operations WHERE id='stable-op'")
            .unwrap()[0]["payload"],
        "same-input-digest"
    );
    assert!(
        b.commit(
            "INSERT INTO operations VALUES ('stable-op','different-input')",
            "duplicate",
            "alice"
        )
        .is_err()
    );
    assert_eq!(b.revision().unwrap(), revision);
}

#[test]
fn unexplained_dirty_branches_are_not_discarded() {
    let s = Server::new();
    let mut raw = s.raw("proposal/test");
    raw.query_drop("INSERT INTO records VALUES (9,'raw SQL bypass')")
        .unwrap();
    drop(raw);
    let error = SharedSession::connect(&s.config(), "main").err().unwrap();
    assert!(error.to_string().contains("dirty_state"), "{error:#}");
    let mut raw = s.raw("proposal/test");
    let value: String = raw
        .query_first("SELECT payload FROM records WHERE id=9")
        .unwrap()
        .unwrap();
    assert_eq!(value, "raw SQL bypass");
}

#[test]
fn sql_result_types_and_literals_match_the_local_boundary() {
    let s = Server::new();
    let a = s.shared("main");
    let local_dir = tempfile::tempdir().unwrap();
    let local =
        scrolls::storage::Store::open(&local_dir.path().join("store"), &Server::binary(), true)
            .unwrap();
    let query = format!(
        "SELECT {} AS text, 0 AS zero, -1 AS negative, 2.5e0 AS float_value, NULL AS missing",
        literal("quotes '\\ and 💾")
    );
    let transports: [&dyn SqlSession; 2] = [&local, &a];
    assert_eq!(
        transports[0].sql(&query).unwrap(),
        transports[1].sql(&query).unwrap()
    );
    assert_eq!(
        a.sql("SELECT DOLT_VERSION() AS version").unwrap(),
        vec![json!({"version":DOLT_VERSION})]
    );
    assert_eq!(
        a.sql(&format!(
            "SELECT {} AS text, 0 AS zero, NULL AS missing; SELECT 2 AS count",
            literal("quotes '\\ and 💾")
        ))
        .unwrap(),
        vec![
            json!({"text":"quotes '\\ and 💾","zero":0}),
            json!({"count":2})
        ]
    );
    for branch in ["", "../main", "main`", "main; DROP TABLE records"] {
        assert!(SharedSession::connect(&s.config(), branch).is_err());
    }
    let mut config = s.config();
    config.database = "knowledge/main".into();
    assert!(SharedSession::connect(&config, "main").is_err());
    let mut config = s.config();
    config.host = "localhost".into();
    let error = SharedSession::connect(&config, "main").err().unwrap();
    assert!(
        error.to_string().contains("numeric IP address"),
        "{error:#}"
    );
}

// A separate OS process is the client in the contention/kill test below.
#[test]
fn client_process() {
    let Ok(port) = std::env::var("SCROLLS_TEST_PORT") else {
        return;
    };
    let marker = std::env::var("SCROLLS_TEST_MARKER").unwrap();
    let config = Connection {
        host: "127.0.0.1".into(),
        port: port.parse().unwrap(),
        database: "knowledge".into(),
        user: "alice".into(),
        password: "test-only".into(),
        connect_timeout: Duration::from_secs(2),
        io_timeout: Duration::from_secs(5),
        lock_timeout: Duration::ZERO,
    };
    let a = SharedSession::connect(&config, "main").unwrap();
    a.sql("START TRANSACTION; INSERT INTO records VALUES (7,'interrupted')")
        .unwrap();
    fs::write(marker, "locked").unwrap();
    std::thread::sleep(Duration::from_secs(30));
    panic!("qualification parent failed to kill the interrupted client");
}

#[test]
fn killed_client_releases_lock_and_uncommitted_rows() {
    let s = Server::new();
    let marker = s.dir.path().join("client-ready");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "client_process", "--nocapture"])
        .env("SCROLLS_TEST_PORT", s.port.to_string())
        .env("SCROLLS_TEST_MARKER", &marker)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !marker.exists() {
        if start.elapsed() > Duration::from_secs(10) || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child failed before acquiring lock");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let contender = SharedSession::connect(&s.config(), "proposal/test");
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        contender
            .err()
            .unwrap()
            .to_string()
            .contains("publication_lock_timeout")
    );
    let mut config = s.config();
    config.lock_timeout = Duration::from_secs(2);
    let recovered = SharedSession::connect(&config, "main").unwrap();
    assert!(recovered.sql("SELECT * FROM records").unwrap().is_empty());
    recovered.clean().unwrap();
}

impl Server {
    fn client(&self, client: &str, args: &[&str]) -> Command {
        let root = self.dir.path().join(client);
        fs::create_dir_all(&root).unwrap();
        let connection = root.join("connection.json");
        fs::write(&connection, serde_json::to_vec(&json!({"host":"127.0.0.1","port":self.port,"database":"knowledge","user":"alice","password":"test-only"})).unwrap()).unwrap();
        let mut c = Command::new(env!("CARGO_BIN_EXE_scrolls"));
        if !args.contains(&"--actor") {
            c.args(["--actor", "alice"]);
        }
        if !args.contains(&"--role") {
            c.args(["--role", "human"]);
        }
        c.arg("--json")
            .arg("--connection")
            .arg(connection)
            .arg("--store")
            .arg(root)
            .args(args)
            .stdin(Stdio::null());
        c
    }
    fn call(&self, client: &str, args: &[&str], expected: bool) -> serde_json::Value {
        let out = self.client(client, args).output().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
            panic!(
                "{} {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        assert_eq!(out.status.success(), expected, "{args:?}: {value}");
        value
    }
    fn init(&self, schema: Option<&str>) -> serde_json::Value {
        let mut args = vec!["init"];
        if let Some(path) = schema {
            args.extend(["--schema", path]);
        }
        let mut plan_args = args.clone();
        plan_args.push("--dry-run");
        let plan = self.call("a", &plan_args, true);
        args.extend([
            "--op-id",
            "initialization",
            "--expect-base",
            plan["data"]["base_revision"].as_str().unwrap(),
            "--expect-schema",
            plan["data"]["ontology_digest"].as_str().unwrap(),
        ]);
        let initialized = self.call("a", &args, true);
        assert_eq!(initialized, self.call("b", &args, true));
        initialized
    }
}

#[test]
fn shared_cli_initialization_replay_isolation_fencing_and_refusals() {
    let s = Server::empty();
    let base: String = s
        .raw("main")
        .query_first("SELECT DOLT_HASHOF('HEAD')")
        .unwrap()
        .unwrap();
    let plan = s.call("a", &["init", "--dry-run"], true);
    assert_eq!(plan["revision"], base);
    let bad = s.call(
        "a",
        &[
            "init",
            "--op-id",
            "init",
            "--expect-base",
            "stale",
            "--expect-schema",
            "stale",
        ],
        false,
    );
    assert!(bad.to_string().contains("shared_init_plan_stale"));
    assert!(
        s.raw("main")
            .query::<String, _>("SHOW TABLES")
            .unwrap()
            .is_empty()
    );
    let initialized = s.init(None);
    assert_ne!(initialized["revision"], base);
    fs::write(s.dir.path().join("a/beads.json"), "not JSON").unwrap();
    fs::write(
        s.dir.path().join("b/beads.json"),
        r#"{"operations":{"new-source":{"hash":"collision","result":{}}}}"#,
    )
    .unwrap();
    let args = [
        "--op-id",
        "new-source",
        "new",
        "evidence",
        "--title",
        "Source",
        "--body",
        "fact 🌍 ' ; DROP TABLE records;",
    ];
    let written = s.call("a", &args, true);
    assert_eq!(written, s.call("b", &args, true));
    let mismatch = s.call(
        "b",
        &[
            "--op-id",
            "new-source",
            "--actor",
            "mallory",
            "new",
            "evidence",
            "--title",
            "Source",
            "--body",
            "changed",
        ],
        false,
    );
    assert!(mismatch.to_string().contains("different arguments"));
    let id = written["data"]["id"].as_str().unwrap();
    let read = s.call("b", &["show", id], true);
    assert_eq!(read["consumed"]["records"][id], 1);
    assert!(!s.dir.path().join("b/consumed.jsonl").exists());
    let stale = s.call(
        "b",
        &[
            "--op-id", "stale", "edit", id, "--expect", "0", "--body", "wrong",
        ],
        false,
    );
    assert!(stale.to_string().contains("revision"));
    let before = s.call("a", &["list"], true)["revision"].clone();
    for args in [
        vec!["migrate"],
        vec!["beads", "list"],
        vec!["consumed"],
        vec!["import", "markdown", ".", "--rev", "HEAD"],
    ] {
        assert!(
            s.call("b", &args, false)
                .to_string()
                .contains("shared_unsupported")
        );
    }
    assert_eq!(before, s.call("a", &["list"], true)["revision"]);
    let mut raw = s.raw("main");
    raw.query_drop(format!("UPDATE metadata SET payload='99' WHERE id={}; CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','future writer','--author','Test <test@localhost>')",literal("shared_writer"))).unwrap();
    assert!(
        s.call("a", &["list"], false)
            .to_string()
            .contains("unsupported_shared_writer")
    );
}

#[test]
fn shared_cli_concurrent_revision_and_graph_races() {
    let s = Server::empty();
    s.init(None);
    let mut children = vec![];
    for n in 0..4 {
        children.push(
            s.client(
                &format!("client{n}"),
                &[
                    "--op-id",
                    &format!("op{n}"),
                    "new",
                    "note",
                    "--title",
                    "parallel",
                    "--body",
                    "content",
                ],
            )
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
        );
    }
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
    assert_eq!(
        s.call("a", &["list"], true)["data"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let created = s.call(
        "a",
        &[
            "--op-id",
            "edit-target",
            "new",
            "note",
            "--title",
            "target",
            "--body",
            "original",
        ],
        true,
    );
    let id = created["data"]["id"].as_str().unwrap();
    let mut children = vec![];
    for n in 0..2 {
        children.push(
            s.client(
                &format!("editor{n}"),
                &[
                    "--op-id",
                    &format!("edit{n}"),
                    "edit",
                    id,
                    "--expect",
                    "1",
                    "--body",
                    &format!("new {n}"),
                ],
            )
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
        );
    }
    let results: Vec<_> = children
        .into_iter()
        .map(|c| c.wait_with_output().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|o| o.status.success()).count(), 1);
    assert_eq!(
        s.call("a", &["show", id], true)["data"]["record"]["revision"],
        2
    );
}

#[test]
fn shared_cli_handbook_cycle_race_and_research_policies() {
    let s = Server::empty();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    s.init(Some(
        manifest.join("examples/handbook.yaml").to_str().unwrap(),
    ));
    let mut ids = vec![];
    for n in 0..2 {
        ids.push(
            s.call(
                "a",
                &[
                    "--op-id",
                    &format!("page{n}"),
                    "new",
                    "page",
                    "--title",
                    "Page",
                    "--body",
                    "text",
                ],
                true,
            )["data"]["id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    let before = s.call("a", &["list"], true)["revision"].clone();
    let protected = s.call(
        "b",
        &[
            "--role",
            "agent",
            "--op-id",
            "protected",
            "new",
            "page",
            "--title",
            "Review needed",
            "--body",
            "x",
        ],
        true,
    );
    assert!(
        protected["data"]["branch"]
            .as_str()
            .unwrap()
            .starts_with("proposal/")
    );
    assert_eq!(before, s.call("a", &["list"], true)["revision"]);
    let mut children = vec![];
    for n in 0..2 {
        children.push(
            s.client(
                &format!("linker{n}"),
                &[
                    "--op-id",
                    &format!("link{n}"),
                    "link",
                    &ids[n],
                    "chapter",
                    &ids[1 - n],
                    "--expect",
                    "1",
                ],
            )
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
        );
    }
    let results: Vec<_> = children
        .into_iter()
        .map(|c| c.wait_with_output().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|o| o.status.success()).count(), 1);
    assert!(
        results
            .iter()
            .filter(|o| !o.status.success())
            .any(|o| String::from_utf8_lossy(&o.stdout).contains("cycle"))
    );
    s.call("a", &["lint"], true);
    let r = Server::empty();
    r.init(Some(
        manifest.join("examples/research.yaml").to_str().unwrap(),
    ));
    let trial = r.call(
        "a",
        &[
            "--op-id", "trial", "new", "trial", "--title", "Trial", "--body", "observed",
            "--field", "count=0",
        ],
        true,
    );
    let id = trial["data"]["id"].as_str().unwrap();
    assert_eq!(
        r.call("a", &["show", id], true)["data"]["record"]["fields"]["count"],
        0
    );
    r.call(
        "a",
        &[
            "--op-id",
            "invalid",
            "new",
            "trial",
            "--title",
            "Missing field",
            "--body",
            "x",
        ],
        false,
    );
    r.call("a", &["lint"], true);
}

#[test]
fn shared_cli_lost_ack_and_implicit_edit_body_replay_after_later_write() {
    let mut s = Server::empty();
    s.init(None);
    let args = [
        "--op-id", "lost-new", "new", "note", "--title", "Source", "--body", "first",
    ];
    let actual_port = s.port;
    let (port, proxy) = lose_commit_ack(actual_port);
    s.port = port;
    let failed = s.call("a", &args, false);
    assert!(
        failed.to_string().contains("indeterminate_commit"),
        "{failed}"
    );
    proxy.join().unwrap();
    s.port = actual_port;
    let recovered = s.call("b", &args, true);
    let id = recovered["data"]["id"].as_str().unwrap();
    assert_eq!(s.call("b", &["list"], true)["consumed"]["records"][id], 1);
    assert_eq!(
        s.call("b", &["walk", id], true)["consumed"]["records"][id],
        1
    );
    let title = [
        "--op-id",
        "title-only",
        "edit",
        id,
        "--expect",
        "1",
        "--title",
        "Updated",
    ];
    let first = s.call("a", &title, true);
    s.call(
        "b",
        &[
            "--op-id",
            "later",
            "edit",
            id,
            "--expect",
            "2",
            "--body",
            "later body",
        ],
        true,
    );
    assert_eq!(first["data"], s.call("b", &title, true)["data"]);
    assert_eq!(
        s.call("b", &["show", id], true)["data"]["record"]["body"],
        "later body"
    );
    s.stop();
    s.start();
    assert_eq!(first["data"], s.call("b", &title, true)["data"]);
}

#[test]
fn shared_initialization_refuses_existing_partial_and_noninitial_stores() {
    for sql in [
        "CREATE TABLE custom (id INT PRIMARY KEY)",
        "CREATE TABLE metadata (id VARCHAR(128) PRIMARY KEY, payload LONGTEXT); INSERT INTO metadata VALUES ('schema','4')",
        "CREATE TABLE metadata (id VARCHAR(128) PRIMARY KEY, payload LONGTEXT); INSERT INTO metadata VALUES ('schema','5')",
        "CALL DOLT_BRANCH('extra')",
        "CALL DOLT_COMMIT('--allow-empty','-m','retained history','--author','Test <test@localhost>'); CALL DOLT_RESET('--hard','HEAD~1')",
        "CALL DOLT_COMMIT('--allow-empty','-m','retained history','--author','Test <test@localhost>'); CALL DOLT_TAG('prior'); CALL DOLT_RESET('--hard','HEAD~1')",
        "CALL DOLT_COMMIT('--allow-empty','-m','noninitial','--author','Test <test@localhost>')",
    ] {
        let s = Server::empty();
        let mut raw = s.raw("main");
        raw.query_drop(sql).unwrap();
        if sql.starts_with("CREATE") {
            raw.query_drop("CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','fixture','--author','Test <test@localhost>')").unwrap();
        }
        let before: String = raw
            .query_first("SELECT DOLT_HASHOF('HEAD')")
            .unwrap()
            .unwrap();
        s.call("a", &["init", "--dry-run"], false);
        s.call(
            "a",
            &[
                "init",
                "--op-id",
                "bad",
                "--expect-base",
                &before,
                "--expect-schema",
                "bad",
            ],
            false,
        );
        assert_eq!(
            Some(before),
            raw.query_first::<String, _>("SELECT DOLT_HASHOF('HEAD')")
                .unwrap()
        );
        assert!(
            raw.query::<String, _>("SELECT table_name FROM dolt_status")
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn shared_initialization_lost_ack_reconciles_on_another_client() {
    let mut s = Server::empty();
    let plan = s.call("a", &["init", "--dry-run"], true);
    let args = [
        "init",
        "--op-id",
        "lost-init",
        "--expect-base",
        plan["data"]["base_revision"].as_str().unwrap(),
        "--expect-schema",
        plan["data"]["ontology_digest"].as_str().unwrap(),
    ];
    let actual_port = s.port;
    let (port, proxy) = lose_matching_ack(actual_port, literal("initialize shared Scrolls"));
    s.port = port;
    assert!(
        s.call("a", &args, false)
            .to_string()
            .contains("indeterminate_commit")
    );
    proxy.join().unwrap();
    s.port = actual_port;
    let recovered = s.call("b", &args, true);
    assert_ne!(recovered["revision"], plan["revision"]);
    s.call("b", &["doctor"], true);
}

#[test]
fn shared_input_files_replay_from_different_client_paths() {
    let s = Server::empty();
    s.init(None);
    let a = s.dir.path().join("first.json");
    let b = s.dir.path().join("elsewhere.json");
    let batch = json!([{"op":"new","kind":"note","title":"portable","body":"same bytes"}]);
    for file in [&a, &b] {
        fs::write(file, serde_json::to_vec(&batch).unwrap()).unwrap();
    }
    let first = s.call(
        "a",
        &["--op-id", "portable-batch", "batch", a.to_str().unwrap()],
        true,
    );
    fs::remove_file(&a).unwrap();
    assert_eq!(
        first,
        s.call(
            "b",
            &["--op-id", "portable-batch", "batch", b.to_str().unwrap()],
            true
        )
    );
    let body_a = s.dir.path().join("body-a.md");
    let body_b = s.dir.path().join("body-b.md");
    for file in [&body_a, &body_b] {
        fs::write(file, "portable body").unwrap();
    }
    let first = s.call(
        "a",
        &[
            "--op-id",
            "portable-body",
            "new",
            "note",
            "--title",
            "Body",
            "--body-file",
            body_a.to_str().unwrap(),
        ],
        true,
    );
    fs::remove_file(&body_a).unwrap();
    assert_eq!(
        first,
        s.call(
            "b",
            &[
                "--op-id",
                "portable-body",
                "new",
                "note",
                "--title",
                "Body",
                "--body-file",
                body_b.to_str().unwrap()
            ],
            true
        )
    );
    fs::write(&body_b, "changed input").unwrap();
    assert!(
        s.call(
            "b",
            &[
                "--op-id",
                "portable-body",
                "new",
                "note",
                "--title",
                "Body",
                "--body-file",
                body_b.to_str().unwrap()
            ],
            false
        )
        .to_string()
        .contains("different arguments")
    );
}

#[test]
fn pinned_030_executable_refuses_shared_format_without_database_changes() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = manifest.join(".verification/scrolls-0.3.0");
    let build_dir = build_pinned(&source, "3ae0a38dcc4da0a508e82ce4737d226e06d6f2b4");
    let target = build_dir.path();
    let mut s = Server::empty();
    let initialized = s.init(None);
    s.stop();
    // SIGKILL can leave the server marker. It is not the writer fence being tested.
    let marker = s.dir.path().join("knowledge/.dolt/sql-server.info");
    if marker.exists() {
        fs::remove_file(marker).unwrap();
    }
    let root = s.dir.path().join("old-client");
    fs::create_dir(&root).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(s.dir.path().join("knowledge"), root.join("dolt")).unwrap();
    #[cfg(not(unix))]
    panic!("qualification requires Unix symlink support");
    let out = Command::new(target.join("debug/scrolls"))
        .args([
            "--json",
            "--actor",
            "old",
            "--role",
            "human",
            "--op-id",
            "must-refuse",
            "--store",
        ])
        .arg(root)
        .arg("--dolt")
        .arg(Server::binary())
        .args(["new", "note", "--title", "must not write", "--body", "x"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let output = String::from_utf8_lossy(&out.stdout);
    assert!(output.contains("unsupported store schema"), "{output}");
    assert!(!output.contains("unexpected Dolt server"));
    s.start();
    assert_eq!(
        initialized["revision"],
        s.call("a", &["list"], true)["revision"]
    );
    assert!(
        s.raw("main")
            .query::<String, _>("SELECT table_name FROM dolt_status")
            .unwrap()
            .is_empty()
    );
}

impl Server {
    fn proposed_page(&self, op: &str, title: &str) -> serde_json::Value {
        self.call(
            "author",
            &[
                "--role",
                "agent",
                "--op-id",
                op,
                "new",
                "page",
                "--title",
                title,
                "--body",
                "proposal text",
            ],
            true,
        )["data"]
            .clone()
    }
    fn token(&self, branch: &str) -> String {
        self.call("reviewer", &["review", branch], true)["data"]["review_token"]
            .as_str()
            .unwrap()
            .into()
    }
    fn intent(&self, op: &str) -> serde_json::Value {
        let id = format!("intent:{}", scrolls::model::digest(op));
        let text: String = self
            .raw("scrolls-operations")
            .query_first(format!(
                "SELECT payload FROM metadata WHERE id={}",
                literal(&id)
            ))
            .unwrap()
            .unwrap();
        serde_json::from_str(&text).unwrap()
    }
}
#[test]
fn shared_review_propose_inspect_accept_reject_and_configuration() {
    let s = Server::empty();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    s.init(Some(
        manifest.join("examples/handbook.yaml").to_str().unwrap(),
    ));
    let p = s.proposed_page("page-proposal", "Page");
    let branch = p["branch"].as_str().unwrap();
    let id = p["id"].as_str().unwrap();
    assert!(
        s.call("reader", &["list"], true)["data"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let original = s.call(
        "other",
        &[
            "--role",
            "agent",
            "--op-id",
            "page-proposal",
            "new",
            "page",
            "--title",
            "Page",
            "--body",
            "proposal text",
        ],
        true,
    );
    assert_eq!(original["data"], p);
    assert_eq!(original["branch"], branch);
    let token = s.token(branch);
    let preview = s.call(
        "reviewer",
        &[
            "--dry-run",
            "review",
            "accept",
            branch,
            "--review-token",
            &token,
        ],
        true,
    );
    assert_eq!(preview["data"]["validated"], true);
    assert!(
        s.call(
            "agent",
            &[
                "--role",
                "agent",
                "--op-id",
                "bad-accept",
                "review",
                "accept",
                branch,
                "--review-token",
                &token
            ],
            false
        )
        .to_string()
        .contains("human")
    );
    let accepted = s.call(
        "reviewer",
        &[
            "--op-id",
            "accept-page",
            "review",
            "accept",
            branch,
            "--review-token",
            &token,
        ],
        true,
    );
    assert_eq!(accepted["data"]["proposal"]["status"], "accepted");
    assert_eq!(
        s.call("reader", &["show", id], true)["data"]["record"]["title"],
        "Page"
    );
    assert_eq!(
        accepted["data"],
        s.call(
            "other",
            &[
                "--op-id",
                "accept-page",
                "review",
                "accept",
                branch,
                "--review-token",
                &token
            ],
            true
        )["data"]
    );
    let p = s.proposed_page("rejected-page", "Rejected");
    let branch = p["branch"].as_str().unwrap();
    let rejected = s.call(
        "reviewer",
        &[
            "--op-id",
            "reject-page",
            "review",
            "reject",
            branch,
            "--reason",
            "not wanted",
        ],
        true,
    );
    assert_eq!(rejected["data"]["proposal"]["status"], "rejected");
    assert_eq!(
        rejected["data"],
        s.call(
            "other",
            &[
                "--op-id",
                "reject-page",
                "review",
                "reject",
                branch,
                "--reason",
                "not wanted"
            ],
            true
        )["data"]
    );
    assert!(
        s.call(
            "other",
            &[
                "--op-id",
                "reject-page",
                "review",
                "reject",
                branch,
                "--reason",
                "different"
            ],
            false
        )
        .to_string()
        .contains("different arguments")
    );
    let schema = s.call("reader", &["schema", "show"], true)["data"]["schema_revision"]
        .as_str()
        .unwrap()
        .to_string();
    let changed = s.dir.path().join("schema.yaml");
    let mut text = fs::read_to_string(manifest.join("examples/handbook.yaml")).unwrap();
    text.push_str("\n# A reviewed configuration update\n");
    text = text.replace("A handbook page", "A reviewed handbook page");
    fs::write(&changed, text).unwrap();
    let proposal = s.call(
        "author",
        &[
            "--op-id",
            "schema-change",
            "schema",
            "apply",
            changed.to_str().unwrap(),
            "--expect",
            &schema,
        ],
        true,
    );
    let branch = proposal["data"]["branch"].as_str().unwrap();
    assert_eq!(
        s.call("reader", &["schema", "show"], true)["data"]["schema_revision"],
        schema
    );
    let token = s.token(branch);
    s.call(
        "reviewer",
        &[
            "--op-id",
            "schema-accept",
            "review",
            "accept",
            branch,
            "--review-token",
            &token,
        ],
        true,
    );
    assert_ne!(
        s.call("reader", &["schema", "show"], true)["data"]["schema_revision"],
        schema
    );
}

impl Server {
    fn interrupted(&mut self, args: &[&str], needle: String) {
        let actual = self.port;
        let (port, proxy) = lose_matching_ack(actual, needle);
        self.port = port;
        self.call("lost-client", args, false);
        proxy.join().unwrap();
        self.port = actual;
        fs::remove_dir_all(self.dir.path().join("lost-client")).unwrap();
    }
    fn handbook() -> Self {
        let s = Self::empty();
        s.init(Some(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("examples/handbook.yaml")
                .to_str()
                .unwrap(),
        ));
        s
    }
    fn main_head(&self) -> String {
        self.raw("main")
            .query_first("SELECT DOLT_HASHOF('HEAD')")
            .unwrap()
            .unwrap()
    }
}
#[test]
fn shared_review_recovers_each_acceptance_boundary_without_origin_files() {
    for boundary in [
        "CALL DOLT_BRANCH(",
        "SELECT * FROM dolt_conflicts",
        "accept-commit",
        "CALL DOLT_MERGE('--ff-only'",
    ] {
        let mut s = Server::handbook();
        let p = s.proposed_page("propose", "Recover me");
        let branch = p["branch"].as_str().unwrap();
        let token = s.token(branch);
        let before = s.main_head();
        let needle = if boundary == "accept-commit" {
            literal(&format!("accept {branch} by alice"))
        } else {
            boundary.into()
        };
        s.interrupted(
            &[
                "--op-id",
                "accept",
                "review",
                "accept",
                branch,
                "--review-token",
                &token,
            ],
            needle,
        );
        let intent = s.intent("accept");
        assert_eq!(intent["plan"]["base"], before);
        let r = s.call("second-machine", &["recover", "accept", "--resume"], true);
        assert_eq!(r["data"]["proposal"]["status"], "accepted");
        assert_ne!(s.main_head(), before);
        assert_eq!(
            r["data"],
            s.call("third-machine", &["recover", "accept", "--resume"], true)["data"]
        );
        s.call("reader", &["doctor"], true);
    }
}
#[test]
fn shared_review_candidate_is_not_publication_and_stale_recovery_never_refreshes() {
    let mut s = Server::handbook();
    let p = s.proposed_page("propose", "Candidate");
    let branch = p["branch"].as_str().unwrap();
    let token = s.token(branch);
    let before = s.main_head();
    s.interrupted(
        &[
            "--op-id",
            "accept",
            "review",
            "accept",
            branch,
            "--review-token",
            &token,
        ],
        literal(&format!("accept {branch} by alice")),
    );
    assert_eq!(s.main_head(), before);
    s.call(
        "other",
        &[
            "--op-id",
            "advance",
            "new",
            "page",
            "--title",
            "Later",
            "--body",
            "unrelated",
        ],
        true,
    );
    let later = s.main_head();
    let error = s.call("second", &["recover", "accept", "--resume"], false);
    assert!(error.to_string().contains("stale"), "{error}");
    assert_eq!(s.main_head(), later);
    let inspected = s.call("second", &["recover", "accept"], true);
    assert_eq!(inspected["data"]["published"], false);
    s.call(
        "second",
        &[
            "--op-id",
            "cancel",
            "recover",
            "accept",
            "--abandon",
            "--reason",
            "Reinspect against changed main",
        ],
        true,
    );
    assert!(
        s.call("second", &["recover", "accept", "--resume"], false)
            .to_string()
            .contains("abandoned")
    );
    assert_eq!(s.main_head(), later);
}
#[test]
fn shared_review_published_receipt_survives_later_main_advance() {
    let mut s = Server::handbook();
    let p = s.proposed_page("propose", "Published");
    let branch = p["branch"].as_str().unwrap();
    let token = s.token(branch);
    s.interrupted(
        &[
            "--op-id",
            "accept",
            "review",
            "accept",
            branch,
            "--review-token",
            &token,
        ],
        "CALL DOLT_MERGE('--ff-only'".into(),
    );
    s.call(
        "other",
        &[
            "--op-id",
            "advance",
            "new",
            "page",
            "--title",
            "Later",
            "--body",
            "unrelated",
        ],
        true,
    );
    let later = s.main_head();
    assert_eq!(
        s.call("second", &["recover", "accept", "--resume"], true)["data"]["proposal"]["status"],
        "accepted"
    );
    assert_eq!(s.main_head(), later);
}

#[test]
fn shared_review_preserves_staged_only_foreign_data() {
    let mut s = Server::handbook();
    let p = s.proposed_page("propose", "Staged protection");
    let branch = p["branch"].as_str().unwrap();
    let token = s.token(branch);
    s.interrupted(
        &[
            "--op-id",
            "accept",
            "review",
            "accept",
            branch,
            "--review-token",
            &token,
        ],
        "SELECT * FROM dolt_conflicts".into(),
    );
    let intent = s.intent("accept");
    let temp = intent["plan"]["branch"].as_str().unwrap();
    let mut raw = s.raw(temp);
    raw.query_drop("INSERT INTO metadata VALUES ('foreign','{}'); CALL DOLT_ADD('.'); DELETE FROM metadata WHERE id='foreign'").unwrap();
    let staged: String = raw
        .query_first("SELECT DOLT_HASHOF_DB('STAGED')")
        .unwrap()
        .unwrap();
    let working: String = raw
        .query_first("SELECT DOLT_HASHOF_DB('WORKING')")
        .unwrap()
        .unwrap();
    for args in [
        vec!["recover", "accept", "--resume"],
        vec![
            "--op-id",
            "cancel",
            "recover",
            "accept",
            "--abandon",
            "--reason",
            "cannot erase foreign work",
        ],
    ] {
        s.call("second", &args, false);
        assert_eq!(
            raw.query_first::<String, _>("SELECT DOLT_HASHOF_DB('STAGED')")
                .unwrap()
                .unwrap(),
            staged
        );
        assert_eq!(
            raw.query_first::<String, _>("SELECT DOLT_HASHOF_DB('WORKING')")
                .unwrap()
                .unwrap(),
            working
        );
    }
}
#[test]
fn shared_review_resumes_interrupted_abandonment_control() {
    let mut s = Server::handbook();
    let p = s.proposed_page("propose", "Cancel");
    let branch = p["branch"].as_str().unwrap();
    let token = s.token(branch);
    s.interrupted(
        &[
            "--op-id",
            "accept",
            "review",
            "accept",
            branch,
            "--review-token",
            &token,
        ],
        "SELECT * FROM dolt_conflicts".into(),
    );
    s.interrupted(
        &[
            "--op-id",
            "cancel",
            "recover",
            "accept",
            "--abandon",
            "--reason",
            "Owner cancelled",
        ],
        literal("record shared operation recovery"),
    );
    let result = s.call("second", &["recover", "cancel", "--resume"], true);
    assert_eq!(result["data"]["abandoned"], "accept");
    s.call("reader", &["doctor"], true);
}

fn build_pinned(source: &Path, revision: &str) -> tempfile::TempDir {
    let pin = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("pinned baseline checkout required, see docs/shared-authoring.md");
    assert!(pin.status.success(), "pinned baseline checkout required");
    assert_eq!(String::from_utf8_lossy(&pin.stdout).trim(), revision);
    let clean = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        clean.status.success() && clean.stdout.is_empty(),
        "baseline source must be clean"
    );
    let build_dir = tempfile::tempdir().unwrap();
    let target = build_dir.path();
    let build = Command::new("cargo")
        .args(["build", "--locked", "--bin", "scrolls"])
        .current_dir(source)
        .env("CARGO_TARGET_DIR", target)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    build_dir
}

#[test]
fn prior_shared_writer_executable_refuses_current_generation() {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".verification/scrolls-shared-previous");
    let build = build_pinned(&source, "a4bf897811abbb19657a824b5eb3470a2733ce16");
    let s = Server::handbook();
    let before = s.main_head();
    let client = s.client(
        "old",
        &[
            "--op-id",
            "must-refuse",
            "new",
            "page",
            "--title",
            "No",
            "--body",
            "x",
        ],
    );
    let out = Command::new(build.path().join("debug/scrolls"))
        .args(client.get_args())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("unsupported_shared_writer"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(s.main_head(), before);
    s.call("new", &["doctor"], true);
}
#[test]
fn shared_review_proposal_rejection_recover_and_operation_ids_are_reserved() {
    let mut s = Server::handbook();
    let args = [
        "--role",
        "agent",
        "--op-id",
        "propose",
        "new",
        "page",
        "--title",
        "Interrupted",
        "--body",
        "durable",
    ];
    s.interrupted(&args, "CALL DOLT_BRANCH(".into());
    let changed = s.call(
        "second",
        &[
            "--role",
            "agent",
            "--op-id",
            "propose",
            "new",
            "page",
            "--title",
            "Different",
            "--body",
            "durable",
        ],
        false,
    );
    assert!(changed.to_string().contains("different arguments"));
    let p = s.call(
        "second",
        &["--role", "agent", "recover", "propose", "--resume"],
        true,
    );
    let branch = p["data"]["branch"].as_str().unwrap();
    s.interrupted(
        &[
            "--op-id", "reject", "review", "reject", branch, "--reason", "No",
        ],
        literal(&format!("reject {branch} by alice")),
    );
    let r = s.call("third", &["recover", "reject", "--resume"], true);
    assert_eq!(r["data"]["proposal"]["status"], "rejected");
    s.call(
        "third",
        &[
            "--op-id",
            "reject",
            "new",
            "page",
            "--title",
            "cannot reuse",
            "--body",
            "x",
        ],
        false,
    );
}
#[test]
fn shared_partial_initialization_is_never_implicitly_repaired() {
    let mut s = Server::empty();
    let p = s.call("a", &["init", "--dry-run"], true);
    let args = [
        "init",
        "--op-id",
        "init",
        "--expect-base",
        p["data"]["base_revision"].as_str().unwrap(),
        "--expect-schema",
        p["data"]["ontology_digest"].as_str().unwrap(),
    ];
    s.interrupted(&args, "CALL DOLT_COMMIT(".into());
    let before = s.main_head();
    s.call("b", &args, false);
    s.call("b", &["list"], false);
    assert_eq!(s.main_head(), before);
}

#[test]
fn shared_review_concurrent_acceptance_and_graph_cycle_merge_fail_closed() {
    let s = Server::handbook();
    let ids: Vec<String> = (0..2)
        .map(|n| {
            s.call(
                "writer",
                &[
                    "--op-id",
                    &format!("page{n}"),
                    "new",
                    "page",
                    "--title",
                    "Page",
                    "--body",
                    "x",
                ],
                true,
            )["data"]["id"]
                .as_str()
                .unwrap()
                .into()
        })
        .collect();
    let proposals: Vec<String> = (0..2)
        .map(|n| {
            s.call(
                "agent",
                &[
                    "--role",
                    "agent",
                    "--op-id",
                    &format!("link{n}"),
                    "link",
                    &ids[n],
                    "chapter",
                    &ids[1 - n],
                    "--expect",
                    "1",
                ],
                true,
            )["data"]["branch"]
                .as_str()
                .unwrap()
                .into()
        })
        .collect();
    let tokens: Vec<String> = proposals.iter().map(|p| s.token(p)).collect();
    let children: Vec<_> = (0..2)
        .map(|n| {
            s.client(
                &format!("reviewer{n}"),
                &[
                    "--op-id",
                    &format!("accept{n}"),
                    "review",
                    "accept",
                    &proposals[n],
                    "--review-token",
                    &tokens[n],
                ],
            )
            .stdout(Stdio::piped())
            .spawn()
            .unwrap()
        })
        .collect();
    let results: Vec<_> = children
        .into_iter()
        .map(|c| c.wait_with_output().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|o| o.status.success()).count(), 1);
    let failed = results.iter().position(|o| !o.status.success()).unwrap();
    assert!(String::from_utf8_lossy(&results[failed].stdout).contains("stale"));
    let before = s.main_head();
    let token = s.token(&proposals[failed]);
    let error = s.call(
        "reviewer",
        &[
            "--op-id",
            "reinspect",
            "review",
            "accept",
            &proposals[failed],
            "--review-token",
            &token,
        ],
        false,
    );
    assert!(error.to_string().contains("cycle"), "{error}");
    assert_eq!(s.main_head(), before);
    s.call("reader", &["doctor"], true);
}
#[test]
fn shared_review_research_ontology_and_stale_proposal_token() {
    let s = Server::empty();
    s.init(Some(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples/research.yaml")
            .to_str()
            .unwrap(),
    ));
    let file = s.dir.path().join("research.json");
    fs::write(&file,serde_json::to_vec(&json!([
      {"op":"new","handle":"trial","kind":"trial","title":"Measurement","fields":{"count":3}},
      {"op":"new","handle":"claim","kind":"hypothesis","title":"Claim","fields":{"claim":"Measurable"}},
      {"op":"link","from":"$trial","rel":"corroborates","to":"$claim","why":"measurement"}
    ])).unwrap()).unwrap();
    let p = s.call(
        "researcher",
        &["--op-id", "research", "batch", file.to_str().unwrap()],
        true,
    );
    let branch = p["data"]["branch"].as_str().unwrap();
    let id = p["data"]["handles"]["claim"].as_str().unwrap();
    let token = s.token(branch);
    s.call(
        "researcher",
        &[
            "--proposal",
            branch,
            "--op-id",
            "revision",
            "edit",
            id,
            "--expect",
            "1",
            "--body",
            "More evidence",
        ],
        true,
    );
    let before = s.main_head();
    assert!(
        s.call(
            "reviewer",
            &[
                "--op-id",
                "stale",
                "review",
                "accept",
                branch,
                "--review-token",
                &token
            ],
            false
        )
        .to_string()
        .contains("stale")
    );
    assert_eq!(s.main_head(), before);
    let token = s.token(branch);
    s.call(
        "reviewer",
        &[
            "--op-id",
            "accept",
            "review",
            "accept",
            branch,
            "--review-token",
            &token,
        ],
        true,
    );
    s.call("reader", &["lint"], true);
}

#[test]
fn shared_review_preserves_hidden_versioned_trigger_state() {
    for staged_only in [false, true] {
        let mut s = Server::handbook();
        let p = s.proposed_page("propose", "Hidden state");
        let branch = p["branch"].as_str().unwrap();
        let token = s.token(branch);
        s.interrupted(
            &[
                "--op-id",
                "accept",
                "review",
                "accept",
                branch,
                "--review-token",
                &token,
            ],
            "SELECT * FROM dolt_conflicts".into(),
        );
        let intent = s.intent("accept");
        let temp = intent["plan"]["branch"].as_str().unwrap();
        let mut raw = s.raw(temp);
        raw.query_drop("CREATE TRIGGER foreign_trigger BEFORE INSERT ON records FOR EACH ROW SET NEW.payload = NEW.payload").unwrap();
        if staged_only {
            raw.query_drop("CALL DOLT_ADD('.'); DROP TRIGGER foreign_trigger")
                .unwrap();
        }
        let staged: String = raw
            .query_first("SELECT DOLT_HASHOF_DB('STAGED')")
            .unwrap()
            .unwrap();
        let working: String = raw
            .query_first("SELECT DOLT_HASHOF_DB('WORKING')")
            .unwrap()
            .unwrap();
        for args in [
            vec!["recover", "accept", "--resume"],
            vec![
                "--op-id",
                "cancel",
                "recover",
                "accept",
                "--abandon",
                "--reason",
                "Preserve foreign state",
            ],
        ] {
            let error = s.call("second", &args, false);
            assert!(error.to_string().contains("dirty_state"), "{error}");
            assert_eq!(
                raw.query_first::<String, _>("SELECT DOLT_HASHOF_DB('STAGED')")
                    .unwrap()
                    .unwrap(),
                staged
            );
            assert_eq!(
                raw.query_first::<String, _>("SELECT DOLT_HASHOF_DB('WORKING')")
                    .unwrap()
                    .unwrap(),
                working
            );
        }
    }
}

#[test]
fn shared_review_default_ontology_conflicting_proposals_preserve_main() {
    let s = Server::empty();
    s.init(None);
    let e = s.call(
        "a",
        &[
            "--op-id", "evidence", "new", "evidence", "--title", "Observed", "--body", "facts",
        ],
        true,
    );
    let file = s.dir.path().join("supported.json");
    fs::write(
        &file,
        serde_json::to_vec(&json!([
          {"op":"new","handle":"doc","kind":"knowledge","title":"Knowledge","body":"supported"},
          {"op":"link","from":e["data"]["id"],"rel":"supports","to":"$doc","expect":1}
        ]))
        .unwrap(),
    )
    .unwrap();
    let p = s.call(
        "a",
        &["--op-id", "knowledge", "batch", file.to_str().unwrap()],
        true,
    );
    let branch = p["data"]["branch"].as_str().unwrap();
    let id = p["data"]["handles"]["doc"].as_str().unwrap();
    s.call(
        "b",
        &[
            "--op-id",
            "initial-review",
            "review",
            "accept",
            branch,
            "--review-token",
            &s.token(branch),
        ],
        true,
    );
    let branches: Vec<String> = ["one", "two"]
        .iter()
        .map(|body| {
            s.call(
                "a",
                &["--op-id", body, "edit", id, "--expect", "1", "--body", body],
                true,
            )["data"]["branch"]
                .as_str()
                .unwrap()
                .into()
        })
        .collect();
    s.call(
        "b",
        &[
            "--op-id",
            "review-one",
            "review",
            "accept",
            &branches[0],
            "--review-token",
            &s.token(&branches[0]),
        ],
        true,
    );
    let before = s.main_head();
    let error = s.call(
        "b",
        &[
            "--op-id",
            "review-two",
            "review",
            "accept",
            &branches[1],
            "--review-token",
            &s.token(&branches[1]),
        ],
        false,
    );
    assert!(error.to_string().contains("merge_conflict"), "{error}");
    assert_eq!(s.main_head(), before);
    s.call("b", &["doctor"], true);
}

#[test]
fn shared_committed_row_identity_mismatch_is_refused() {
    let s = Server::handbook();
    s.call(
        "a",
        &[
            "--op-id", "page", "new", "page", "--title", "Page", "--body", "x",
        ],
        true,
    );
    let mut raw = s.raw("main");
    raw.query_drop("UPDATE records SET id='wrong'; CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','foreign identity mismatch','--author','Raw <raw@localhost>')").unwrap();
    let before = s.main_head();
    let error = s.call("b", &["list"], false);
    assert!(error.to_string().contains("stored_row_identity"), "{error}");
    assert_eq!(s.main_head(), before);
}

fn import_source(s: &Server) -> PathBuf {
    let repo = s.dir.path().join("source");
    fs::create_dir_all(repo.join("chapters")).unwrap();
    fs::write(
        repo.join("chapters/one.md"),
        "---\nid: one\ntitle: First\n---\nCaptured; text ' 💾\n",
    )
    .unwrap();
    fs::write(repo.join("README.md"), "# Overview\n").unwrap();
    fs::write(repo.join("asset.bin"), [0, 255, 1]).unwrap();
    fs::write(repo.join("run.sh"), "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(repo.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@localhost"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["config", "core.hooksPath", "/dev/null"],
        vec!["add", "."],
        vec!["commit", "-qm", "source"],
    ] {
        let out = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    repo
}
impl Server {
    fn import_plan(&self, repo: &Path, role: &str, custom: bool, name: &str) -> PathBuf {
        let plan = self
            .dir
            .path()
            .join(format!("{}.plan.json", uuid::Uuid::new_v4()));
        let mut args = vec![
            "--role",
            role,
            "import",
            "plan",
            "git",
            repo.to_str().unwrap(),
            "--root",
            ".",
            "--rev",
            "HEAD",
            "--source",
            name,
            "--out",
            plan.to_str().unwrap(),
        ];
        let profile = self.dir.path().join("profile.yaml");
        if custom {
            fs::write(
                &profile,
                include_str!("../profiles/handbook.yaml")
                    .replace("literal: evidence", "literal: page")
                    .replace("source_updated:", "updated:"),
            )
            .unwrap();
            args.extend(["--profile-file", profile.to_str().unwrap()]);
        } else {
            args.extend(["--profile", "handbook"]);
        }
        self.call("planner", &args, true);
        plan
    }
}
#[test]
fn shared_import_direct_fidelity_and_replay_without_source_checkout() {
    let mut s = Server::empty();
    s.init(None);
    let repo = import_source(&s);
    let before = s.main_head();
    let plan = s.import_plan(&repo, "human", false, "book");
    assert_eq!(s.main_head(), before);
    s.call("a", &["import", "profiles", "list"], true);
    let args = [
        "--op-id",
        "import",
        "import",
        "apply",
        plan.to_str().unwrap(),
    ];
    s.interrupted(&args, literal("import source book by alice"));
    let receipt: String = s
        .raw("main")
        .query_first("SELECT payload FROM operations WHERE id='import'")
        .unwrap()
        .unwrap();
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    let written = json!({"data":receipt["result"]});
    assert_ne!(s.main_head(), before);
    let record = s.call("b", &["show", "book:chapter/one", "--provenance"], true);
    assert!(
        record["data"]["record"]["body"]
            .as_str()
            .unwrap()
            .contains("Captured; text")
    );
    let id = record["data"]["record"]["id"].as_str().unwrap();
    assert!(
        s.call(
            "b",
            &[
                "--op-id",
                "edit-capture",
                "edit",
                id,
                "--expect",
                "1",
                "--body",
                "changed"
            ],
            false
        )
        .to_string()
        .contains("source_ownership")
    );
    let export = s.dir.path().join("export");
    s.call("b", &["render", "--out", export.to_str().unwrap()], true);
    for path in ["chapters/one.md", "README.md", "asset.bin", "run.sh"] {
        assert_eq!(
            fs::read(repo.join(path)).unwrap(),
            fs::read(export.join("sources/book").join(path)).unwrap()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            fs::metadata(export.join("sources/book/run.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
    }
    let copied = s.dir.path().join("copied-plan.json");
    fs::copy(&plan, &copied).unwrap();
    fs::remove_file(plan).unwrap();
    fs::remove_dir_all(repo).unwrap();
    s.call(
        "other",
        &[
            "--op-id",
            "later",
            "new",
            "note",
            "--title",
            "Later",
            "--body",
            "unrelated",
        ],
        true,
    );
    let later = s.main_head();
    let replay = s.call(
        "b",
        &[
            "--op-id",
            "import",
            "import",
            "apply",
            copied.to_str().unwrap(),
        ],
        true,
    );
    assert_eq!(written["data"], replay["data"]);
    assert_eq!(s.main_head(), later);
    s.call("b", &["doctor"], true);
}
#[test]
fn shared_import_prepared_namespace_reservation_and_cross_client_recovery() {
    let mut s = Server::handbook();
    let repo = import_source(&s);
    let plan = s.import_plan(&repo, "agent", true, "book");
    let second = s.import_plan(&repo, "agent", true, "book");
    s.interrupted(
        &[
            "--role",
            "agent",
            "--op-id",
            "import",
            "import",
            "apply",
            plan.to_str().unwrap(),
        ],
        literal("record shared operation recovery"),
    );
    let error = s.call(
        "other",
        &[
            "--role",
            "agent",
            "--op-id",
            "second",
            "import",
            "apply",
            second.to_str().unwrap(),
        ],
        false,
    );
    assert!(error.to_string().contains("source_reserved"), "{error}");
    fs::remove_file(plan).unwrap();
    fs::remove_file(second).unwrap();
    fs::remove_dir_all(repo).unwrap();
    let recovered = s.call(
        "other",
        &["--role", "agent", "recover", "import", "--resume"],
        true,
    );
    let branch = recovered["data"]["branch"].as_str().unwrap();
    let token = s.token(branch);
    s.call(
        "human",
        &[
            "--op-id",
            "accept",
            "review",
            "accept",
            branch,
            "--review-token",
            &token,
        ],
        true,
    );
    s.call("reader", &["show", "book:chapter/one"], true);
    s.call("reader", &["doctor"], true);
}

#[test]
fn shared_import_pins_original_git_objects_despite_replacements() {
    let s = Server::empty();
    s.init(None);
    let repo = import_source(&s);
    let original = fs::read(repo.join("chapters/one.md")).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("--no-replace-objects")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    let base = git(&["rev-parse", "HEAD"]);
    let original_blob = git(&["rev-parse", "HEAD:chapters/one.md"]);
    fs::write(
        repo.join("chapters/one.md"),
        String::from_utf8(original.clone())
            .unwrap()
            .replace("Captured", "Tampered"),
    )
    .unwrap();
    git(&["add", "chapters/one.md"]);
    git(&["commit", "-qm", "replacement"]);
    let newer = git(&["rev-parse", "HEAD"]);
    let substitute = git(&["rev-parse", "HEAD:chapters/one.md"]);
    git(&["replace", &original_blob, &substitute]);
    git(&["replace", &base, &newer]);
    let plan = s.dir.path().join("original.plan.json");
    let nested = repo.join("chapters");
    // A foreign GIT_DIR must not redirect the explicitly selected repository.
    let out = s
        .client(
            "a",
            &[
                "import",
                "plan",
                "git",
                nested.to_str().unwrap(),
                "--root",
                ".",
                "--rev",
                &base,
                "--source",
                "original",
                "--profile",
                "handbook",
                "--out",
                plan.to_str().unwrap(),
            ],
        )
        .env("GIT_DIR", s.dir.path().join("absent.git"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let p: serde_json::Value = serde_json::from_slice(&fs::read(&plan).unwrap()).unwrap();
    assert_eq!(p["source"]["commit"], base);
    s.call(
        "b",
        &[
            "--op-id",
            "import",
            "import",
            "apply",
            plan.to_str().unwrap(),
        ],
        true,
    );
    let export = s.dir.path().join("export");
    s.call("b", &["render", "--out", export.to_str().unwrap()], true);
    assert_eq!(
        fs::read(export.join("sources/original/chapters/one.md")).unwrap(),
        original
    );
}

#[test]
fn shared_import_packet_limit_refuses_without_partial_state() {
    let s = Server::empty();
    s.init(None);
    let repo = import_source(&s);
    fs::write(repo.join("large.bin"), vec![42; 100_000]).unwrap();
    for args in [
        ["add", "large.bin"].as_slice(),
        ["commit", "-qm", "large asset"].as_slice(),
    ] {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let plan = s.import_plan(&repo, "human", false, "large");
    let before = s.main_head();
    s.raw("main")
        .query_drop("SET GLOBAL max_allowed_packet=65536")
        .unwrap();
    let error = s.call(
        "a",
        &[
            "--op-id",
            "large",
            "import",
            "apply",
            plan.to_str().unwrap(),
        ],
        false,
    );
    assert!(error.to_string().contains("input_limit"), "{error}");
    assert_eq!(s.main_head(), before);
    assert_eq!(
        s.raw("main")
            .query_first::<u64, _>("SELECT COUNT(*) FROM import_sources")
            .unwrap()
            .unwrap(),
        0
    );
    assert!(
        s.raw("main")
            .query::<String, _>("SELECT table_name FROM dolt_status")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        s.call("a", &["doctor"], true)["data"]["shared_query_bytes"],
        65535
    );
    s.raw("main")
        .query_drop("SET GLOBAL max_allowed_packet=1073741824")
        .unwrap();
    s.call(
        "a",
        &[
            "--op-id",
            "large",
            "import",
            "apply",
            plan.to_str().unwrap(),
        ],
        true,
    );
    assert_eq!(
        s.call("a", &["doctor"], true)["data"]["import_health"]["files"],
        5
    );
}

#[test]
fn shared_import_stale_and_changed_bindings_do_not_publish() {
    for change in ["main", "schema", "actor", "plan"] {
        let s = Server::handbook();
        let repo = import_source(&s);
        let plan = s.import_plan(&repo, "agent", true, "book");
        if change == "main" {
            s.call(
                "other",
                &[
                    "--op-id", "later", "new", "page", "--title", "Later", "--body", "x",
                ],
                true,
            );
        } else if change == "schema" {
            let schema = s.call("other", &["schema", "show"], true)["data"]["schema_revision"]
                .as_str()
                .unwrap()
                .to_owned();
            let file = s.dir.path().join("updated-schema.yaml");
            fs::write(
                &file,
                include_str!("../examples/handbook.yaml")
                    .replace("A handbook page", "Updated handbook page"),
            )
            .unwrap();
            let p = s.call(
                "other",
                &[
                    "--op-id",
                    "schema",
                    "schema",
                    "apply",
                    file.to_str().unwrap(),
                    "--expect",
                    &schema,
                ],
                true,
            );
            let branch = p["data"]["branch"].as_str().unwrap();
            s.call(
                "other",
                &[
                    "--op-id",
                    "schema-review",
                    "review",
                    "accept",
                    branch,
                    "--review-token",
                    &s.token(branch),
                ],
                true,
            );
        } else if change == "plan" {
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&plan).unwrap()).unwrap();
            value["source"]["root"] = json!("changed");
            fs::write(&plan, serde_json::to_vec(&value).unwrap()).unwrap();
        }
        let before = s.main_head();
        let mut args = vec![
            "--role",
            "agent",
            "--op-id",
            "import",
            "import",
            "apply",
            plan.to_str().unwrap(),
        ];
        if change == "actor" {
            args.extend(["--actor", "bob"]);
        }
        let error = s.call("a", &args, false);
        assert!(
            error.to_string().contains("stale_plan"),
            "{change}: {error}"
        );
        assert_eq!(s.main_head(), before);
        assert_eq!(
            s.raw("main")
                .query_first::<u64, _>("SELECT COUNT(*) FROM import_sources")
                .unwrap()
                .unwrap(),
            0
        );
    }
    let a = Server::empty();
    a.init(None);
    let repo = import_source(&a);
    let plan = a.import_plan(&repo, "human", false, "book");
    let b = Server::empty();
    b.init(None);
    let before = b.main_head();
    assert!(
        b.call(
            "a",
            &[
                "--op-id",
                "import",
                "import",
                "apply",
                plan.to_str().unwrap()
            ],
            false
        )
        .to_string()
        .contains("wrong store")
    );
    assert_eq!(b.main_head(), before);
}

#[test]
fn shared_import_abandonment_releases_namespace_for_another_snapshot() {
    let mut s = Server::handbook();
    let repo = import_source(&s);
    let original = s.import_plan(&repo, "agent", true, "book");
    fs::write(
        repo.join("chapters/one.md"),
        "---\nid: one\ntitle: First\n---\nChanged snapshot\n",
    )
    .unwrap();
    for args in [
        ["add", "chapters/one.md"].as_slice(),
        ["commit", "-qm", "changed snapshot"].as_slice(),
    ] {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let changed = s.import_plan(&repo, "agent", true, "book");
    s.interrupted(
        &[
            "--role",
            "agent",
            "--op-id",
            "held",
            "import",
            "apply",
            original.to_str().unwrap(),
        ],
        literal("record shared operation recovery"),
    );
    assert!(
        s.call(
            "other",
            &[
                "--role",
                "agent",
                "--op-id",
                "new",
                "import",
                "apply",
                changed.to_str().unwrap()
            ],
            false
        )
        .to_string()
        .contains("source_reserved")
    );
    s.call(
        "owner",
        &[
            "--op-id",
            "cancel",
            "recover",
            "held",
            "--abandon",
            "--reason",
            "Use the newer snapshot",
        ],
        true,
    );
    let p = s.call(
        "other",
        &[
            "--role",
            "agent",
            "--op-id",
            "new",
            "import",
            "apply",
            changed.to_str().unwrap(),
        ],
        true,
    );
    let branch = p["data"]["branch"].as_str().unwrap();
    s.call(
        "owner",
        &[
            "--op-id",
            "accept",
            "review",
            "accept",
            branch,
            "--review-token",
            &s.token(branch),
        ],
        true,
    );
    assert!(
        s.call("reader", &["show", "book:chapter/one"], true)["data"]["record"]["body"]
            .as_str()
            .unwrap()
            .contains("Changed snapshot")
    );
    assert!(
        s.call(
            "other",
            &["--role", "agent", "recover", "held", "--resume"],
            false
        )
        .to_string()
        .contains("abandoned")
    );
}

#[test]
fn shared_import_concurrent_identical_apply_and_changed_input_replay() {
    let s = Server::empty();
    s.init(None);
    let repo = import_source(&s);
    let plan = s.import_plan(&repo, "human", false, "book");
    let children: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|op| {
            s.client(
                op,
                &["--op-id", op, "import", "apply", plan.to_str().unwrap()],
            )
            .stdout(Stdio::piped())
            .spawn()
            .unwrap()
        })
        .collect();
    let values: Vec<serde_json::Value> = children
        .into_iter()
        .map(|c| {
            let out = c.wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stdout)
            );
            serde_json::from_slice(&out.stdout).unwrap()
        })
        .collect();
    assert_eq!(
        values
            .iter()
            .filter(|v| v["data"]["identical_replay"] == true)
            .count(),
        1
    );
    assert_eq!(
        s.call("reader", &["doctor"], true)["data"]["import_health"]["files"],
        4
    );
    let before = s.main_head();
    let mut changed: scrolls::source_import::Plan =
        serde_json::from_slice(&fs::read(&plan).unwrap()).unwrap();
    changed.actor = "bob".into();
    changed.source.actor = "bob".into();
    changed.seal().unwrap();
    fs::write(&plan, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(
        s.call(
            "other",
            &[
                "--actor",
                "bob",
                "--op-id",
                "one",
                "import",
                "apply",
                plan.to_str().unwrap()
            ],
            false
        )
        .to_string()
        .contains("different arguments")
    );
    assert_eq!(s.main_head(), before);
}

#[test]
fn shared_import_corrupt_original_blocks_acceptance() {
    let s = Server::handbook();
    let repo = import_source(&s);
    let plan = s.import_plan(&repo, "agent", true, "book");
    let p = s.call(
        "author",
        &[
            "--role",
            "agent",
            "--op-id",
            "import",
            "import",
            "apply",
            plan.to_str().unwrap(),
        ],
        true,
    );
    let branch = p["data"]["branch"].as_str().unwrap();
    let mut raw = s.raw(branch);
    let (id, payload): (String, String) = raw
        .query_first("SELECT id,payload FROM import_files ORDER BY id LIMIT 1")
        .unwrap()
        .unwrap();
    let mut file: scrolls::source_import::File = serde_json::from_str(&payload).unwrap();
    file.file.sha256 = "0".repeat(64);
    raw.query_drop(format!("UPDATE import_files SET payload={} WHERE id={}; CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','foreign corruption','--author','Raw <raw@localhost>')", literal(&serde_json::to_string(&file).unwrap()), literal(&id))).unwrap();
    let before = s.main_head();
    let token = s.token(branch);
    let error = s.call(
        "owner",
        &[
            "--op-id",
            "accept",
            "review",
            "accept",
            branch,
            "--review-token",
            &token,
        ],
        false,
    );
    assert!(
        error.to_string().contains("checksum/mode mismatch"),
        "{error}"
    );
    assert_eq!(s.main_head(), before);
}

#[test]
fn shared_import_research_profile_preserves_zero_and_metadata() {
    let s = Server::empty();
    s.init(Some(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples/research.yaml")
            .to_str()
            .unwrap(),
    ));
    let repo = import_source(&s);
    let mut profile: serde_json::Value = serde_yaml::from_str(scrolls::profile::HANDBOOK).unwrap();
    for rule in profile["documents"].as_array_mut().unwrap() {
        rule["record"]["kind"] = json!({"literal":"trial"});
        rule["record"]["fields"] = json!({"count":{"literal":0}});
    }
    let file = s.dir.path().join("research-profile.json");
    fs::write(&file, serde_json::to_vec(&profile).unwrap()).unwrap();
    s.call(
        "reader",
        &["import", "profiles", "validate", file.to_str().unwrap()],
        true,
    );
    let plan = s.dir.path().join("research.plan.json");
    s.call(
        "planner",
        &[
            "import",
            "plan",
            "git",
            repo.to_str().unwrap(),
            "--root",
            ".",
            "--rev",
            "HEAD",
            "--source",
            "research",
            "--profile-file",
            file.to_str().unwrap(),
            "--out",
            plan.to_str().unwrap(),
        ],
        true,
    );
    s.call(
        "writer",
        &[
            "--op-id",
            "research",
            "import",
            "apply",
            plan.to_str().unwrap(),
        ],
        true,
    );
    assert_eq!(
        s.call(
            "reader",
            &["show", "research:chapter/one", "--provenance"],
            true
        )["data"]["record"]["fields"],
        json!({"count":0,"replicated":false})
    );
}

#[test]
fn shared_import_acceptance_recovers_original_token_and_assets() {
    for after_main in [false, true] {
        let mut s = Server::handbook();
        let repo = import_source(&s);
        let plan = s.import_plan(&repo, "agent", true, "book");
        let p = s.call(
            "author",
            &[
                "--role",
                "agent",
                "--op-id",
                "import",
                "import",
                "apply",
                plan.to_str().unwrap(),
            ],
            true,
        );
        let branch = p["data"]["branch"].as_str().unwrap();
        let token = s.token(branch);
        fs::remove_file(plan).unwrap();
        fs::remove_dir_all(repo).unwrap();
        let needle = if after_main {
            "CALL DOLT_MERGE('--ff-only'".into()
        } else {
            literal(&format!("accept {branch} by alice"))
        };
        s.interrupted(
            &[
                "--op-id",
                "accept",
                "review",
                "accept",
                branch,
                "--review-token",
                &token,
            ],
            needle,
        );
        if after_main {
            s.call(
                "other",
                &[
                    "--op-id", "later", "new", "page", "--title", "Later", "--body", "x",
                ],
                true,
            );
        }
        let recovered = s.call("second", &["recover", "accept", "--resume"], true);
        assert_eq!(recovered["data"]["review_token"], token);
        assert_eq!(
            s.call("reader", &["doctor"], true)["data"]["import_health"]["files"],
            4
        );
        let export = s.dir.path().join("export");
        s.call(
            "reader",
            &["render", "--out", export.to_str().unwrap()],
            true,
        );
        assert_eq!(
            fs::read(export.join("sources/book/asset.bin")).unwrap(),
            [0, 255, 1]
        );
    }
}
