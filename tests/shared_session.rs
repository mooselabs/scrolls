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
        // Serialize the ephemeral-port handoff across fixture servers.
        static STARTING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _starting = STARTING.lock().unwrap();
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
                self.child.as_mut().unwrap().try_wait().unwrap().is_none(),
                "owned fixture server exited during startup"
            );
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
        if expected && !out.status.success() && args.starts_with(&["backup", "create"]) {
            if let Some(index) = args.iter().position(|arg| *arg == "--out") {
                let log = Path::new(args[index + 1]).join("verification/verification-error.txt");
                if let Ok(bytes) =
                    fs::read(Path::new(args[index + 1]).join("verification/file-change.json"))
                {
                    let proof: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    for (name, stamp) in proof["after"].as_object().unwrap() {
                        if proof["before"][name] != *stamp {
                            eprintln!(
                                "private fixture file change: {name}: {:?} → {stamp}",
                                proof["before"][name]
                            );
                        }
                    }
                }

                if let Ok(detail) = fs::read_to_string(
                    Path::new(args[index + 1]).join("verification/ref-error.json"),
                ) {
                    eprintln!("private fixture ref failure: {detail}");
                }
                if let Ok(detail) = fs::read_to_string(log) {
                    eprintln!("private fixture verification failure: {detail}");
                }
            }
        }
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

#[test]
fn shared_backup_create_and_restore_preserve_store_identity() {
    let s = Server::empty();
    s.init(Some("examples/handbook.yaml"));
    let note = s.call(
        "a",
        &[
            "--op-id",
            "backup-note",
            "new",
            "page",
            "--title",
            "Preserved",
            "--body",
            "private-backup-body-marker-7c2e",
        ],
        true,
    );
    let before = s.main_head();
    let backup = s.dir.path().join("backup");
    let made = s.call(
        "a",
        &["backup", "create", "--out", backup.to_str().unwrap()],
        true,
    );
    assert_eq!(s.main_head(), before);
    assert_eq!(made["data"]["verified"], true);
    assert_eq!(made["data"]["database_only"], true);
    assert_eq!(made["data"]["source_revision"], before);
    let restored = s.dir.path().join("restored");
    let result = Command::new(env!("CARGO_BIN_EXE_scrolls"))
        .arg("--json")
        .arg("--dolt")
        .arg(Server::binary())
        .args(["backup", "restore"])
        .arg(&backup)
        .arg("--out")
        .arg(&restored)
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(result.status.success(), "{value}");
    assert_eq!(value["data"]["store_id"], made["data"]["store_id"]);
    assert_eq!(value["data"]["restored"], true);
    assert_eq!(value["data"]["source_revision"], before);
    assert!(restored.join("database/.dolt").is_dir());
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(backup.join("verified.json")).unwrap()).unwrap();
    let copy: serde_json::Value =
        serde_json::from_slice(&fs::read(restored.join("verified.json")).unwrap()).unwrap();
    for directory in [
        &backup,
        &restored,
        &backup.join("verification"),
        &backup.join("recheck-verification"),
    ] {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() {
                let bytes = fs::read(entry.path()).unwrap();
                assert!(
                    !String::from_utf8_lossy(&bytes).contains("private-backup-body-marker-7c2e"),
                    "diagnostic leaked body: {:?}",
                    entry.path()
                );
            }
        }
    }
    assert_eq!(original["kind"], "database-backup");
    assert_eq!(copy["kind"], "database-restore");
    assert_eq!(copy["source_backup_digest"], original["digest"]);
    assert_eq!(copy["snapshot"], original["snapshot"]);
    assert_eq!(
        s.call("b", &["show", note["data"]["id"].as_str().unwrap()], true)["data"]["record"]["body"],
        "private-backup-body-marker-7c2e"
    );
}

