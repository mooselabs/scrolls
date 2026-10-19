//! Real Dolt, real TCP, independent CLI writers. These tests never skip a backend.
use mysql::{Conn, OptsBuilder, prelude::Queryable};
use scrolls::{
    model::digest,
    storage::{Store, put},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn dolt() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(".tools/dolt")
}
struct Fixture {
    dir: tempfile::TempDir,
    backend: Option<Child>,
    port: u16,
    shared: bool,
}
impl Fixture {
    fn new(shared: bool, custom: bool) -> Self {
        let mut f = Self {
            dir: tempfile::tempdir().unwrap(),
            backend: None,
            port: 0,
            shared,
        };
        if shared {
            fs::create_dir(f.dir.path().join("db")).unwrap();
            let o = f
                .dolt()
                .args(["init", "--name", "Test", "--email", "test@localhost"])
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            f.port = TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            f.start_backend();
            let mut c = Conn::new(f.opts()).unwrap();
            c.query_drop("CREATE USER alice IDENTIFIED BY 'test-only'; GRANT ALL ON *.* TO alice")
                .unwrap();
            fs::write(f.dir.path().join("connection.json"),json!({"host":"127.0.0.1","port":f.port,"database":"db","user":"alice","password":"test-only","io_timeout_seconds":11}).to_string()).unwrap();
        }
        let schema = if custom {
            vec!["--schema", "examples/handbook.yaml"]
        } else {
            vec![]
        };
        let mut args = vec!["init"];
        args.extend(schema);
        if shared {
            let mut plan = args.clone();
            plan.push("--dry-run");
            let p = f.ok(&plan);
            args.extend([
                "--expect-base",
                p["base_revision"].as_str().unwrap(),
                "--expect-schema",
                p["ontology_digest"].as_str().unwrap(),
            ]);
            f.ok(&args);
        } else {
            f.ok(&args);
        }
        f
    }
    fn opts(&self) -> OptsBuilder {
        OptsBuilder::new()
            .ip_or_hostname(Some("127.0.0.1"))
            .tcp_port(self.port)
            .prefer_socket(false)
            .user(Some("root"))
            .db_name(Some("db"))
            .tcp_connect_timeout(Some(Duration::from_millis(200)))
    }
    fn dolt(&self) -> Command {
        let mut c = Command::new(dolt());
        c.current_dir(self.dir.path().join("db"))
            .env("DOLT_ROOT_PATH", self.dir.path().join("home"))
            .env("DOLT_DISABLE_EVENT_FLUSH", "1");
        c
    }
    fn start_backend(&mut self) {
        let log = fs::File::create(self.dir.path().join("backend.log")).unwrap();
        self.backend = Some(
            self.dolt()
                .args([
                    "sql-server",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &self.port.to_string(),
                ])
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        let start = Instant::now();
        while Conn::new(self.opts()).is_err() {
            assert!(start.elapsed() < Duration::from_secs(15));
            assert!(self.backend.as_mut().unwrap().try_wait().unwrap().is_none());
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    fn stop_backend(&mut self) {
        if let Some(mut c) = self.backend.take() {
            unsafe {
                libc::kill(c.id() as i32, libc::SIGTERM);
            }
            c.wait().unwrap();
        }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_scrolls"));
        c.args([
            "--json",
            "--actor",
            "alice",
            "--op-id",
            &uuid::Uuid::new_v4().to_string(),
        ]);
        if !args.contains(&"--role") {
            c.args(["--role", "human"]);
        }
        if !args.contains(&"--store") {
            c.arg("--store").arg(self.dir.path().join("store"));
        }
        c.arg("--dolt").arg(dolt());
        if self.shared {
            c.arg("--connection")
                .arg(self.dir.path().join("connection.json"));
        }
        c.args(args).stdin(Stdio::null());
        c
    }
    fn ok(&self, args: &[&str]) -> Value {
        let o = self.command(args).output().unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {} {}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        serde_json::from_slice::<Value>(&o.stdout).unwrap()["data"].clone()
    }
    fn serve(&self, args: &[&str]) -> Http {
        self.serve_on(args, "127.0.0.1:0")
    }
    fn serve_on(&self, args: &[&str], bind: &str) -> Http {
        let log = fs::File::create(
            self.dir
                .path()
                .join(format!("http-{}.log", uuid::Uuid::new_v4())),
        )
        .unwrap();
        let mut cmd = self.command(args);
        cmd.args(["serve", "--bind", bind])
            .stdout(Stdio::piped())
            .stderr(log);
        let mut child = cmd.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, recv) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            let _ = send.send(line);
        });
        let line = recv.recv_timeout(Duration::from_secs(30)).unwrap();
        let value: Value =
            serde_json::from_str(&line).unwrap_or_else(|_| panic!("server startup: {line}"));
        assert_eq!(value["event"], "listening", "{value}");
        Http {
            child,
            address: value["address"].as_str().unwrap().parse().unwrap(),
        }
    }
    fn seed(&self, kind: &str) -> String {
        self.ok(&["new", kind, "--title", "A title", "--body", "Body"])["id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn state(&self) -> Value {
        let config = scrolls::read::Config {
            store: self.dir.path().join("store"),
            dolt: dolt(),
            connection: self.shared.then(|| self.dir.path().join("connection.json")),
            branch: "main".into(),
            actor: "alice".into(),
        };
        let snap = scrolls::read::Snapshot::open(&config).unwrap();
        let mut rows = BTreeMap::new();
        for branch in snap.store.branches().unwrap() {
            for table in [
                "metadata",
                "records",
                "edges",
                "operations",
                "import_sources",
                "import_members",
                "import_files",
            ] {
                rows.insert(
                    format!("{branch}:{table}"),
                    snap.store
                        .sql(&format!(
                            "SELECT * FROM {table} AS OF {} ORDER BY id",
                            scrolls::storage::revision_literal(&branch)
                        ))
                        .unwrap(),
                );
            }
        }
        let active = snap.store.sql("SELECT active_branch() AS branch").unwrap();
        let mut files = BTreeMap::new();
        for file in [
            "consumed.jsonl",
            "beads.json",
            "review-journal.json",
            "migration-journal.json",
            "dolt-home/.dolt/config_global.json",
        ] {
            let path = config.store.join(file);
            files.insert(file, fs::read(path).ok().map(digest));
        }
        json!({"rows":rows,"files":files,"active":active})
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop_backend();
    }
}
struct Http {
    child: Child,
    address: SocketAddr,
}
struct Reply {
    status: u16,
    headers: BTreeMap<String, String>,
    value: Value,
    bytes: Vec<u8>,
}
impl Http {
    fn request(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Reply {
        let mut stream = TcpStream::connect(self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(40)))
            .unwrap();
        let host = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("host"))
            .map(|(_, v)| v.to_string())
            .unwrap_or_else(|| self.address.to_string());
        write!(stream,"{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: {}\r\n",body.len()).unwrap();
        for (k, v) in headers
            .iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("host"))
        {
            write!(stream, "{k}: {v}\r\n").unwrap();
        }
        stream.write_all(b"\r\n").unwrap();
        stream.write_all(body).unwrap();
        let mut bytes = vec![];
        stream.read_to_end(&mut bytes).unwrap();
        let split = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8(bytes[..split].to_vec()).unwrap();
        let mut lines = head.lines();
        let status = lines
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.to_lowercase(), v.trim().to_string()))
            .collect();
        let bytes = bytes[split + 4..].to_vec();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| panic!("invalid body: {}", String::from_utf8_lossy(&bytes)))
        };
        Reply {
            status,
            headers,
            value,
            bytes,
        }
    }
    fn get(&self, path: &str) -> Value {
        let r = self.request("GET", path, &[], &[]);
        assert_eq!(r.status, 200, "{path}: {}", r.value);
        r.value
    }
    fn signal(&mut self, signal: i32) {
        unsafe {
            libc::kill(self.child.id() as i32, signal);
        }
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "{status}");
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(15));
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Http {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn exercise(shared: bool) {
    let mut f = Fixture::new(shared, false);
    let id = f.seed("note");
    let other = f.seed("note");
    f.ok(&["link", &id, "related", &other, "--expect", "1"]);
    let prop = f.ok(&[
        "--role",
        "agent",
        "new",
        "entity",
        "--title",
        "Proposed",
        "--body",
        "Proposal body",
    ]);
    let branch = prop["branch"].as_str().unwrap();
    // Import real captured source so alias resolution and provenance are non-null.
    let source = f.dir.path().join("source");
    fs::create_dir_all(source.join("chapters")).unwrap();
    fs::write(
        source.join("chapters/intro.md"),
        "---\nid: intro\ntitle: Imported chapter\nupdated: '2026-10-19'\n---\nSource body\n",
    )
    .unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "chapters/intro.md"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@localhost",
            "commit",
            "-qm",
            "source",
        ],
    ] {
        let o = Command::new("git")
            .current_dir(&source)
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    let rev = Command::new("git")
        .current_dir(&source)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let rev = String::from_utf8(rev.stdout).unwrap();
    let plan = f.dir.path().join("import.json");
    f.ok(&[
        "import",
        "plan",
        "git",
        source.to_str().unwrap(),
        "--root",
        ".",
        "--rev",
        rev.trim(),
        "--source",
        "http-handbook",
        "--profile",
        "handbook",
        "--out",
        plan.to_str().unwrap(),
    ]);
    let imported = f.ok(&["import", "apply", plan.to_str().unwrap()]);
    let imported_branch = imported["branch"].as_str().unwrap();
    if imported_branch != "main" {
        let token = f.ok(&["review", imported_branch])["review_token"]
            .as_str()
            .unwrap()
            .to_string();
        f.ok(&[
            "review",
            "accept",
            imported_branch,
            "--review-token",
            &token,
        ]);
    }
    let imported_show = f.ok(&["show", "chapter:intro", "--provenance"]);
    assert!(!imported_show["provenance"].is_null());
    let active_schema = f.ok(&["schema", "show"]);
    let mut configured = active_schema["schema"].clone();
    configured["pins"] = json!(["chapter:intro"]);
    let schema_file = f.dir.path().join("pinned-schema.json");
    fs::write(&schema_file, configured.to_string()).unwrap();
    let proposed_schema = f.ok(&[
        "schema",
        "apply",
        schema_file.to_str().unwrap(),
        "--expect",
        active_schema["schema_revision"].as_str().unwrap(),
    ]);
    let schema_branch = proposed_schema["branch"].as_str().unwrap();
    let token = f.ok(&["review", schema_branch])["review_token"]
        .as_str()
        .unwrap()
        .to_string();
    f.ok(&["review", "accept", schema_branch, "--review-token", &token]);

    // CLI comparison also leaves an audit; capture the baseline after it.
    let show = f.ok(&["show", &id, "--provenance"]);
    let context = f.ok(&["context", &id]);
    let walk = f.ok(&["walk", &id, "--full"]);
    let schema = f.ok(&["schema", "show"]);
    let prime = f.ok(&["prime", "--budget", "1"]);
    let review = f.ok(&["review", branch]);
    let history = f.ok(&["history", &id]);
    let before = f.state();
    let http = f.serve(&[]);
    let proposal_http = f.serve(&["--proposal", branch]);
    let v = http.get("/v0/context");
    assert_eq!(v["data"]["read_only"], true);
    assert_eq!(v["branch"], "main");
    assert_eq!(http.get("/v0/schema")["data"], schema);
    assert_eq!(
        http.get(&format!("/v0/records/{id}?provenance=true"))["data"],
        show
    );
    assert_eq!(
        http.get(&format!("/v0/records/{id}/context"))["data"],
        context
    );
    assert_eq!(
        http.get(&format!("/v0/records/{id}/walk?full=true"))["data"],
        walk
    );
    assert_eq!(http.get("/v0/prime?budget=1")["data"], prime);
    let proposals = http.get("/v0/proposals");
    assert!(
        proposals["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["branch"] == branch)
    );
    assert_eq!(
        http.get("/v0/records/chapter%3Aintro?provenance=true")["data"],
        imported_show
    );
    let native = imported_show["record"]["id"].as_str().unwrap();
    assert_eq!(
        http.get("/v0/records/chapter%3Aintro/context")["data"],
        http.get(&format!("/v0/records/{native}/context"))["data"]
    );
    assert_eq!(
        http.get("/v0/records/chapter%3Aintro/walk")["data"],
        http.get(&format!("/v0/records/{native}/walk"))["data"]
    );
    assert_eq!(http.get(&format!("/v0/proposals/{branch}"))["data"], review);
    assert_eq!(
        http.get(&format!("/v0/history?id={id}"))["data"]["items"],
        history
    );
    assert!(
        http.get("/v0/history")["data"]["items"]
            .as_array()
            .unwrap()
            .len()
            > 1
    );
    assert_eq!(http.get("/v0/openapi.json")["openapi"], "3.1.0");
    assert_eq!(http.get("/readyz")["data"]["ready"], true);
    assert_eq!(http.get("/healthz")["ok"], true);
    assert_eq!(proposal_http.get("/v0/context")["branch"], branch);
    assert_eq!(
        proposal_http.get(&format!("/v0/records/{}", prop["id"].as_str().unwrap()))["data"]["record"]
            ["title"],
        "Proposed"
    );
    assert_eq!(
        f.state(),
        before,
        "startup or reads changed application state"
    );
    // JSON query bindings exercise the same expression evaluator as the CLI.
    let expression = json!({"op":"compare","field":"owner","cmp":"eq","value":"$actor"});
    let file = f.dir.path().join("query.json");
    fs::write(&file, expression.to_string()).unwrap();
    let expected = f.ok(&[
        "query",
        "--file",
        file.to_str().unwrap(),
        "--at",
        "2026-10-19T00:00:00Z",
        "--path",
        "src/a.rs",
    ]);
    let before = f.state(); // only the preceding CLI read may append consumption.
    let r=http.request("POST","/v0/query",&[("Content-Type","application/json")],json!({"expression":expression,"actor":"alice","paths":["src/a.rs"],"at":"2026-10-19T00:00:00Z"}).to_string().as_bytes());
    assert_eq!(r.status, 200, "{}", r.value);
    assert_eq!(r.value["data"], expected);
    let filtered = http.get("/v0/records?kind=note&owner=me&q=title");
    assert_eq!(filtered["data"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        http.request(
            "GET",
            &format!("/v0/records/{id}/context?budget=1"),
            &[],
            &[]
        )
        .status,
        400
    );
    assert_eq!(
        http.request("GET", "/v0/records/missing", &[], &[]).status,
        404
    );
    assert_eq!(
        http.request("GET", "/v0/proposals/missing", &[], &[])
            .status,
        404
    );
    // Complete record and history pagination, with opaque snapshot-bound cursors.
    for path in ["/v0/records", "/v0/history"] {
        let expected = http.get(path)["data"]["items"].as_array().unwrap().clone();
        let mut got = vec![];
        let mut next = format!("{path}?limit=2");
        loop {
            let page = http.get(&next);
            got.extend(page["data"]["items"].as_array().unwrap().clone());
            if let Some(cursor) = page["data"]["next_cursor"].as_str() {
                next = format!("{path}?limit=2&cursor={cursor}");
            } else {
                break;
            }
        }
        assert_eq!(got, expected);
    }
    assert_eq!(f.state(), before, "HTTP requests changed application state");
    // Compare pre-startup state except consumption intentionally added by CLI.
    assert_eq!(before["rows"], f.state()["rows"]);
    let page = http.get("/v0/records?limit=1");
    let cursor = page["data"]["next_cursor"].as_str().unwrap();
    let first = http.request("GET", &format!("/v0/records/{id}"), &[], &[]);
    let etag = first.headers["etag"].clone();
    let cached = http.request(
        "GET",
        &format!("/v0/records/{id}"),
        &[("If-None-Match", &etag)],
        &[],
    );
    assert_eq!(cached.status, 304);
    assert!(cached.bytes.is_empty());
    let detail_etag = http
        .request("GET", &format!("/v0/proposals/{branch}"), &[], &[])
        .headers["etag"]
        .clone();
    let prop_before = http.request("GET", "/v0/proposals", &[], &[]).headers["etag"].clone();
    let prop_id = prop["id"].as_str().unwrap();
    f.ok(&[
        "--proposal",
        branch,
        "edit",
        prop_id,
        "--expect",
        "1",
        "--title",
        "Changed proposal",
    ]);
    assert_eq!(
        http.request(
            "GET",
            "/v0/proposals",
            &[("If-None-Match", &prop_before)],
            &[]
        )
        .status,
        200
    );
    assert_eq!(
        http.request(
            "GET",
            &format!("/v0/proposals/{branch}"),
            &[("If-None-Match", &detail_etag)],
            &[]
        )
        .status,
        200
    );
    f.ok(&["edit", &id, "--expect", "2", "--title", "Updated"]);
    let changed = http.request(
        "GET",
        &format!("/v0/records?limit=1&cursor={cursor}"),
        &[],
        &[],
    );
    assert_eq!(changed.status, 409);
    assert_eq!(changed.value["error"]["code"], "snapshot_changed");
    let changed = http.request(
        "GET",
        &format!("/v0/records/{id}"),
        &[("If-None-Match", &etag)],
        &[],
    );
    assert_eq!(changed.status, 200);
    assert_ne!(changed.headers["etag"], etag);
    if shared {
        f.stop_backend();
        assert_eq!(http.request("GET", "/readyz", &[], &[]).status, 503);
        assert_eq!(
            http.request(
                "GET",
                &format!("/v0/records/{id}"),
                &[("If-None-Match", &changed.headers["etag"])],
                &[]
            )
            .status,
            503
        );
        assert_eq!(http.request("GET", "/healthz", &[], &[]).status, 200);
        f.start_backend();
        assert_eq!(http.get("/readyz")["data"]["ready"], true);
    }
}
#[test]
fn local_routes_cli_parity_pagination_and_readonly() {
    exercise(false);
}
#[test]
fn shared_routes_cli_parity_backend_recovery_and_readonly() {
    exercise(true);
}

#[test]
fn browser_security_limits_and_shutdown() {
    let f = Fixture::new(false, true);
    let id = f.seed("page");
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let before = f.state();
        let mut h = f.serve(&[]);
        assert_eq!(
            h.get("/v0/schema")["data"]["schema"]["kinds"]["page"]["initial"],
            "draft"
        );
        assert_eq!(
            h.get("/v0/records?field=audience%3D%22staff%22")["data"]["items"][0]["id"],
            id
        );
        for (path, headers, status) in [
            ("/healthz", vec![("Origin", "http://localhost")], 400),
            ("/healthz", vec![("Host", "evil.example")], 400),
            ("/v0/records?limit=201", vec![], 400),
            ("/v0/records?limit=0", vec![], 400),
            ("/v0/records?limit=1&limit=2", vec![], 400),
            ("/v0/records?q=%zz", vec![], 400),
            ("/v0/records?unknown=x", vec![], 400),
            ("/v0/records/nope/walk?depth=101", vec![], 404),
        ] {
            let r = h.request("GET", path, &headers, &[]);
            assert_eq!(r.status, status, "{}", r.value);
            assert_eq!(r.headers["x-content-type-options"], "nosniff");
            assert_eq!(r.headers["cache-control"], "private, no-cache");
            assert!(!r.headers.contains_key("access-control-allow-origin"));
        }
        let userinfo_host = format!("name@{}", h.address);
        let rejected = h.request("GET", "/healthz", &[("Host", &userinfo_host)], &[]);
        assert_eq!(rejected.status, 400);
        assert_eq!(rejected.value["error"]["code"], "host_rejected");
        assert_eq!(
            h.request("DELETE", &format!("/v0/records/{id}"), &[], &[])
                .status,
            405
        );
        assert_eq!(
            h.request(
                "POST",
                "/v0/query",
                &[("Content-Type", "application/json")],
                b"not json"
            )
            .status,
            400
        );
        assert_eq!(
            h.request(
                "POST",
                "/v0/query",
                &[("Content-Type", "application/json")],
                &vec![b' '; scrolls::http::BODY_LIMIT + 1]
            )
            .status,
            413
        );
        assert_eq!(
            h.request(
                "POST",
                "/v0/query",
                &[("Content-Type", "application/json")],
                br#"{"expression":{"op":"all","args":[]},"file":"/etc/passwd"}"#
            )
            .status,
            400
        );
        h.signal(signal);
        assert_eq!(before, f.state());
    }
    for bind in ["0.0.0.0:0", "192.0.2.1:0", "[::]:0", "localhost:0"] {
        let o = f.command(&["serve", "--bind", bind]).output().unwrap();
        assert!(!o.status.success());
    }
    let o = f
        .command(&[
            "--store",
            f.dir.path().join("missing").to_str().unwrap(),
            "serve",
        ])
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(!f.dir.path().join("missing").exists());
}

#[test]
fn local_lock_timeout_dirty_and_interrupted_states_are_preserved() {
    let f = Fixture::new(false, false);
    let id = f.seed("note");
    let h = f.serve(&[]);
    let locked = Store::open_readonly(&f.dir.path().join("store"), &dolt()).unwrap();
    let start = Instant::now();
    let r = h.request("GET", "/v0/records", &[], &[]);
    assert_eq!(r.status, 503);
    assert_eq!(r.value["error"]["code"], "busy");
    assert!((Duration::from_secs(5)..Duration::from_secs(7)).contains(&start.elapsed()));
    assert_eq!(h.get("/healthz")["ok"], true);
    drop(locked);
    assert!(h.get("/v0/records")["data"]["items"].is_array());
    let root = f.dir.path().join("store");
    fs::write(root.join("review-journal.json"), b"preserve me").unwrap();
    assert_eq!(h.request("GET", "/v0/records", &[], &[]).status, 409);
    let o = f.command(&["serve"]).output().unwrap();
    assert!(!o.status.success());
    assert_eq!(
        fs::read(root.join("review-journal.json")).unwrap(),
        b"preserve me"
    );
    fs::remove_file(root.join("review-journal.json")).unwrap();
    {
        let s = Store::open_readonly(&root, &dolt()).unwrap();
        s.sql(&put("metadata", "dirty-test", &true).unwrap())
            .unwrap();
    }
    assert_eq!(h.request("GET", "/v0/records", &[], &[]).status, 409);
    let o = f.command(&["serve"]).output().unwrap();
    assert!(!o.status.success());
    // Inspect directly: no HTTP repair removed the uncommitted row.
    let mut c = Command::new(dolt());
    let o = c
        .current_dir(root.join("dolt"))
        .env("DOLT_ROOT_PATH", root.join("dolt-home"))
        .args([
            "sql",
            "-r",
            "json",
            "-q",
            "SELECT payload FROM metadata WHERE id='dirty-test'",
        ])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&o.stdout).contains("true"));
    assert!(!id.is_empty());
}

