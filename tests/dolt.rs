//! Integration tests intentionally fail, rather than silently skip, without the pinned binary.
use scrolls::storage::{Store, put};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};
fn dolt() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".tools/dolt")
}
struct Fixture {
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let f = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        f.ok(&["init"]);
        f
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_scrolls"));
        c.args(["--json", "--actor", "alice"]);
        if !args.contains(&"--role") {
            c.args(["--role", "human"]);
        }
        c.arg("--dolt")
            .arg(dolt())
            .arg("--store")
            .arg(self.dir.path().join("store"))
            .args(args);
        c
    }
    fn call(&self, args: &[&str]) -> (bool, Value) {
        let o = self.command(args).output().unwrap();
        let v = serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
            panic!(
                "{args:?}: {e} {} {}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        });
        (o.status.success(), v)
    }
    fn ok(&self, args: &[&str]) -> Value {
        let (ok, v) = self.call(args);
        assert!(ok, "{args:?}: {v}");
        v["data"].clone()
    }
    fn fail(&self, args: &[&str], message: &str) {
        let (ok, v) = self.call(args);
        assert!(!ok, "{args:?}: {v}");
        assert!(v.to_string().contains(message), "{v}");
    }
}
#[test]
fn retry_concurrency_stale_and_questions() {
    let f = Fixture::new();
    let args = [
        "--op-id",
        "e1",
        "new",
        "evidence",
        "--title",
        "Source",
        "--body",
        "facts ' ; DROP TABLE records; -- \\ 🌍",
    ];
    let first = f.ok(&args);
    assert_eq!(first, f.ok(&args));
    f.fail(
        &[
            "--op-id", "e1", "new", "note", "--title", "changed", "--body", "",
        ],
        "different arguments",
    );
    let id = first["id"].as_str().unwrap();
    let mut children = vec![];
    for n in 0..4 {
        children.push(
            f.command(&[
                "new",
                "note",
                "--title",
                &format!("parallel {n}"),
                "--body",
                "ok",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
        )
    }
    for c in children {
        let o = c.wait_with_output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout))
    }
    assert_eq!(f.ok(&["list"]).as_array().unwrap().len(), 5);
    f.ok(&["edit", id, "--expect", "1", "--body", "updated"]);
    f.fail(
        &["edit", id, "--expect", "1", "--body", "stale"],
        "stale edit",
    );
    let q = f.ok(&["ask", "What now?"]);
    let q = q["id"].as_str().unwrap();
    let a = f.ok(&["answer", q, "Yes", "--expect", "1"]);
    let a = a["id"].as_str().unwrap();
    f.ok(&["accept", a, "--expect", "1", "--expect-question", "2"]);
    assert_eq!(f.ok(&["show", q])["record"]["status"], "answered");
    assert!(!f.ok(&["history", q]).as_array().unwrap().is_empty());
}
#[test]
fn proposals_isolation_merge_and_conflict() {
    let f = Fixture::new();
    let e = f.ok(&[
        "new", "evidence", "--title", "Evidence", "--body", "observed",
    ]);
    let e = e["id"].as_str().unwrap();
    let args = [
        "--op-id",
        "knowledge-1",
        "new",
        "knowledge",
        "--title",
        "Knowledge",
        "--body",
        "supported",
        "--supports",
        e,
    ];
    let p = f.ok(&args);
    let branch = p["branch"].as_str().unwrap();
    let id = p["id"].as_str().unwrap();
    assert_eq!(f.ok(&args), p);
    f.fail(&["show", id], "unknown record");
    f.ok(&["review", branch]);
    f.fail(&["--role", "agent", "review", "accept", branch], "human");
    f.ok(&[
        "--proposal",
        branch,
        "status",
        id,
        "current",
        "--expect",
        "1",
    ]);
    f.ok(&["review", "accept", branch]);
    assert_eq!(f.ok(&["show", id])["record"]["status"], "current");
    let p1 = f.ok(&["edit", id, "--expect", "2", "--body", "one"]);
    let p2 = f.ok(&["edit", id, "--expect", "2", "--body", "two"]);
    let b1 = p1["branch"].as_str().unwrap();
    let b2 = p2["branch"].as_str().unwrap();
    f.ok(&["review", "accept", b1]);
    let before = f.ok(&["show", id]);
    f.fail(&["review", "accept", b2], "conflict");
    assert_eq!(f.ok(&["show", id]), before);
    f.ok(&[
        "review",
        "reject",
        b2,
        "--reason",
        "conflicting alternative",
    ]);
    assert!(
        f.ok(&["proposals"])
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["status"] == "rejected")
    );
}
#[test]
fn rollback_interruption_and_dirty_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let s = Store::open(&root, &dolt(), true).unwrap();
    let before = s.revision().unwrap();
    assert!(
        s.transaction(
            "INSERT INTO operations VALUES ('bad','{}'); INSERT INTO no_such_table VALUES (1);",
            "must roll back",
            "test"
        )
        .is_err()
    );
    assert_eq!(s.revision().unwrap(), before);
    assert!(s.one::<Value>("operations", "bad").unwrap().is_none());
    s.clean().unwrap();
    let mut child = Command::new(dolt())
        .env("DOLT_ROOT_PATH", root.join("dolt-home"))
        .env("DOLT_DISABLE_EVENT_FLUSH", "1")
        .current_dir(&s.db)
        .args(["sql", "-r", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(b"START TRANSACTION; INSERT INTO operations VALUES ('interrupted','{}'); SELECT 'transaction-started' AS marker; SELECT SLEEP(60);\n").unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout).lines() {
            if line.unwrap().contains("transaction-started") {
                let _ = tx.send(());
                break;
            }
        }
    });
    let ready = rx.recv_timeout(std::time::Duration::from_secs(10));

    child.kill().unwrap();
    child.wait().unwrap();
    ready.expect("Dolt must execute the insert before the interruption");
    assert!(
        s.one::<Value>("operations", "interrupted")
            .unwrap()
            .is_none()
    );
    s.clean().unwrap();
    s.sql(&put("operations", "external", &json!({"unexpected":true})).unwrap())
        .unwrap();
    drop(s);
    assert!(
        Store::open(&root, &dolt(), false)
            .err()
            .unwrap()
            .to_string()
            .contains("dirty")
    );
}
#[test]
fn failed_validation_preserves_main() {
    let f = Fixture::new();
    let e = f.ok(&["new", "evidence", "--title", "Evidence", "--body", "facts"]);
    let p = f.ok(&[
        "new",
        "knowledge",
        "--title",
        "Knowledge",
        "--body",
        "text",
        "--supports",
        e["id"].as_str().unwrap(),
    ]);
    let branch = p["branch"].as_str().unwrap();
    let root = f.dir.path().join("store");
    {
        let s = Store::open(&root, &dolt(), false).unwrap();
        s.checkout(branch).unwrap();
        let mut g = s.graph().unwrap();
        g.edges.clear();
        s.transaction("DELETE FROM edges;", "external invalid edit", "test")
            .unwrap();
    }
    f.fail(&["review", "accept", branch], "structural validation");
    f.fail(&["show", p["id"].as_str().unwrap()], "unknown record");
    f.ok(&["doctor"]);
}
#[test]
fn review_crash_recovery_and_dry_run() {
    let f = Fixture::new();
    let root = f.dir.path().join("store");
    let base;
    {
        let s = Store::open(&root, &dolt(), false).unwrap();
        base = s.head("main").unwrap();
        let temp = format!("review/{}", uuid::Uuid::new_v4().simple());
        s.run(&["checkout", "-b", &temp]).unwrap();
        s.journal_review(&temp, &base).unwrap();
        s.sql("INSERT INTO operations VALUES ('unfinished','{}')")
            .unwrap();
    }
    {
        let s = Store::open(&root, &dolt(), false).unwrap();
        assert_eq!(s.head("main").unwrap(), base);
        assert!(
            s.one::<Value>("operations", "unfinished")
                .unwrap()
                .is_none()
        );
    }
    f.ok(&[
        "--dry-run",
        "new",
        "evidence",
        "--title",
        "preview",
        "--body",
        "facts",
    ]);
    assert_eq!(f.ok(&["list"]), json!([]));
}
#[test]
fn committed_import_render_and_frozen_history() {
    let f = Fixture::new();
    let repo = f.dir.path().join("repo");
    std::fs::create_dir_all(repo.join("docs/raw")).unwrap();
    std::fs::create_dir_all(repo.join("docs/questions")).unwrap();
    let evidence = "---\ntitle: Source\ndoc_type: raw\nstatus: captured\nowner: alice\nsource_documents: [https://example.com/external]\n---\n\nverbatim\n";
    std::fs::write(repo.join("docs/raw/a.md"), evidence).unwrap();
    std::fs::write(repo.join("docs/raw/archive.md"), "No frontmatter\r\n").unwrap();
    std::fs::write(repo.join("docs/raw/image.png"), [0, 255, 128, 4]).unwrap();
    std::fs::write(
        repo.join("docs/questions/Q-20261016-abcd-q.md"),
        "---\ndoc_type: question\ntitle: Historical\nstatus: open\nowner: alice\n---\nFrozen\n",
    )
    .unwrap();
    std::fs::create_dir_all(repo.join("docs/decisions")).unwrap();
    for (n, supersedes) in [(1, "[]"), (2, "[ADR-0001]")] {
        std::fs::write(repo.join(format!("docs/decisions/{n:04}-decision.md")),format!("---\ntitle: Decision {n}\ndoc_type: adr\nstatus: accepted\nowner: alice\nsource_documents: [../raw/a.md]\nsupersedes: {supersedes}\n---\nDecision\n")).unwrap();
    }
    for args in [
        vec!["init", "-q"],
        vec!["add", "docs"],
        vec![
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-qm",
            "fixture",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(["-c", "commit.gpgsign=false"])
                .arg("-C")
                .arg(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        )
    }
    std::fs::write(repo.join("docs/untracked.md"), "excluded").unwrap();
    let docs = repo.join("docs");
    let docs = docs.to_str().unwrap();
    let first = f.ok(&["import", "markdown", docs, "--rev", "HEAD"]);
    assert_eq!(first["import"]["files"], 6);
    assert_eq!(
        f.ok(&["import", "markdown", docs, "--rev", "HEAD"])["identical_replay"],
        true
    );
    let replay = [
        "--op-id",
        "import-replay",
        "import",
        "markdown",
        docs,
        "--rev",
        "HEAD",
    ];
    assert_eq!(f.ok(&replay), f.ok(&replay));
    f.fail(
        &[
            "--op-id",
            "import-replay",
            "new",
            "note",
            "--title",
            "reuse",
            "--body",
            "",
        ],
        "different arguments",
    );
    assert!(
        f.ok(&["show", "ADR-0002"])["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["rel"] == "supersedes")
    );
    let q = f.ok(&["show", "Q-20261016-abcd"]);
    let q = q["record"]["id"].as_str().unwrap();
    f.fail(&["answer", q, "invented", "--expect", "1"], "immutable");
    let out = f.dir.path().join("render");
    f.ok(&[
        "render",
        "--format",
        "liskov",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(
        std::fs::read(out.join("raw/a.md")).unwrap(),
        evidence.as_bytes()
    );
    assert_eq!(
        std::fs::read(out.join("raw/image.png")).unwrap(),
        [0, 255, 128, 4]
    );
    assert!(!out.join("untracked.md").exists());
    f.fail(&["render", "--out", out.to_str().unwrap()], "fresh");
    let outside = f.dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let link = f.dir.path().join("link");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    f.fail(&["render", "--out", link.to_str().unwrap()], "fresh");
    f.ok(&["new", "note", "--title", "Native", "--body", "local"]);
    f.fail(
        &[
            "render",
            "--format",
            "liskov",
            "--out",
            f.dir.path().join("unsupported").to_str().unwrap(),
        ],
        "native addition",
    );
    std::fs::write(repo.join("docs/raw/a.md"), "changed source").unwrap();
    assert!(
        Command::new("git")
            .args(["-c", "commit.gpgsign=false"])
            .arg("-C")
            .arg(&repo)
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@localhost",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "-am",
                "change",
                "-q"
            ])
            .status()
            .unwrap()
            .success()
    );
    f.fail(
        &["import", "markdown", docs, "--rev", "HEAD"],
        "changed-source",
    );
}
