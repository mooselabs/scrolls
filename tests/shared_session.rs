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
        s.cli(&["sql", "-q", "CREATE TABLE records (id INT PRIMARY KEY, payload TEXT); CREATE TABLE operations (id VARCHAR(128) PRIMARY KEY, payload TEXT); CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','fixture','--author','Test <test@localhost>'); CALL DOLT_BRANCH('proposal/test');"]);
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