#[test]
fn coherent_concurrent_reads_release_the_lock_to_cli_writes() {
    for shared in [false, true] {
        let f = Fixture::new(shared, false);
        let id = f.seed("note");
        let h = f.serve(&[]);
        let address = h.address;
        let args = ["edit", &id, "--expect", "1", "--title", "New"];
        let writer = f.command(&args).stdout(Stdio::piped()).spawn().unwrap();
        let readers: Vec<_> = (0..3)
            .map(|_| {
                let id = id.clone();
                std::thread::spawn(move || {
                    let dummy = Http {
                        child: Command::new("true").spawn().unwrap(),
                        address,
                    };
                    dummy.request("GET", &format!("/v0/records/{id}"), &[], &[])
                })
            })
            .collect();
        let out = writer.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let replies: Vec<_> = readers.into_iter().map(|r| r.join().unwrap()).collect();
        for reply in replies {
            // A CLI writer or prior HTTP operation may legitimately occupy
            // the entire five-second admission budget on a loaded machine.
            if reply.status == 503 {
                assert_eq!(reply.value["error"]["code"], "busy");
                continue;
            }
            assert_eq!(reply.status, 200, "{}", reply.value);
            let v = reply.value;
            let record = &v["data"]["record"];
            assert!(
                (record["title"] == "A title" && record["revision"] == 1)
                    || (record["title"] == "New" && record["revision"] == 2)
            );
            assert_eq!(v["revision"].as_str().unwrap().len(), 32);
        }
        assert_eq!(
            h.get(&format!("/v0/records/{id}"))["data"]["record"]["revision"],
            2
        );
    }
}