#[test]
fn local_clean_predicate_accepts_native_boolean_but_refuses_dirty_state() {
    let root = tempfile::tempdir().unwrap();
    let local =
        scrolls::storage::Store::open(&root.path().join("store"), &Server::binary(), true).unwrap();
    SqlSession::clean(&local).unwrap();
    local
        .sql("CREATE TABLE unknown_state (id INT PRIMARY KEY)")
        .unwrap();
    assert!(
        SqlSession::clean(&local)
            .unwrap_err()
            .to_string()
            .contains("dirty_state")
    );
}

/// Real protocol transport with one controlled request-side fault callback.
fn backup_query_proxy<F>(target: u16, mut callback: F) -> (u16, std::thread::JoinHandle<()>)
where
    F: FnMut(&str) -> Option<String> + Send + 'static,
{
    use std::{
        io::{Read, Write},
        net::{Shutdown, TcpStream},
    };
    fn packet(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
        let mut header = [0u8; 4];
        stream.read_exact(&mut header)?;
        let len =
            usize::from(header[0]) | usize::from(header[1]) << 8 | usize::from(header[2]) << 16;
        let mut bytes = vec![0; len + 4];
        bytes[..4].copy_from_slice(&header);
        stream.read_exact(&mut bytes[4..])?;
        Ok(bytes)
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut client, _) = listener.accept().unwrap();
        let mut server = TcpStream::connect(("127.0.0.1", target)).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(120)))
            .unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(120)))
            .unwrap();
        let mut input = client.try_clone().unwrap();
        let mut output = server.try_clone().unwrap();
        let upstream = std::thread::spawn(move || {
            while let Ok(mut bytes) = packet(&mut input) {
                if bytes.get(4) == Some(&3) {
                    if let Some(sql) = callback(&String::from_utf8_lossy(&bytes[5..])) {
                        let len = sql.len() + 1;
                        assert!(len < 0xff_ffff);
                        let sequence = bytes[3];
                        bytes = vec![
                            (len & 255) as u8,
                            ((len >> 8) & 255) as u8,
                            ((len >> 16) & 255) as u8,
                            sequence,
                            3,
                        ];
                        bytes.extend_from_slice(sql.as_bytes());
                    }
                }
                if output.write_all(&bytes).is_err() {
                    break;
                }
            }
            let _ = output.shutdown(Shutdown::Both);
        });
        while let Ok(bytes) = packet(&mut server) {
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
#[cfg(target_os = "linux")]
fn shared_backup_attestation_and_parent_replacement_fail_without_redirected_writes() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    for mode in [
        "missing",
        "wrong",
        "other-filesystem",
        "parent-before",
        "parent-during",
        "parent-directory-before",
        "parent-directory-during",
    ] {
        let mut s = Server::empty();
        s.init(None);
        let before = s.main_head();
        let actual = s.port;
        let out = s.dir.path().join("backup");
        let moved = s.dir.path().join("moved");
        let foreign = s.dir.path().join("foreign");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("marker"), "must survive").unwrap();
        let observed = Arc::new(AtomicUsize::new(0));
        let count = observed.clone();
        let fired = Arc::new(AtomicBool::new(false));
        let seen = fired.clone();
        let (output, renamed, replacement) = (out.clone(), moved.clone(), foreign.clone());
        let absent = s.dir.path().join("not-shared/host-nonce");
        let (port, proxy) = backup_query_proxy(actual, move |sql| {
            let backup = sql.starts_with("CALL DOLT_BACKUP(");
            if backup {
                count.fetch_add(1, Ordering::SeqCst);
            }
            let matches = if mode.ends_with("during") {
                backup
            } else {
                sql.contains("HEX(LOAD_FILE(")
            };
            if matches && !seen.swap(true, Ordering::SeqCst) {
                match mode {
                    "missing" => fs::remove_file(output.join("host-nonce")).unwrap(),
                    "wrong" => fs::write(output.join("host-nonce"), "wrong nonce").unwrap(),
                    "other-filesystem" => {
                        return Some(format!(
                            "SELECT HEX(LOAD_FILE({})) AS proof",
                            literal(absent.to_str().unwrap())
                        ));
                    }
                    _ => {
                        fs::rename(&output, &renamed).unwrap();
                        if mode.contains("directory") {
                            fs::create_dir(&output).unwrap();
                        } else {
                            std::os::unix::fs::symlink(&replacement, &output).unwrap();
                        }
                    }
                }
            }
            None
        });
        s.port = port;
        let result = s.call(
            "client",
            &["backup", "create", "--out", out.to_str().unwrap()],
            false,
        );
        proxy.join().unwrap();
        s.port = actual;
        assert!(
            fired.load(Ordering::SeqCst),
            "fault was never reached: {result}"
        );
        assert_eq!(
            observed.load(Ordering::SeqCst),
            usize::from(mode.ends_with("during"))
        );
        assert_eq!(
            result["error"]["diagnostic"]["code"],
            if mode.starts_with("parent") {
                "backup_destination_changed"
            } else {
                "backup_host_mismatch"
            }
        );
        assert_eq!(s.main_head(), before);
        assert!(!out.join("verified.json").exists());
        assert!(!moved.join("verified.json").exists());
        assert_eq!(
            fs::read_to_string(foreign.join("marker")).unwrap(),
            "must survive"
        );
        assert_eq!(fs::read_dir(&foreign).unwrap().count(), 1);
        if mode.ends_with("during") {
            assert!(
                moved.join("snapshot/manifest").is_file(),
                "server must write only to the held directory"
            );
        }
        if mode == "other-filesystem" {
            assert!(
                out.join("host-nonce").exists(),
                "local file exists; the loopback server sees a different filesystem"
            );
        }
    }
}

