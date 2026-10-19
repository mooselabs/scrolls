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
                    && String::from_utf8_lossy(&bytes[5..]).starts_with("CALL DOLT_COMMIT(")
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
        vec!["review", "anything"],
        vec!["proposals"],
        vec!["beads", "list"],
        vec!["consumed"],
        vec!["import", "profiles", "list"],
        vec!["schema", "apply", "nonexistent", "--expect", "wrong"],
        vec!["--proposal", "proposal/other", "list"],
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
    assert!(
        s.call(
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
                "x"
            ],
            false
        )
        .to_string()
        .contains("shared_unsupported")
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
    let (port, proxy) = lose_commit_ack(actual_port);
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
    let pin = Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("pinned 0.3.0 checkout required, see docs/shared-authoring.md");
    assert!(pin.status.success(), "pinned 0.3.0 checkout required");
    assert_eq!(
        String::from_utf8_lossy(&pin.stdout).trim(),
        "3ae0a38dcc4da0a508e82ce4737d226e06d6f2b4"
    );
    let clean = Command::new("git")
        .arg("-C")
        .arg(&source)
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
        .current_dir(&source)
        .env("CARGO_TARGET_DIR", target)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
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