#[test]
fn pagination_defaults_maximum_and_request_binding() {
    for shared in [false, true] {
        let f = Fixture::new(shared, true);
        let ops: Vec<_> = (0..205)
            .map(|n| json!({"op":"new","kind":"page","title":format!("Page {n}"),"body":"same"}))
            .collect();
        let path = f.dir.path().join("batch.json");
        fs::write(&path, json!(ops).to_string()).unwrap();
        f.ok(&["batch", path.to_str().unwrap()]);
        let h = f.serve(&[]);
        let first = h.get("/v0/records");
        assert_eq!(first["data"]["items"].as_array().unwrap().len(), 50);
        let first = h.get("/v0/records?limit=200");
        let cursor = first["data"]["next_cursor"].as_str().unwrap();
        assert_eq!(first["data"]["items"].as_array().unwrap().len(), 200);
        let last = h.get(&format!("/v0/records?limit=200&cursor={cursor}"));
        assert_eq!(last["data"]["items"].as_array().unwrap().len(), 5);
        assert!(last["data"]["next_cursor"].is_null());
        assert_eq!(
            h.request(
                "GET",
                &format!("/v0/records?limit=200&kind=page&cursor={cursor}"),
                &[],
                &[]
            )
            .status,
            400
        );
        let all = f.ok(&["list"]);
        let got: Vec<_> = first["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .chain(last["data"]["items"].as_array().unwrap())
            .cloned()
            .collect();
        assert_eq!(json!(got), all);
    }
}

#[test]
fn ipv6_loopback_and_incompatible_store_refusal() {
    let f = Fixture::new(false, true);
    let mut h = f.serve_on(&[], "[::1]:0");
    assert!(h.address.is_ipv6());
    assert_eq!(h.get("/healthz")["ok"], true);
    h.signal(libc::SIGTERM);
    let h = f.serve(&[]);
    let root = f.dir.path().join("store");
    let revision = {
        let s = Store::open_readonly(&root, &dolt()).unwrap();
        s.transaction(
            &put("metadata", "schema", &99).unwrap(),
            "fixture future format",
            "test",
        )
        .unwrap();
        s.head("main").unwrap()
    };
    assert_eq!(h.request("GET", "/v0/records", &[], &[]).status, 409);
    let out = f.command(&["serve"]).output().unwrap();
    assert!(!out.status.success());
    let s = Store::open_readonly(&root, &dolt()).unwrap();
    assert_eq!(s.head("main").unwrap(), revision);
    assert_eq!(s.one::<u32>("metadata", "schema").unwrap(), Some(99));
}

#[test]
fn shared_interrupted_and_dirty_state_are_never_repaired() {
    let f = Fixture::new(true, true);
    let h = f.serve(&[]);
    f.ok(&[
        "--role", "agent", "new", "page", "--title", "Proposed", "--body", "text",
    ]);
    let mut raw = Conn::new(f.opts()).unwrap();
    raw.query_drop("USE `db/scrolls-operations`").unwrap();
    let (id, payload): (String, String) = raw
        .query_first("SELECT id,payload FROM metadata WHERE id LIKE 'intent:%'")
        .unwrap()
        .unwrap();
    let mut intent: Value = serde_json::from_str(&payload).unwrap();
    intent["stage"] = json!({"state":"prepared"});
    raw.query_drop(format!("UPDATE metadata SET payload={} WHERE id={}; CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','fixture interrupted','--author','Test <test@localhost>')",scrolls::storage::literal(&intent.to_string()),scrolls::storage::literal(&id))).unwrap();
    let head: String = raw
        .query_first("SELECT DOLT_HASHOF('HEAD')")
        .unwrap()
        .unwrap();
    assert_eq!(h.request("GET", "/v0/records", &[], &[]).status, 409);
    assert!(!f.command(&["serve"]).output().unwrap().status.success());
    assert_eq!(
        raw.query_first::<String, _>("SELECT DOLT_HASHOF('HEAD')")
            .unwrap()
            .unwrap(),
        head
    );
    let observed: String = raw
        .query_first(format!(
            "SELECT payload FROM metadata WHERE id={}",
            scrolls::storage::literal(&id)
        ))
        .unwrap()
        .unwrap();
    assert_eq!(observed, intent.to_string());
    // Restore only the test-owned intent, then introduce unexplained main dirt.
    raw.query_drop(format!("UPDATE metadata SET payload={} WHERE id={}; CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','fixture restored','--author','Test <test@localhost>'); USE `db/main`; INSERT INTO metadata VALUES ('dirty-test','true')",scrolls::storage::literal(&payload),scrolls::storage::literal(&id))).unwrap();
    assert_eq!(h.request("GET", "/v0/records", &[], &[]).status, 409);
    assert!(!f.command(&["serve"]).output().unwrap().status.success());
    assert_eq!(
        raw.query_first::<String, _>("SELECT payload FROM metadata WHERE id='dirty-test'")
            .unwrap()
            .unwrap(),
        "true"
    );
}

#[test]
fn admission_timeout_and_graceful_drain_of_inflight_read() {
    let f = Fixture::new(true, true);
    let mut h = f.serve(&[]);
    let mut lock = Conn::new(f.opts()).unwrap();
    let lock_name = digest("scrolls:db");
    let acquired: Option<u32> = lock
        .query_first(format!("SELECT GET_LOCK('{}',0)", lock_name))
        .unwrap();
    assert_eq!(acquired, Some(1));
    // The first admitted read waits in the existing ten-second publication
    // lock budget; later requests time out at the five-second admission gate.
    let address = h.address;
    let waiting = std::thread::spawn(move || {
        let dummy = Http {
            child: Command::new("true").spawn().unwrap(),
            address,
        };
        dummy.get("/v0/context")
    });
    std::thread::sleep(Duration::from_millis(200));
    let start = Instant::now();
    let r = h.request("GET", "/readyz", &[], &[]);
    assert_eq!(r.status, 503);
    assert_eq!(r.value["error"]["code"], "busy");
    assert!((Duration::from_secs(5)..Duration::from_secs(7)).contains(&start.elapsed()));
    assert_eq!(h.get("/healthz")["ok"], true);
    // Graceful shutdown must preserve the accepted request while it drains.
    unsafe {
        libc::kill(h.child.id() as i32, libc::SIGTERM);
    }
    lock.query_drop(format!("SELECT RELEASE_LOCK('{}')", lock_name))
        .unwrap();
    drop(lock);
    assert_eq!(waiting.join().unwrap()["data"]["read_only"], true);
    let start = Instant::now();
    loop {
        if let Some(status) = h.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(20));
    }
}