fn restore_backup_artifact(backup: &Path, output: &Path, success: bool) -> serde_json::Value {
    let result = Command::new(env!("CARGO_BIN_EXE_scrolls"))
        .arg("--json")
        .arg("--dolt")
        .arg(Server::binary())
        .args(["backup", "restore"])
        .arg(backup)
        .arg("--out")
        .arg(output)
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(result.status.success(), success, "{value}");
    value
}
fn server_from_restored(output: &Path) -> Server {
    let dir = tempfile::tempdir().unwrap();
    fs::rename(output.join("database"), dir.path().join("knowledge")).unwrap();
    let mut server = Server {
        dir,
        child: None,
        port: 0,
    };
    server.start();
    let mut admin = server.raw("main");
    let absent: u64 = admin
        .query_first("SELECT COUNT(*) FROM mysql.user WHERE User='alice'")
        .unwrap()
        .unwrap();
    assert_eq!(
        absent, 0,
        "database-only backup must not claim to restore the original SQL account"
    );
    // Explicit, disposable provisioning is separate from the restore operation.
    admin
        .query_drop("CREATE USER alice IDENTIFIED BY 'test-only'; GRANT ALL ON *.* TO alice")
        .unwrap();
    drop(admin);
    server
}

#[test]
fn shared_backup_roundtrip_refs_assets_receipts_and_clean_interrupted_intent() {
    let mut s = Server::handbook();
    let repo = import_source(&s);
    let expected: Vec<_> = ["README.md", "chapters/one.md", "asset.bin", "run.sh"]
        .into_iter()
        .map(|name| (name, fs::read(repo.join(name)).unwrap()))
        .collect();
    let plan = s.import_plan(&repo, "human", true, "book");
    let imported = s.call(
        "a",
        &[
            "--op-id",
            "capture",
            "import",
            "apply",
            plan.to_str().unwrap(),
        ],
        true,
    );
    let pending = s.proposed_page("review-later", "Pending review");
    let body = s.dir.path().join("origin-body.md");
    fs::write(&body, "recover exact body").unwrap();
    s.interrupted(
        &[
            "--role",
            "agent",
            "--op-id",
            "recover-after-restore",
            "new",
            "page",
            "--title",
            "Recover",
            "--body-file",
            body.to_str().unwrap(),
        ],
        "CALL DOLT_BRANCH(".into(),
    );
    let mut raw = s.raw("main");
    raw.query_drop("CALL DOLT_TAG('-m','first annotation','--author','Fixture <fixture@localhost>','snapshot-a','main'); CALL DOLT_TAG('-m','second annotation','--author','Fixture <fixture@localhost>','snapshot-b','main')").unwrap();
    let remote = s.dir.path().join("remote");
    raw.query_drop(format!(
        "CALL DOLT_REMOTE('add','fixture',{}); CALL DOLT_PUSH('fixture','main')",
        literal(&format!("file://{}", remote.display()))
    ))
    .unwrap();
    let remote_count: u64 = raw
        .query_first("SELECT COUNT(*) FROM dolt_remote_branches")
        .unwrap()
        .unwrap();
    assert!(remote_count > 0);
    let original_tags: Vec<(String, String, String)> = raw
        .query("SELECT tag_name,tag_hash,message FROM dolt_tags ORDER BY tag_name")
        .unwrap();
    drop(raw);
    let before = s.main_head();
    let backup = s.dir.path().join("backup");
    s.call(
        "a",
        &["backup", "create", "--out", backup.to_str().unwrap()],
        true,
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(backup.join("verified.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["snapshot"]["tags"]["snapshot-a"]["commit"],
        manifest["snapshot"]["tags"]["snapshot-b"]["commit"]
    );
    assert_ne!(
        manifest["snapshot"]["tags"]["snapshot-a"]["metadata_digest"],
        manifest["snapshot"]["tags"]["snapshot-b"]["metadata_digest"]
    );
    assert!(
        !manifest["snapshot"]["remote_refs"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    let output = s.dir.path().join("restored");
    restore_backup_artifact(&backup, &output, true);
    s.stop();
    let restored = server_from_restored(&output);
    fs::remove_dir_all(s.dir.path()).unwrap();
    assert_eq!(restored.main_head(), before);
    let tags: Vec<(String, String, String)> = restored
        .raw("main")
        .query("SELECT tag_name,tag_hash,message FROM dolt_tags ORDER BY tag_name")
        .unwrap();
    assert_eq!(tags, original_tags);
    let after_remote_count: u64 = restored
        .raw("main")
        .query_first("SELECT COUNT(*) FROM dolt_remote_branches")
        .unwrap()
        .unwrap();
    assert_eq!(after_remote_count, remote_count);
    let export = restored.dir.path().join("export");
    restored.call(
        "reader",
        &["render", "--out", export.to_str().unwrap()],
        true,
    );
    for (name, bytes) in expected {
        assert_eq!(
            fs::read(export.join("sources/book").join(name)).unwrap(),
            bytes
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
    let receipt: String = restored
        .raw("main")
        .query_first("SELECT payload FROM operations WHERE id='capture'")
        .unwrap()
        .unwrap();
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    assert_eq!(receipt["result"], imported["data"]);
    let recovered = restored.call(
        "new-client",
        &[
            "--role",
            "agent",
            "recover",
            "recover-after-restore",
            "--resume",
        ],
        true,
    );
    let branch = recovered["data"]["branch"].as_str().unwrap();
    let token = restored.token(branch);
    restored.call(
        "reviewer",
        &[
            "--op-id",
            "accept-restored",
            "review",
            "accept",
            branch,
            "--review-token",
            &token,
        ],
        true,
    );
    let original_branch = pending["branch"].as_str().unwrap();
    assert!(
        restored.call("reviewer", &["review", original_branch], true)["data"]["review_token"]
            .is_string()
    );
    assert_ne!(restored.main_head(), before);
}

#[test]
fn shared_backup_research_configuration_retains_zero_and_false() {
    let s = Server::empty();
    s.init(Some("examples/research.yaml"));
    let trial = s.call(
        "a",
        &[
            "--op-id",
            "trial",
            "new",
            "trial",
            "--title",
            "Zero result",
            "--body",
            "observed",
            "--field",
            "count=0",
            "--field",
            "replicated=false",
        ],
        true,
    );
    let backup = s.dir.path().join("backup");
    s.call(
        "a",
        &["backup", "create", "--out", backup.to_str().unwrap()],
        true,
    );
    let output = s.dir.path().join("restored");
    restore_backup_artifact(&backup, &output, true);
    let restored = server_from_restored(&output);
    let row = restored.call(
        "reader",
        &["show", trial["data"]["id"].as_str().unwrap()],
        true,
    );
    assert_eq!(row["data"]["record"]["fields"]["count"], 0);
    assert_eq!(row["data"]["record"]["fields"]["replicated"], false);
}

#[test]
#[cfg(target_os = "linux")]
fn shared_backup_refuses_corruption_incomplete_artifacts_and_collisions() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let s = Server::empty();
    s.init(None);
    let backup = s.dir.path().join("backup");
    s.call(
        "a",
        &["backup", "create", "--out", backup.to_str().unwrap()],
        true,
    );
    let marker = fs::read(backup.join("verified.json")).unwrap();
    let inventory: serde_json::Value = serde_json::from_slice(&marker).unwrap();
    let file = backup.join("snapshot").join(
        inventory["files"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .unwrap(),
    );
    let original = fs::read(&file).unwrap();
    let permissions = fs::metadata(&file).unwrap().permissions();
    let out = s.dir.path().join("must-not-exist");
    for fault in [
        "bytes",
        "mode",
        "missing",
        "symlink",
        "special",
        "extra",
        "manifest",
        "incomplete",
    ] {
        match fault {
            "bytes" => fs::write(&file, b"corrupt").unwrap(),
            "mode" => fs::set_permissions(
                &file,
                fs::Permissions::from_mode(permissions.mode() ^ 0o100),
            )
            .unwrap(),
            "missing" => fs::remove_file(&file).unwrap(),
            "symlink" => {
                fs::remove_file(&file).unwrap();
                symlink(backup.join("verified.json"), &file).unwrap();
            }
            "special" => {
                fs::remove_file(&file).unwrap();
                assert!(
                    Command::new("mkfifo")
                        .arg(&file)
                        .status()
                        .unwrap()
                        .success()
                );
            }
            "extra" => fs::write(backup.join("snapshot/unknown"), "extra").unwrap(),
            "manifest" => fs::write(backup.join("verified.json"), b"{}").unwrap(),
            _ => fs::remove_file(backup.join("verified.json")).unwrap(),
        }
        let error = restore_backup_artifact(&backup, &out, false);
        assert!(!out.exists(), "{fault}: {error}");
        if fs::symlink_metadata(&file).is_ok() {
            fs::remove_file(&file).unwrap();
        }
        fs::write(&file, &original).unwrap();
        fs::set_permissions(&file, permissions.clone()).unwrap();
        if backup.join("snapshot/unknown").exists() {
            fs::remove_file(backup.join("snapshot/unknown")).unwrap();
        }
        fs::write(backup.join("verified.json"), &marker).unwrap();
    }
    fs::create_dir(&out).unwrap();
    fs::write(out.join("keep"), "untouched").unwrap();
    restore_backup_artifact(&backup, &out, false);
    s.call(
        "a",
        &["backup", "create", "--out", out.to_str().unwrap()],
        false,
    );
    assert_eq!(fs::read_to_string(out.join("keep")).unwrap(), "untouched");
    assert_eq!(fs::read_dir(&out).unwrap().count(), 1);
    let moved = s.dir.path().join("relocated-backup");
    fs::rename(&backup, &moved).unwrap();
    restore_backup_artifact(&moved, &s.dir.path().join("valid-restored"), true);
}

#[test]
fn shared_backup_rejects_dirty_source_and_holds_publication_lock() {
    let s = Server::empty();
    s.init(None);
    let out = s.dir.path().join("backup");
    let held = s.shared("main");
    let result = s.call(
        "a",
        &["backup", "create", "--out", out.to_str().unwrap()],
        false,
    );
    assert_eq!(result["error"]["diagnostic"]["code"], "backup_lock_timeout");
    assert!(!out.exists());
    drop(held);
    let mut raw = s.raw("main");
    raw.query_drop("CREATE TABLE foreign_state (id INT PRIMARY KEY)")
        .unwrap();
    let result = s.call(
        "a",
        &["backup", "create", "--out", out.to_str().unwrap()],
        false,
    );
    assert_eq!(result["error"]["diagnostic"]["code"], "backup_source_dirty");
    assert!(!out.exists());
    let tables: Vec<String> = raw.query("SHOW TABLES").unwrap();
    assert!(tables.contains(&"foreign_state".into()));
}

#[test]
fn shared_backup_lost_ack_leaves_unverified_copy_and_releases_lock() {
    let mut s = Server::empty();
    s.init(None);
    let before = s.main_head();
    let actual = s.port;
    let (port, proxy) = lose_matching_ack(actual, "CALL DOLT_BACKUP(".into());
    s.port = port;
    let out = s.dir.path().join("interrupted");
    s.call(
        "a",
        &["backup", "create", "--out", out.to_str().unwrap()],
        false,
    );
    proxy.join().unwrap();
    s.port = actual;
    assert!(out.join("snapshot/manifest").exists());
    assert!(!out.join("verified.json").exists());
    assert_eq!(s.main_head(), before);
    s.call(
        "b",
        &["backup", "create", "--out", out.to_str().unwrap()],
        false,
    );
    s.call(
        "b",
        &[
            "backup",
            "create",
            "--out",
            s.dir.path().join("retry").to_str().unwrap(),
        ],
        true,
    );
}

#[test]
fn shared_backup_detects_source_change_after_copy() {
    let mut s = Server::empty();
    s.init(None);
    let actual = s.port;
    let opts = s.opts("main");
    let (port, proxy) = backup_query_proxy(actual, move |sql| {
        if sql.starts_with("CALL DOLT_BACKUP(") {
            let mut raw = Conn::new(opts.clone()).unwrap();
            raw.query_drop("CALL DOLT_TAG('-m','concurrent change','--author','Fixture <fixture@localhost>','new-tag','main')").unwrap();
        }
        None
    });
    s.port = port;
    let out = s.dir.path().join("changed");
    let error = s.call(
        "a",
        &["backup", "create", "--out", out.to_str().unwrap()],
        false,
    );
    proxy.join().unwrap();
    s.port = actual;
    assert_eq!(
        error["error"]["diagnostic"]["code"],
        "backup_restore_mismatch"
    );
    assert!(!out.join("verified.json").exists());
    let tags: Vec<String> = s
        .raw("main")
        .query("SELECT tag_name FROM dolt_tags")
        .unwrap();
    assert_eq!(tags, vec!["new-tag"]);
}

#[test]
fn shared_backup_refuses_artifact_changed_after_restore_verification() {
    let mut s = Server::empty();
    s.init(None);
    let actual = s.port;
    let out = s.dir.path().join("backup");
    let artifact = out.join("snapshot/manifest");
    let mut copied = false;
    let (port, proxy) = backup_query_proxy(actual, move |sql| {
        if sql.starts_with("CALL DOLT_BACKUP(") {
            copied = true;
        } else if copied && sql == "SELECT * FROM dolt_stashes" {
            // Source is rechecked only after the actual native restore completed.
            fs::write(&artifact, "corrupt after restore").unwrap();
            copied = false;
        }
        None
    });
    s.port = port;
    let error = s.call(
        "a",
        &["backup", "create", "--out", out.to_str().unwrap()],
        false,
    );
    proxy.join().unwrap();
    s.port = actual;
    assert_eq!(error["error"]["diagnostic"]["code"], "backup_changed");
    assert!(!out.join("verified.json").exists());
}

#[test]
fn shared_backup_refuses_unqualified_stashes_and_native_tool_versions() {
    use std::os::unix::fs::PermissionsExt;
    let mut s = Server::empty();
    s.init(None);
    let tool = s.dir.path().join("wrong-dolt");
    fs::write(&tool, "#!/bin/sh\nprintf 'dolt version 2.3.0\\n'\n").unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
    let out = s.dir.path().join("wrong-version");
    let error = s.call(
        "a",
        &[
            "--dolt",
            tool.to_str().unwrap(),
            "backup",
            "create",
            "--out",
            out.to_str().unwrap(),
        ],
        false,
    );
    assert_eq!(error["error"]["diagnostic"]["code"], "backup_tool_version");
    assert!(!out.join("verified.json").exists());
    s.stop();
    s.cli(&["sql", "-q", "CREATE TABLE saved_work (id INT PRIMARY KEY)"]);
    s.cli(&["stash", "push", "--include-untracked"]);
    s.start();
    let out = s.dir.path().join("has-stash");
    let error = s.call(
        "a",
        &["backup", "create", "--out", out.to_str().unwrap()],
        false,
    );
    assert_eq!(
        error["error"]["diagnostic"]["code"],
        "backup_unsupported_refs"
    );
    assert!(!out.exists());
    let count: u64 = s
        .raw("main")
        .query_first("SELECT COUNT(*) FROM dolt_stashes")
        .unwrap()
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn shared_backup_walks_multilevel_native_reference_maps() {
    let s = Server::empty();
    s.init(None);
    let mut raw = s.raw("main");
    for index in 0..80 {
        raw.query_drop(format!("CALL DOLT_BRANCH('archive-{index:03}','main')"))
            .unwrap();
    }
    drop(raw);
    let backup = s.dir.path().join("many-refs");
    s.call(
        "a",
        &["backup", "create", "--out", backup.to_str().unwrap()],
        true,
    );
    let proof: serde_json::Value =
        serde_json::from_slice(&fs::read(backup.join("verified.json")).unwrap()).unwrap();
    let refs = proof["dataset_refs"].as_object().unwrap();
    assert_eq!(proof["snapshot"]["branches"].as_object().unwrap().len(), 82);
    for index in 0..80 {
        assert!(refs.contains_key(&format!("refs/heads/archive-{index:03}")));
    }
    assert!(refs.contains_key("refs/internal/create"));
    assert!(
        proof["dataset_map_nodes"].as_u64().unwrap() > 1,
        "fixture must traverse a multilevel map"
    );
    restore_backup_artifact(&backup, &s.dir.path().join("restored-many-refs"), true);
}

#[test]
fn shared_backup_refuses_unqualified_native_ref_categories() {
    use std::os::unix::fs::PermissionsExt;
    let s = Server::empty();
    s.init(None);
    let before = s.main_head();
    // Inject a native inspection result at the tool boundary. These categories
    // are not exposed by the SQL branches/tags/remotes tables.
    for category in ["workspaces", "internal", "statistics", "tuples"] {
        let wrapper = s.dir.path().join(format!("inspect-{category}"));
        let program = format!(
            r#"#!/usr/bin/env python3
import subprocess,sys
if sys.argv[1] != 'show': sys.exit(subprocess.call([{binary}]+sys.argv[1:]))
output=subprocess.check_output([{binary}]+sys.argv[1:])
if sys.argv[1]=='show' and output.startswith(b'SerialMessage StoreRoot{{AddressMap {{'):
 output=output.replace(b'\n', b'\n\t\trefs/{category}/unexpected: #{before}\n',1)
sys.stdout.buffer.write(output)
"#,
            binary = serde_json::to_string(Server::binary().to_str().unwrap()).unwrap()
        );
        fs::write(&wrapper, program).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
        let out = s.dir.path().join(format!("ref-{category}"));
        let error = s.call(
            "a",
            &[
                "--dolt",
                wrapper.to_str().unwrap(),
                "backup",
                "create",
                "--out",
                out.to_str().unwrap(),
            ],
            false,
        );
        assert_eq!(
            error["error"]["diagnostic"]["code"], "backup_unsupported_refs",
            "{category}: {error}"
        );
        assert!(!out.join("verified.json").exists());
        assert_eq!(s.main_head(), before);
    }
}

#[test]
fn shared_backup_refuses_native_interactive_rebase_even_after_clean_commit() {
    let s = Server::empty();
    s.init(None);
    s.call(
        "a",
        &[
            "--op-id",
            "rebase-note",
            "new",
            "note",
            "--title",
            "Rebase input",
            "--body",
            "body",
        ],
        true,
    );
    let mut raw = s.raw("main");
    raw.query_drop("CALL DOLT_REBASE('-i','HEAD~1')").unwrap();
    let active: String = raw.query_first("SELECT active_branch()").unwrap().unwrap();
    assert!(active.starts_with("dolt_rebase_"));
    raw.query_drop("CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','preserved rebase state','--author','Fixture <fixture@localhost>')").unwrap();
    let dirty: u64 = raw
        .query_first("SELECT COUNT(*) FROM dolt_status")
        .unwrap()
        .unwrap();
    assert_eq!(dirty, 0);
    let out = s.dir.path().join("rebase-refused");
    s.call(
        "a",
        &["backup", "create", "--out", out.to_str().unwrap()],
        false,
    );
    assert!(!out.join("verified.json").exists());
    let branches: Vec<String> = raw.query("SELECT name FROM dolt_branches").unwrap();
    assert!(branches.contains(&active));
}

#[test]
fn shared_backup_restore_refuses_chunks_changed_after_inspection() {
    use std::os::unix::fs::PermissionsExt;
    let s = Server::empty();
    s.init(None);
    let backup = s.dir.path().join("backup");
    s.call(
        "a",
        &["backup", "create", "--out", backup.to_str().unwrap()],
        true,
    );
    let wrapper = s.dir.path().join("change-after-inspection");
    let program = format!(
        r#"#!/usr/bin/env python3
import subprocess,sys,pathlib
result=subprocess.run([{binary}]+sys.argv[1:])
if result.returncode==0 and sys.argv[1]=='sql':
 directory=pathlib.Path.cwd()
 chunk=next(p for p in (directory/'.dolt/noms').iterdir() if p.is_file() and len(p.name)==32)
 with chunk.open('ab') as stream: stream.write(b'changed after verification')
 (directory.parent/'mutation-proof').write_text('injected')
sys.exit(result.returncode)
"#,
        binary = serde_json::to_string(Server::binary().to_str().unwrap()).unwrap()
    );
    fs::write(&wrapper, program).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let out = s.dir.path().join("changed-restored");
    let result = Command::new(env!("CARGO_BIN_EXE_scrolls"))
        .arg("--json")
        .arg("--dolt")
        .arg(&wrapper)
        .args(["backup", "restore"])
        .arg(&backup)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    let error: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(!result.status.success(), "{error}");
    assert_eq!(error["error"]["diagnostic"]["code"], "backup_changed");
    assert!(out.join("mutation-proof").exists());
    assert!(!out.join("verified.json").exists());
}

#[test]
fn shared_connection_io_budget_is_explicit_bounded_and_redacted() {
    let s = Server::empty();
    s.init(None);
    for (value, valid) in [
        (json!(11), true),
        (json!(600), true),
        (json!(0), false),
        (json!(10), false),
        (json!(601), false),
        (json!("private-budget-marker"), false),
    ] {
        let mut command = s.client("budget", &["doctor"]);
        let path = s.dir.path().join("budget/connection.json");
        let mut config: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        config["io_timeout_seconds"] = value;
        fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        let output = command.output().unwrap();
        let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.success(), valid, "{response}");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("private-budget-marker"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-budget-marker"));
    }
}
