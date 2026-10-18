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
        c.arg("--json");
        if !args.contains(&"--actor") {
            c.args(["--actor", "alice"]);
        }
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
    fn review_token(&self, branch: &str) -> String {
        self.ok(&["review", branch])["review_token"]
            .as_str()
            .unwrap()
            .into()
    }
    fn batch_file(&self, ops: Value) -> PathBuf {
        let path = self
            .dir
            .path()
            .join(format!("{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&path, serde_json::to_vec(&ops).unwrap()).unwrap();
        path
    }
    fn batch(&self, ops: Value) -> Value {
        let file = self.batch_file(ops);
        self.ok(&["batch", file.to_str().unwrap()])
    }
    fn supported(&self, evidence: &str, title: &str) -> Value {
        let rev = self.ok(&["show", evidence])["record"]["revision"].clone();
        let mut result = self.batch(json!([
            {"op":"new","kind":"knowledge","handle":"doc","title":title,"body":"text"},
            {"op":"link","from":evidence,"rel":"supports","to":"$doc","expect":rev}
        ]));
        result["id"] = result["handles"]["doc"].clone();
        result
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
    let q = f.ok(&["new", "question", "--title", "What now?", "--body", ""]);
    let q = q["id"].as_str().unwrap();
    let a=f.batch(json!([{"op":"new","kind":"answer","handle":"a","title":"Yes","body":"Yes"},{"op":"link","from":"$a","rel":"answers","to":q,"expect_to":1}]));
    let a = a["handles"]["a"].as_str().unwrap();
    f.batch(json!([{"op":"status","id":a,"state":"accepted","expect":1},{"op":"status","id":q,"state":"answered","expect":2}]));
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
    let file=f.batch_file(json!([{"op":"new","handle":"doc","kind":"knowledge","title":"Knowledge","body":"supported"},{"op":"link","from":e,"rel":"supports","to":"$doc","expect":1}]));
    let args = ["--op-id", "knowledge-1", "batch", file.to_str().unwrap()];
    let p = f.ok(&args);
    let branch = p["branch"].as_str().unwrap();
    let id = p["handles"]["doc"].as_str().unwrap();
    assert_eq!(f.ok(&args), p);
    f.fail(&["show", id], "unknown record");
    f.ok(&["review", branch]);
    f.fail(
        &[
            "--role",
            "agent",
            "review",
            "accept",
            branch,
            "--review-token",
            &f.review_token(branch),
        ],
        "human",
    );
    f.ok(&[
        "--proposal",
        branch,
        "status",
        id,
        "current",
        "--expect",
        "1",
    ]);
    f.ok(&[
        "review",
        "accept",
        branch,
        "--review-token",
        &f.review_token(branch),
    ]);
    assert_eq!(f.ok(&["show", id])["record"]["status"], "current");
    let p1 = f.ok(&["edit", id, "--expect", "2", "--body", "one"]);
    let p2 = f.ok(&["edit", id, "--expect", "2", "--body", "two"]);
    let b1 = p1["branch"].as_str().unwrap();
    let b2 = p2["branch"].as_str().unwrap();
    f.ok(&[
        "review",
        "accept",
        b1,
        "--review-token",
        &f.review_token(b1),
    ]);
    let before = f.ok(&["show", id]);
    f.fail(
        &[
            "review",
            "accept",
            b2,
            "--review-token",
            &f.review_token(b2),
        ],
        "conflict",
    );
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
    let p = f.supported(e["id"].as_str().unwrap(), "Knowledge");
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
    f.fail(
        &[
            "review",
            "accept",
            branch,
            "--review-token",
            &f.review_token(branch),
        ],
        "missing-support",
    );
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
    assert_eq!(
        f.ok(&["show", "ADR-0002"])["record"]["metadata"]["number"],
        "0002"
    );
    // Simulate a pre-metadata store; read normalization must not persist a write.
    let root = f.dir.path().join("store");
    let old_revision;
    let old_payload;
    let adr_id;
    {
        let store = Store::open(&root, &dolt(), false).unwrap();
        store.checkout("main").unwrap();
        let graph = store.graph().unwrap();
        adr_id = graph.resolve("ADR-0002").unwrap();
        let mut old = serde_json::to_value(&graph.records[&adr_id]).unwrap();
        old.as_object_mut().unwrap().remove("metadata");
        old["original"].as_object_mut().unwrap().remove("metadata");
        store
            .transaction(
                &put("records", &adr_id, &old).unwrap(),
                "old store fixture",
                "test",
            )
            .unwrap();
        old_revision = store.revision().unwrap();
        old_payload = old;
    }
    assert_eq!(
        f.ok(&["show", "ADR-0002"])["record"]["metadata"]["number"],
        "0002"
    );
    {
        let store = Store::open(&root, &dolt(), false).unwrap();
        assert_eq!(store.revision().unwrap(), old_revision);
        assert_eq!(
            store.one::<Value>("records", &adr_id).unwrap().unwrap(),
            old_payload
        );
    }
    let q = f.ok(&["show", "Q-20261016-abcd"]);
    let q = q["record"]["id"].as_str().unwrap();
    f.fail(&["status", q, "answered", "--expect", "1"], "immutable");
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
    let history_reference = f.ok(&[
        "--role",
        "agent",
        "new",
        "evidence",
        "--title",
        "Historical citation",
        "--body",
        &format!("[history]({q})"),
    ]);
    assert_eq!(history_reference["branch"], "main");
    assert_eq!(history_reference["records"].as_object().unwrap().len(), 1);
    assert_eq!(f.ok(&["show", q])["record"]["revision"], 1);
    f.ok(&["new", "note", "--title", "Native", "--body", "local"]);
    f.ok(&[
        "render",
        "--format",
        "liskov",
        "--out",
        f.dir.path().join("with-native").to_str().unwrap(),
    ]);
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

#[test]
fn reference_ownership_concurrency_proposals_and_retries() {
    let f = Fixture::new();
    let entity = f.ok(&["new", "entity", "--title", "Shared", "--body", ""]);
    let id = entity["id"].as_str().unwrap();
    f.ok(&[
        "review",
        "accept",
        entity["branch"].as_str().unwrap(),
        "--review-token",
        &f.review_token(entity["branch"].as_str().unwrap()),
    ]);
    let baseline = f.ok(&["show", id])["record"].clone();
    let mut children = vec![];
    for actor in ["one", "two"] {
        let file=f.batch_file(json!([{"op":"new","handle":"e","kind":"evidence","title":actor},{"op":"link","from":"$e","rel":"about","to":id}]));
        children.push(
            f.command(&[
                "--actor",
                actor,
                "--role",
                "agent",
                "batch",
                file.to_str().unwrap(),
            ])
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
    assert_eq!(f.ok(&["show", id])["record"], baseline);
    let e = f.ok(&[
        "new",
        "evidence",
        "--title",
        "Body",
        "--body",
        &format!("[entity]({id})"),
    ]);
    let eid = e["id"].as_str().unwrap();
    let args = [
        "--op-id",
        "unlink-ref",
        "unlink",
        eid,
        "related",
        id,
        "--expect",
        "1",
    ];
    assert_eq!(f.ok(&args), f.ok(&args));
    assert!(f.ok(&["show", eid])["edges"].as_array().unwrap().is_empty());
    f.fail(&["link", eid, "related", id, "--expect", "1"], "stale");
    f.fail(
        &[
            "link",
            eid,
            "related",
            id,
            "--expect",
            "2",
            "--expect-to",
            "999",
        ],
        "stale",
    );
    {
        let store = Store::open(&f.dir.path().join("store"), &dolt(), false).unwrap();
        store.checkout("main").unwrap();
        let mut r = store.graph().unwrap().records[id].clone();
        r.frozen = true;
        store
            .transaction(&put("records", id, &r).unwrap(), "freeze fixture", "test")
            .unwrap();
    }
    f.ok(&[
        "link",
        eid,
        "related",
        id,
        "--expect",
        "2",
        "--expect-to",
        "1",
    ]);
    f.fail(&["edit", id, "--expect", "1", "--body", "bad"], "immutable");
    f.fail(&["link", id, "related", eid, "--expect", "1"], "immutable");
}

#[test]
fn triggers_validate_authoring_acceptance_and_existing_store_reads() {
    let f = Fixture::new();
    for field in [
        "glob=[",
        "glob=1",
        "glob=[false]",
        "glob=\"\"",
        "scope_clause=\"\"",
        "scope_clause=null",
    ] {
        f.fail(
            &[
                "new", "doctrine", "--title", "Invalid", "--body", "rule", "--field", field,
            ],
            if field.starts_with("glob") {
                "glob"
            } else {
                "scope_clause"
            },
        );
    }
    let p = f.ok(&[
        "new",
        "doctrine",
        "--title",
        "Rule",
        "--body",
        &"mandatory ".repeat(3000),
        "--field",
        "glob=**/*.rs",
    ]);
    let id = p["id"].as_str().unwrap();
    let branch = p["branch"].as_str().unwrap();
    f.fail(
        &[
            "--proposal",
            branch,
            "edit",
            id,
            "--expect",
            "1",
            "--field",
            "glob=[",
        ],
        "glob",
    );
    f.ok(&[
        "--proposal",
        branch,
        "status",
        id,
        "active",
        "--expect",
        "1",
    ]);
    f.fail(
        &[
            "--proposal",
            branch,
            "link",
            id,
            "applies_to",
            "glob:[",
            "--expect",
            "2",
        ],
        "applies_to",
    );
    f.ok(&[
        "review",
        "accept",
        branch,
        "--review-token",
        &f.review_token(branch),
    ]);
    let packet = f.ok(&["prime"]);
    assert_eq!(packet["items"], json!([]));
    let invalid = f.ok(&["edit", id, "--expect", "2", "--body", "changed"]);
    let bad_branch = invalid["branch"].as_str().unwrap();
    {
        let s = Store::open(&f.dir.path().join("store"), &dolt(), false).unwrap();
        s.checkout(bad_branch).unwrap();
        let mut r = s.graph().unwrap().records[id].clone();
        r.fields = json!({"glob":false});
        s.transaction(
            &put("records", id, &r).unwrap(),
            "old invalid proposal",
            "test",
        )
        .unwrap();
    }
    f.fail(
        &[
            "review",
            "accept",
            bad_branch,
            "--review-token",
            &f.review_token(bad_branch),
        ],
        "glob",
    );
    {
        let s = Store::open(&f.dir.path().join("store"), &dolt(), false).unwrap();
        s.checkout("main").unwrap();
        let mut r = s.graph().unwrap().records[id].clone();
        r.fields = json!({"glob":"["});
        s.transaction(
            &put("records", id, &r).unwrap(),
            "old invalid active rule",
            "test",
        )
        .unwrap();
    }
    f.ok(&["prime"]);
    let query = f.batch_file(json!({"op":"applicability"}));
    f.fail(&["query", "--file", query.to_str().unwrap()], "glob");
    assert!(
        f.call(&["lint"]).1["data"]["errors"]
            .to_string()
            .contains("glob")
    );
    f.fail(
        &[
            "edit", id, "--body", "change", "--field", "glob=[", "--expect", "2",
        ],
        "glob",
    );
}

#[test]
fn prime_pins_are_reviewed_and_audit_revisions() {
    let f = Fixture::new();
    let r = f.ok(&[
        "new",
        "note",
        "--title",
        "Startup",
        "--body",
        &"read this".repeat(3000),
    ]);
    let id = r["id"].as_str().unwrap();
    let schema = f.ok(&["schema", "show"]);
    let mut config = schema["schema"].clone();
    config["pins"] = json!([id, id]);
    let path = f.batch_file(config);
    let p = f.ok(&[
        "schema",
        "apply",
        path.to_str().unwrap(),
        "--expect",
        schema["schema_revision"].as_str().unwrap(),
    ]);
    assert_eq!(f.ok(&["prime"])["items"], json!([]));
    f.ok(&[
        "review",
        "accept",
        p["branch"].as_str().unwrap(),
        "--review-token",
        &f.review_token(p["branch"].as_str().unwrap()),
    ]);
    let p = f.ok(&["--session", "pinned", "prime", "--budget", "1"]);
    assert_eq!(p["items"].as_array().unwrap().len(), 1);
    assert!(p["over_budget_bytes"].as_u64().unwrap() > 0);
    let audit = f.ok(&["consumed", "--for-session", "pinned"]);
    assert_eq!(audit[0]["record_revisions"][id], 1);
}

#[test]
fn work_references_obey_review_retry_and_render_contracts() {
    let f = Fixture::new();
    let target = "beads://00000000-0000-4000-8000-000000000001/bd-a";
    let e = f.ok(&["new", "evidence", "--title", "Readback", "--body", "facts"]);
    let id = e["id"].as_str().unwrap();
    let args = [
        "--op-id",
        "work-link",
        "link",
        id,
        "produced_by",
        target,
        "--expect",
        "1",
    ];
    let linked = f.ok(&args);
    assert_eq!(f.ok(&args), linked);
    f.fail(
        &["link", id, "related_work", target, "--expect", "1"],
        "stale edit",
    );
    assert_eq!(f.ok(&["beads", "refs", target])["documents"][0]["id"], id);
    assert_eq!(
        f.ok(&["show", id])["related_work"][0]["bead"]["resolution"],
        "unresolved"
    );
    let knowledge = f.supported(id, "Conclusion");
    let kid = knowledge["id"].as_str().unwrap();
    let proposal = knowledge["branch"].as_str().unwrap();
    f.ok(&[
        "--proposal",
        proposal,
        "link",
        kid,
        "implemented_by",
        target,
        "--expect",
        "1",
    ]);
    assert_eq!(
        f.ok(&["beads", "refs", target])["documents"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    f.fail(
        &["--proposal", proposal, "beads", "publish", target],
        "main",
    );
    f.ok(&[
        "review",
        "accept",
        proposal,
        "--review-token",
        &f.review_token(proposal),
    ]);
    assert_eq!(
        f.ok(&["beads", "refs", target])["documents"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let packet = f.ok(&["context", "--bead", target]);
    assert!(
        packet["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == kid && v["summary_only"] == true)
    );
    let out = f.dir.path().join("native");
    f.ok(&["render", "--out", out.to_str().unwrap()]);
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(out.join("SCROLLS-MANIFEST.json")).unwrap()).unwrap();
    let rendered_path = manifest["records"][id].as_str().unwrap();
    assert!(
        std::fs::read_to_string(out.join(rendered_path))
            .unwrap()
            .contains("## Related work")
    );
    let bead_page = std::fs::read_to_string(
        out.join("scrolls/beads/00000000-0000-4000-8000-000000000001/bd-a.md"),
    )
    .unwrap();
    assert!(bead_page.contains(rendered_path));
    assert!(
        out.join("scrolls/beads/00000000-0000-4000-8000-000000000001/bd-a.md")
            .exists()
    );
    let legacy = f.dir.path().join("legacy");
    f.ok(&[
        "render",
        "--format",
        "liskov",
        "--out",
        legacy.to_str().unwrap(),
    ]);
    let normalized: Value =
        serde_json::from_slice(&std::fs::read(legacy.join("scrolls/graph.json")).unwrap()).unwrap();
    assert!(
        normalized["edges"]
            .as_object()
            .unwrap()
            .values()
            .any(|edge| edge["to"] == target)
    );
    assert!(
        std::fs::read_to_string(legacy.join(rendered_path))
            .unwrap()
            .contains(target)
    );
    let before = f.ok(&["show", id]);
    f.ok(&[
        "--dry-run",
        "unlink",
        id,
        "produced_by",
        target,
        "--expect",
        "3",
    ]);
    assert_eq!(before, f.ok(&["show", id]));
}

#[test]
fn schema_upgrade_preserves_records_and_refuses_pending_proposals() {
    let f = Fixture::new();
    let e = f.ok(&["new", "evidence", "--title", "Source", "--body", "facts"]);
    let id = e["id"].as_str().unwrap();
    let pending = f.ok(&["new", "entity", "--title", "Billing", "--body", ""]);
    // Construct a v1 fixture without changing any record or original bytes.
    {
        let s = Store::open(&f.dir.path().join("store"), &dolt(), false).unwrap();
        s.checkout("main").unwrap();
        s.transaction(&((put("metadata","schema",&1).unwrap()+"DROP TABLE import_sources; DROP TABLE import_files; DROP TABLE import_members;")+"DELETE FROM metadata WHERE id='store_id'; DELETE FROM registry WHERE id IN ('rel:related_work','rel:implemented_by','rel:produced_by');"),"v1 fixture","test").unwrap();
    }
    let before = f.ok(&["show", id]);
    f.fail(
        &["new", "note", "--title", "blocked", "--body", ""],
        "migrate",
    );
    f.fail(&["migrate"], "pending proposals");
    f.ok(&[
        "review",
        "reject",
        pending["branch"].as_str().unwrap(),
        "--reason",
        "fixture",
    ]);
    f.ok(&["--dry-run", "migrate"]);
    assert_eq!(f.ok(&["doctor"])["schema_version"], 1);
    let migrated = f.ok(&["--op-id", "migration", "migrate"]);
    assert_eq!(migrated["schema_version"], 4);
    assert_eq!(migrated, f.ok(&["--op-id", "migration", "migrate"]));
    assert_eq!(f.ok(&["doctor"])["schema_version"], 4);
    assert_eq!(before["record"], f.ok(&["show", id])["record"]);
    let uri = format!(
        "scrolls://{}/{}",
        migrated["store_id"].as_str().unwrap(),
        id
    );
    assert_eq!(before["record"], f.ok(&["show", &uri])["record"]);
    f.fail(
        &[
            "show",
            &format!("scrolls://00000000-0000-4000-8000-000000000009/{id}"),
        ],
        "identity mismatch",
    );
    f.ok(&["review", pending["branch"].as_str().unwrap()]);
    f.ok(&["new", "entity", "--title", "After migration", "--body", ""]);
    assert_eq!(migrated, f.ok(&["--op-id", "migration", "migrate"]));
    assert_eq!(f.ok(&["migrate"])["already_current"], true);
}

#[test]
fn custom_ontologies_complete_cli_roundtrip_and_configuration_revalidation() {
    for schema_file in ["handbook.yaml", "research.yaml"] {
        let f = Fixture {
            dir: tempfile::tempdir().unwrap(),
        };
        let schema_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("examples")
            .join(schema_file);
        f.ok(&["init", "--schema", schema_path.to_str().unwrap()]);
        let (id, branch) = if schema_file == "handbook.yaml" {
            let p = f.ok(&[
                "--role",
                "agent",
                "new",
                "page",
                "--title",
                "Welcome",
                "--body",
                "Read me",
                "--field",
                "tags=[\"intro\"]",
            ]);
            (
                p["id"].as_str().unwrap().to_string(),
                p["branch"].as_str().unwrap().to_string(),
            )
        } else {
            let p=f.batch(json!([
                {"op":"new","handle":"trial","kind":"trial","title":"Measurement","fields":{"count":3,"score":1.5,"observed":"2026-10-16T12:00:00Z","data":{"x":[1,2]}}},
                {"op":"new","handle":"claim","kind":"hypothesis","title":"Claim","fields":{"claim":"Measurable"}},
                {"op":"link","from":"$trial","rel":"corroborates","to":"$claim","why":"measurement"}
            ]));
            (
                p["handles"]["claim"].as_str().unwrap().to_string(),
                p["branch"].as_str().unwrap().to_string(),
            )
        };
        f.fail(&["show", &id], "unknown record");
        f.ok(&["review", &branch]);
        f.ok(&[
            "review",
            "accept",
            &branch,
            "--review-token",
            &f.review_token(&branch),
        ]);
        assert!(f.ok(&["lint"])["errors"].as_array().unwrap().is_empty());
        let query =
            f.batch_file(json!({"op":"compare","field":"owner","cmp":"eq","value":"$actor"}));
        assert!(
            !f.ok(&[
                "query",
                "--file",
                query.to_str().unwrap(),
                "--at",
                "2026-10-16T12:00:00Z"
            ])["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            !f.ok(&["walk", &id, "--direction", "incoming"])["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let active = f.ok(&["schema", "show"]);
        let mut config = active["schema"].clone();
        config["pins"] = json!([id]);
        let file = f.batch_file(config.clone());
        f.ok(&["schema", "validate", file.to_str().unwrap()]);
        f.fail(
            &[
                "schema",
                "apply",
                file.to_str().unwrap(),
                "--expect",
                "stale",
            ],
            "stale schema",
        );
        let p = f.ok(&[
            "schema",
            "apply",
            file.to_str().unwrap(),
            "--expect",
            active["schema_revision"].as_str().unwrap(),
        ]);
        let view = f.ok(&["review", p["branch"].as_str().unwrap()]);
        assert!(
            view["configuration_diff"]
                .as_str()
                .unwrap()
                .contains("ontology")
        );
        f.ok(&[
            "review",
            "accept",
            p["branch"].as_str().unwrap(),
            "--review-token",
            &f.review_token(p["branch"].as_str().unwrap()),
        ]);
        assert_eq!(f.ok(&["prime", "--budget", "1"])["items"][0]["id"], id);
        let out = f.dir.path().join("export");
        f.ok(&["render", "--out", out.to_str().unwrap()]);
        let graph: Value =
            serde_json::from_slice(&std::fs::read(out.join("scrolls/graph.json")).unwrap())
                .unwrap();
        assert_eq!(graph["schema"], f.ok(&["schema", "show"])["schema"]);
        assert!(graph["schema_revision"].is_string());
        assert!(graph["records"].get(&id).is_some());
        let kind = f.ok(&["show", &id])["record"]["kind"]
            .as_str()
            .unwrap()
            .to_string();
        config["kinds"][&kind]["fields"]["required_new"] = json!({"type":"string","required":true});
        let bad = f.batch_file(config);
        f.fail(&["schema", "validate", bad.to_str().unwrap()], &id);
    }
    // The proposal is compatible when authored; main adds data that makes it incompatible.
    let f = Fixture::new();
    let active = f.ok(&["schema", "show"]);
    let mut config = active["schema"].clone();
    config["kinds"].as_object_mut().unwrap().remove("note");
    config["relations"]["produced_by"]["from"]
        .as_array_mut()
        .unwrap()
        .retain(|k| k != "note");
    let file = f.batch_file(config);
    let p = f.ok(&[
        "schema",
        "apply",
        file.to_str().unwrap(),
        "--expect",
        active["schema_revision"].as_str().unwrap(),
    ]);
    let note = f.ok(&["new", "note", "--title", "Concurrent", "--body", ""]);
    f.fail(
        &[
            "review",
            "accept",
            p["branch"].as_str().unwrap(),
            "--review-token",
            &f.review_token(p["branch"].as_str().unwrap()),
        ],
        "unknown configured kind",
    );
    f.ok(&["show", note["id"].as_str().unwrap()]);
    assert_eq!(f.ok(&["schema", "show"]), active);
    let mut added = active["schema"].clone();
    added["kinds"]["artifact"] = added["kinds"]["note"].clone();
    let file = f.batch_file(added);
    let p = f.ok(&[
        "schema",
        "apply",
        file.to_str().unwrap(),
        "--expect",
        active["schema_revision"].as_str().unwrap(),
    ]);
    let branch = p["branch"].as_str().unwrap();
    let record = f.ok(&[
        "--proposal",
        branch,
        "new",
        "artifact",
        "--title",
        "New configured kind",
        "--body",
        "",
    ]);
    f.ok(&[
        "review",
        "accept",
        branch,
        "--review-token",
        &f.review_token(branch),
    ]);
    f.ok(&["show", record["id"].as_str().unwrap()]);
}

#[test]
fn batches_are_atomic_retryable_and_guards_cannot_be_bypassed() {
    let f = Fixture::new();
    let q = f.ok(&["new", "question", "--title", "Owned", "--body", ""]);
    let q = q["id"].as_str().unwrap();
    let answer=f.batch(json!([{"op":"new","handle":"a","kind":"answer","title":"Answer"},{"op":"link","from":"$a","rel":"answers","to":q,"expect_to":1}]));
    let a = answer["handles"]["a"].as_str().unwrap();
    let before = f.ok(&["show", q]);
    let bad=f.batch_file(json!([{"op":"edit","id":q,"expect":2,"owner":"bob"},{"op":"status","id":a,"expect":1,"state":"accepted"},{"op":"status","id":q,"expect":2,"state":"answered"}]));
    f.fail(
        &["--actor", "bob", "batch", bad.to_str().unwrap()],
        "pre-change owner",
    );
    assert_eq!(before, f.ok(&["show", q]));
    let acceptance=f.batch_file(json!([{"op":"status","id":a,"expect":1,"state":"accepted"},{"op":"status","id":q,"expect":2,"state":"answered"}]));
    let long_op = "g".repeat(200);
    let proposal = f.ok(&[
        "--op-id",
        &long_op,
        "--actor",
        "assistant",
        "--role",
        "agent",
        "batch",
        acceptance.to_str().unwrap(),
    ]);
    let branch = proposal["branch"].as_str().unwrap();
    f.fail(
        &[
            "--actor",
            "bob",
            "review",
            "accept",
            branch,
            "--review-token",
            &f.review_token(branch),
        ],
        "pre-change owner",
    );
    assert_eq!(before, f.ok(&["show", q]));
    f.ok(&[
        "review",
        "accept",
        branch,
        "--review-token",
        &f.review_token(branch),
    ]);
    assert_eq!(f.ok(&["show", q])["record"]["status"], "answered");
    let note = f.ok(&["new", "note", "--title", "Atomic", "--body", ""]);
    let id = note["id"].as_str().unwrap();
    let bad=f.batch_file(json!([{"op":"edit","id":id,"expect":1,"body":"changed"},{"op":"status","id":id,"expect":1,"state":"bad"}]));
    f.fail(&["batch", bad.to_str().unwrap()], "invalid");
    assert_eq!(f.ok(&["show", id])["record"]["revision"], 1);
    let file=f.batch_file(json!([{"op":"edit","id":id,"expect":1,"body":"one"},{"op":"edit","id":id,"expect":1,"title":"two"}]));
    let args = ["--op-id", "atomic", "batch", file.to_str().unwrap()];
    let result = f.ok(&args);
    assert_eq!(result, f.ok(&args));
    assert_eq!(result["records"][id], 2);
    std::fs::write(&file, "[]").unwrap();
    f.fail(&args, "different arguments");
    for old in [
        "ask",
        "answer",
        "accept",
        "supersede",
        "inbox",
        "doctrine",
        "stale",
    ] {
        let args = match old {
            "ask" => vec![old, "Title"],
            "answer" => vec![old, q, "text", "--expect", "3"],
            "accept" => vec![old, a, "--expect", "2", "--expect-question", "3"],
            "supersede" => vec![
                old,
                q,
                "--with",
                a,
                "--why",
                "x",
                "--expect",
                "3",
                "--expect-to",
                "2",
            ],
            _ => vec![old],
        };
        f.fail(&args, "removed in 0.2.0");
    }
}

#[test]
fn format_two_migration_preserves_identity_receipts_and_historical_reads() {
    let f = Fixture::new();
    let args = [
        "--op-id",
        "prior-record",
        "new",
        "note",
        "--title",
        "Preserved",
        "--body",
        "original",
    ];
    let record = f.ok(&args);
    let id = record["id"].as_str().unwrap();
    let identity = f.ok(&["doctor"])["store_id"].clone();
    let historical = "proposal/legacy-format-two";
    {
        let s = Store::open(&f.dir.path().join("store"), &dolt(), false).unwrap();
        s.checkout("main").unwrap();
        s.transaction(
            &((put("metadata", "schema", &2).unwrap()
                + "DROP TABLE import_sources; DROP TABLE import_files; DROP TABLE import_members;")
                + "DELETE FROM metadata WHERE id='ontology';"),
            "old format fixture",
            "test",
        )
        .unwrap();
        let base = s.revision().unwrap();
        s.run(&["checkout", "-b", historical]).unwrap();
        let proposal = scrolls::model::Proposal {
            branch: historical.into(),
            base,
            actor: "alice".into(),
            status: "rejected".into(),
            reason: Some("historical fixture".into()),
        };
        s.transaction(
            &put("metadata", historical, &proposal).unwrap(),
            "historical proposal",
            "test",
        )
        .unwrap();
        s.checkout("main").unwrap();
    }
    let before = f.ok(&["show", id]);
    f.ok(&["migrate"]);
    assert_eq!(f.ok(&["doctor"])["store_id"], identity);
    assert_eq!(f.ok(&["show", id]), before);
    assert_eq!(f.ok(&args), record);
    assert_eq!(f.ok(&["prime"])["items"], json!([]));
    assert_eq!(
        f.ok(&["--proposal", historical, "show", id])["record"],
        before["record"]
    );
    f.fail(
        &[
            "--proposal",
            historical,
            "edit",
            id,
            "--expect",
            "1",
            "--body",
            "blocked",
        ],
        "migrate",
    );
}

#[test]
fn migration_translates_import_debt_from_originals_not_current_violations() {
    let f = Fixture::new();
    let root = f.dir.path().join("store");
    let id;
    {
        let s = Store::open(&root, &dolt(), false).unwrap();
        s.checkout("main").unwrap();
        let mut record = scrolls::model::Record::new("note", "Legacy", "", "alice").unwrap();
        id = record.id.clone();
        record.confidence = "legacy-invalid".into();
        record.original = Some(serde_json::to_value(&record).unwrap());
        let info = scrolls::model::ImportInfo {
            repository: "fixture".into(),
            commit: "fixture".into(),
            prefix: "docs".into(),
            files: 1,
            markdown: 1,
            untyped: 0,
            lint_debt: [format!("{id}:confidence")].into(),
        };
        let sql = put("records", &id, &record).unwrap()
            + &put("metadata", "import", &info).unwrap()
            + &put(
                "metadata",
                "import_edges",
                &Vec::<scrolls::model::Edge>::new(),
            )
            .unwrap()
            + &(put("metadata", "schema", &2).unwrap()
                + "DROP TABLE import_sources; DROP TABLE import_files; DROP TABLE import_members;")
            + "DELETE FROM metadata WHERE id='ontology';";
        s.transaction(&sql, "legacy import fixture", "test")
            .unwrap();
    }
    f.ok(&["migrate"]);
    let lint = f.ok(&["lint"]);
    assert_eq!(lint["errors"], json!([]));
    assert_eq!(lint["historical_debt"].as_array().unwrap().len(), 1);
    f.fail(
        &[
            "edit",
            &id,
            "--expect",
            "1",
            "--confidence",
            "new-invalid",
            "--body",
            "",
        ],
        "confidence",
    );
    {
        let s = Store::open(&root, &dolt(), false).unwrap();
        s.checkout("main").unwrap();
        let mut r = s.graph().unwrap().records[&id].clone();
        r.confidence = "new-invalid".into();
        s.transaction(
            &put("records", &id, &r).unwrap(),
            "simulate external corruption",
            "test",
        )
        .unwrap();
    }
    assert!(!f.call(&["lint"]).0);
}

// Capture every committed branch and receipt, plus recovery state. Read checkout
// position is incidental; stale decisions must not change any durable content.
fn review_state(f: &Fixture) -> Value {
    let root = f.dir.path().join("store");
    // Check before opening: recovery must not hide a journal left by a failure.
    assert!(!root.join("review-journal.json").exists());
    let s = Store::open(&root, &dolt(), false).unwrap();
    let mut state = serde_json::Map::new();
    for branch in s.branches().unwrap() {
        s.checkout(&branch).unwrap();
        s.clean().unwrap();
        state.insert(branch.clone(), json!({"head":s.head(&branch).unwrap(),"operations":s.rows::<Value>("operations").unwrap()}));
    }
    state.insert(
        "journal".into(),
        json!(std::fs::read(root.join("review-journal.json")).ok()),
    );
    Value::Object(state)
}

#[test]
fn review_tokens_reject_invalid_inputs_without_writes() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let f = Fixture::new();
    let p = f.ok(&["new", "entity", "--title", "Review", "--body", "original"]);
    let branch = p["branch"].as_str().unwrap();
    let review = f.ok(&["review", branch]);
    assert_eq!(review, f.ok(&["review", branch]));
    let token = review["review_token"].as_str().unwrap();
    assert!(
        token
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    );
    let mut payload: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(token.strip_prefix("v1.").unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(payload["main_revision"], review["main_revision"]);
    assert_eq!(payload["proposal_revision"], review["proposal_revision"]);
    let human = scrolls::presentation::human(&json!({"data":review}));
    assert!(human.find("Graph changes:").unwrap() < human.find("Review token:").unwrap());
    assert!(human.contains(&format!("--review-token {token}")));
    assert!(human.contains(review["main_revision"].as_str().unwrap()));
    assert!(human.contains(review["proposal_revision"].as_str().unwrap()));
    let before = review_state(&f);
    for dry in [false, true] {
        let prefix = if dry { vec!["--dry-run"] } else { vec![] };
        let mut args = prefix.clone();
        args.extend(["review", "accept", branch]);
        f.fail(&args, "requires --review-token");
        payload["proposal_revision"] = json!("short");
        let malformed = format!(
            "v1.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap())
        );
        for (bad, error) in [
            ("garbage", "malformed"),
            ("v1.%%%", "malformed"),
            ("v1.e30", "malformed"),
            ("v2.e30", "unsupported"),
            (malformed.as_str(), "malformed"),
        ] {
            let mut args = prefix.clone();
            args.extend([
                "--op-id",
                "invalid-review",
                "review",
                "accept",
                branch,
                "--review-token",
                bad,
            ]);
            f.fail(&args, error);
        }
    }
    f.fail(
        &[
            "--role",
            "agent",
            "review",
            "accept",
            branch,
            "--review-token",
            token,
        ],
        "human",
    );
    f.fail(
        &["review", branch, "--review-token", token],
        "only valid for review accept",
    );
    f.fail(
        &[
            "review",
            "reject",
            branch,
            "--reason",
            "no",
            "--review-token",
            token,
        ],
        "only valid for review accept",
    );
    f.fail(
        &[
            "--role", "agent", "review", "reject", branch, "--reason", "no",
        ],
        "human",
    );
    f.fail(&["review", "reject", branch], "requires --reason");
    f.ok(&[
        "--dry-run",
        "review",
        "reject",
        branch,
        "--reason",
        "preview",
    ]);
    assert_eq!(before, review_state(&f));
    let other = f.ok(&["new", "entity", "--title", "Other", "--body", ""]);
    let before = review_state(&f);
    f.fail(
        &[
            "review",
            "accept",
            other["branch"].as_str().unwrap(),
            "--review-token",
            token,
        ],
        "token is for",
    );
    assert_eq!(before, review_state(&f));
    f.ok(&["review", "reject", branch, "--reason", "not needed"]);
    f.fail(
        &["review", "accept", branch, "--review-token", token],
        "already rejected",
    );
}

#[test]
fn review_tokens_bind_all_proposal_changes_and_any_main_advance() {
    let f = Fixture::new();
    let e = f.ok(&["new", "evidence", "--title", "Evidence", "--body", "facts"]);
    let p = f.ok(&["new", "entity", "--title", "Subject", "--body", "initial"]);
    let branch = p["branch"].as_str().unwrap();
    let id = p["id"].as_str().unwrap();
    for args in [
        vec!["edit", id, "--expect", "1", "--body", "changed"],
        vec![
            "edit",
            id,
            "--expect",
            "2",
            "--body",
            "changed",
            "--field",
            "type=service",
        ],
        vec![
            "link",
            id,
            "related",
            e["id"].as_str().unwrap(),
            "--expect",
            "3",
        ],
    ] {
        let token = f.review_token(branch);
        let mut edit = vec!["--proposal", branch];
        edit.extend(args);
        f.ok(&edit);
        let before = review_state(&f);
        f.fail(
            &[
                "--op-id",
                "stale-proposal",
                "review",
                "accept",
                branch,
                "--review-token",
                &token,
            ],
            "proposal revision changed",
        );
        f.fail(
            &[
                "--dry-run",
                "review",
                "accept",
                branch,
                "--review-token",
                &token,
            ],
            "proposal revision changed",
        );
        assert_eq!(before, review_state(&f));
    }
    let token = f.review_token(branch);
    f.ok(&[
        "new",
        "note",
        "--title",
        "Unrelated main write",
        "--body",
        "",
    ]);
    let before = review_state(&f);
    f.fail(
        &[
            "--op-id",
            "stale-main",
            "review",
            "accept",
            branch,
            "--review-token",
            &token,
        ],
        "main revision changed",
    );
    f.fail(
        &[
            "--dry-run",
            "review",
            "accept",
            branch,
            "--review-token",
            &token,
        ],
        "main revision changed",
    );
    assert_eq!(before, review_state(&f));
    let review = f.ok(&["review", branch]);
    let token = review["review_token"].as_str().unwrap();
    let preview = f.ok(&[
        "--dry-run",
        "--op-id",
        "bound-accept",
        "review",
        "accept",
        branch,
        "--review-token",
        token,
    ]);
    assert_eq!(preview["validated"], true);
    assert_eq!(preview["validated_base"], review["main_revision"]);
    assert_eq!(preview["proposal_revision"], review["proposal_revision"]);
    assert_eq!(preview["review_token"], token);
    assert_eq!(before, review_state(&f));
    let args = [
        "--op-id",
        "bound-accept",
        "review",
        "accept",
        branch,
        "--review-token",
        token,
    ];
    let accepted = f.ok(&args);
    assert_eq!(accepted["validated_base"], review["main_revision"]);
    assert_eq!(accepted["proposal_revision"], review["proposal_revision"]);
    assert_eq!(accepted["review_token"], token);
    f.ok(&["new", "note", "--title", "Later main write", "--body", ""]);
    let before = review_state(&f);
    assert_eq!(accepted, f.ok(&args));
    let changed = f.review_token(branch);
    f.fail(
        &[
            "--op-id",
            "bound-accept",
            "review",
            "accept",
            branch,
            "--review-token",
            &changed,
        ],
        "different arguments",
    );
    assert_eq!(before, review_state(&f));
    let s = Store::open(&f.dir.path().join("store"), &dolt(), false).unwrap();
    s.checkout("main").unwrap();
    assert_eq!(
        s.one::<Value>("operations", "bound-accept")
            .unwrap()
            .unwrap()["result"],
        accepted
    );
}

#[test]
fn two_acceptances_reviewed_against_one_main_are_serialized() {
    let f = Fixture::new();
    let p = f.ok(&["new", "entity", "--title", "First", "--body", ""]);
    let q = f.ok(&["new", "entity", "--title", "Second", "--body", ""]);
    let p = p["branch"].as_str().unwrap();
    let q = q["branch"].as_str().unwrap();
    let pr = f.ok(&["review", p]);
    let qr = f.ok(&["review", q]);
    assert_eq!(pr["main_revision"], qr["main_revision"]);
    // Actual concurrent processes contend for the same store lock.
    let children: Vec<_> = [(p, &pr), (q, &qr)]
        .into_iter()
        .map(|(branch, review)| {
            f.command(&[
                "review",
                "accept",
                branch,
                "--review-token",
                review["review_token"].as_str().unwrap(),
            ])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap()
        })
        .collect();
    let outputs: Vec<_> = children
        .into_iter()
        .map(|c| c.wait_with_output().unwrap())
        .collect();
    assert_eq!(outputs.iter().filter(|o| o.status.success()).count(), 1);
    let loser = outputs.iter().position(|o| !o.status.success()).unwrap();
    assert!(String::from_utf8_lossy(&outputs[loser].stdout).contains("main revision changed"));
    let branch = [p, q][loser];
    let token = f.review_token(branch);
    f.ok(&["review", "accept", branch, "--review-token", &token]);
}

fn acceptance_fingerprint(branch: &str, token: Option<&str>) -> (Value, String) {
    // Explicit historical wire shape, independent of the current Cmd serializer.
    let mut command = json!({"Review":{"action":"accept","branch":branch,"reason":null}});
    if let Some(token) = token {
        command["Review"]["review_token"] = json!(token);
    }
    let hash = scrolls::model::digest(serde_json::to_vec(&json!({"command":command,"body":null,"actor":"alice","role":"Human","proposal":null,"source_commit":null})).unwrap());
    (command, hash)
}

#[test]
fn historical_tokenless_receipts_and_both_store_schemas() {
    for schema in [1, 2, 3] {
        let f = Fixture::new();
        let root = f.dir.path().join("store");
        if schema == 1 {
            let s = Store::open(&root, &dolt(), false).unwrap();
            s.transaction(&((put("metadata","schema",&1).unwrap()+"DROP TABLE import_sources; DROP TABLE import_files; DROP TABLE import_members;") + "DELETE FROM metadata WHERE id='store_id'; DELETE FROM registry WHERE id IN ('rel:related_work','rel:implemented_by','rel:produced_by');"), "schema 1 fixture", "test").unwrap();
        }
        if schema < 3 {
            f.ok(&["migrate"]);
        }
        let p = f.ok(&[
            "new",
            "entity",
            "--title",
            "Existing proposal",
            "--body",
            "",
        ]);
        let branch = p["branch"].as_str().unwrap();
        let token = f.review_token(branch);
        let result = f.ok(&[
            "--op-id",
            "historical",
            "review",
            "accept",
            branch,
            "--review-token",
            &token,
        ]);
        let (command, hash) = acceptance_fingerprint(branch, None);
        let mut historical = result;
        historical.as_object_mut().unwrap().remove("review_token");
        historical
            .as_object_mut()
            .unwrap()
            .remove("proposal_revision");
        {
            let s = Store::open(&root, &dolt(), false).unwrap();
            s.checkout("main").unwrap();
            s.transaction(&put("operations", "historical", &json!({"op_id":"historical","args_hash":hash,"actor":"alice","command":command,"result":historical,"committed_at":"2026-10-16T00:00:00Z"})).unwrap(), "historical receipt fixture", "test").unwrap();
        }
        let before = review_state(&f);
        assert_eq!(
            historical,
            f.ok(&["--op-id", "historical", "review", "accept", branch])
        );
        f.fail(
            &[
                "--op-id",
                "historical",
                "review",
                "accept",
                branch,
                "--review-token",
                &token,
            ],
            "different arguments",
        );
        assert_eq!(before, review_state(&f));
        let next = f.ok(&["new", "entity", "--title", "Pending", "--body", ""]);
        f.fail(
            &["review", "accept", next["branch"].as_str().unwrap()],
            "requires --review-token",
        );
        assert_eq!(f.ok(&["doctor"])["schema_version"], 4);
    }
}

#[test]
fn revision_bound_receipts_survive_review_recovery() {
    for advance_main in [false, true] {
        let f = Fixture::new();
        let p = f.ok(&["new", "entity", "--title", "Recovery", "--body", ""]);
        let branch = p["branch"].as_str().unwrap();
        let review = f.ok(&["review", branch]);
        let token = review["review_token"].as_str().unwrap();
        let root = f.dir.path().join("store");
        let staged;
        {
            let s = Store::open(&root, &dolt(), false).unwrap();
            let mut proposal = s
                .proposals()
                .unwrap()
                .into_iter()
                .find(|p| p.branch == branch)
                .unwrap();
            proposal.status = "accepted".into();
            staged = json!({"proposal":proposal,"op_id":"crashed-accept","validated_base":review["main_revision"],"proposal_revision":review["proposal_revision"],"review_token":token});
            let (command, hash) = acceptance_fingerprint(branch, Some(token));
            let temp = format!("review/{}", uuid::Uuid::new_v4().simple());
            s.run(&["checkout", "-b", &temp, "main"]).unwrap();
            s.journal_review(&temp, review["main_revision"].as_str().unwrap())
                .unwrap();
            s.run(&["merge", "--no-commit", "--no-ff", branch]).unwrap();
            let sql = put("metadata", branch, &proposal).unwrap() + &put("operations", "crashed-accept", &json!({"op_id":"crashed-accept","args_hash":hash,"actor":"alice","command":command,"result":staged})).unwrap();
            s.transaction(&sql, "interrupted acceptance", "alice")
                .unwrap();
            if advance_main {
                s.checkout("main").unwrap();
                s.run(&["merge", "--ff-only", &temp]).unwrap();
            }
            // Drop the process/store before journal cleanup, on either side of FF.
        }
        f.ok(&["doctor"]);
        assert!(!root.join("review-journal.json").exists());
        if advance_main {
            f.ok(&["new", "note", "--title", "Later main", "--body", ""]);
        } else {
            assert_eq!(
                f.ok(&["review", branch])["main_revision"],
                review["main_revision"]
            );
            let s = Store::open(&root, &dolt(), false).unwrap();
            s.checkout("main").unwrap();
            assert!(
                s.one::<Value>("operations", "crashed-accept")
                    .unwrap()
                    .is_none()
            );
        }
        let accepted = f.ok(&[
            "--op-id",
            "crashed-accept",
            "review",
            "accept",
            branch,
            "--review-token",
            token,
        ]);
        assert_eq!(accepted, staged);
    }
}

fn import_fixture(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("chapters")).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@localhost"],
    ] {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(
        root.join("chapters/one.md"),
        "---\nid: one\ntitle: One\ntags: [test]\n---\nCaptured [literal](sc-does-not-exist).\n",
    )
    .unwrap();
    std::fs::write(root.join("README.md"), "# Overview\n").unwrap();
    std::fs::write(root.join("run.sh"), "#!/bin/sh\necho source\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.join("run.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    std::fs::write(
        root.join(".gitattributes"),
        "chapters/one.md export-ignore\n",
    )
    .unwrap();
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["add", "."])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["commit", "-qm", "source"])
            .status()
            .unwrap()
            .success()
    );
}
fn plan_import(
    f: &Fixture,
    repo: &std::path::Path,
    name: &str,
    profile: Option<&std::path::Path>,
) -> PathBuf {
    let plan = f
        .dir
        .path()
        .join(format!("{name}-{}.json", uuid::Uuid::new_v4()));
    let mut args = vec![
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
    if let Some(path) = profile {
        args.extend(["--profile-file", path.to_str().unwrap()]);
    } else {
        args.extend(["--profile", "handbook"]);
    }
    f.ok(&args);
    plan
}
fn persistent_files(root: &std::path::Path) -> std::collections::BTreeMap<PathBuf, String> {
    fn walk(root: &std::path::Path, out: &mut std::collections::BTreeMap<PathBuf, String>) {
        for e in std::fs::read_dir(root).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out)
            } else {
                out.insert(p.clone(), scrolls::model::digest(std::fs::read(p).unwrap()));
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, &mut out);
    out
}
#[test]
fn profile_plan_readonly_apply_ownership_multiple_sources_and_export() {
    let f = Fixture::new();
    let repo = f.dir.path().join("repo");
    import_fixture(&repo);
    let root = f.dir.path().join("store");
    let before = persistent_files(&root);
    let plan = plan_import(&f, &repo, "book", None);
    assert_eq!(persistent_files(&root), before);
    let plan2 = plan_import(&f, &repo, "book", None);
    let read =
        |p: &PathBuf| -> Value { serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap() };
    assert_eq!(read(&plan), read(&plan2));
    f.ok(&[
        "import",
        "apply",
        plan.to_str().unwrap(),
        "--op-id",
        "book",
        "--dry-run",
    ]);
    assert_eq!(persistent_files(&root), before);
    let args = ["import", "apply", plan.to_str().unwrap(), "--op-id", "book"];
    let result = f.ok(&args);
    assert_eq!(result["coverage"]["documents"], 2);
    assert_eq!(f.ok(&args), result);
    let id = scrolls::source_import::record_id("book", "chapter/one");
    let view = f.ok(&["show", "book:chapter/one", "--provenance"]);
    assert_eq!(view["record"]["id"], id);
    assert_eq!(view["provenance"]["membership"]["path"], "chapters/one.md");
    assert!(f.ok(&["lint"])["errors"].as_array().unwrap().is_empty());
    f.fail(
        &["edit", &id, "--expect", "1", "--body", "changed"],
        "source_ownership",
    );
    f.fail(
        &["status", &id, "archived", "--expect", "1"],
        "source_ownership",
    );
    let note = f.ok(&[
        "new",
        "note",
        "--title",
        "Local interpretation",
        "--body",
        "",
    ]);
    let note = note["id"].as_str().unwrap();
    f.ok(&["link", &id, "related", note, "--expect", "1"]);
    assert_eq!(f.ok(&["show", &id])["record"]["revision"], 2);
    f.ok(&["unlink", &id, "related", note, "--expect", "2"]);
    let batch=f.batch_file(json!([{"op":"new","handle":"k","kind":"knowledge","title":"Interpretation"},{"op":"link","from":"book:chapter/one","rel":"supports","to":"$k","expect":3}]));
    let p = f.ok(&["batch", batch.to_str().unwrap()]);
    let b = p["branch"].as_str().unwrap();
    f.ok(&["review", "accept", b, "--review-token", &f.review_token(b)]);
    let plan_other = plan_import(&f, &repo, "other", None);
    f.ok(&[
        "import",
        "apply",
        plan_other.to_str().unwrap(),
        "--op-id",
        "other",
    ]);
    f.fail(&["show", "chapter:one"], "book:chapter/one");
    assert_eq!(
        f.ok(&["show", "other:chapter:one"])["record"]["kind"],
        "evidence"
    );
    let out = f.dir.path().join("export");
    f.ok(&["render", "--out", out.to_str().unwrap()]);
    assert_eq!(
        std::fs::read(out.join("sources/book/chapters/one.md")).unwrap(),
        std::fs::read(repo.join("chapters/one.md")).unwrap()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(out.join("sources/book/run.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(out.join("SCROLLS-MANIFEST.json")).unwrap()).unwrap();
    let normalized =
        std::fs::read_to_string(out.join(manifest["records"][&id].as_str().unwrap())).unwrap();
    assert!(normalized.contains("Captured [literal](sc-does-not-exist)."));
    assert!(normalized.contains("Original source"));
    f.fail(
        &[
            "render",
            "--format",
            "liskov",
            "--out",
            f.dir.path().join("bad-export").to_str().unwrap(),
        ],
        "generic sources",
    );
    f.ok(&args); // exact receipt still wins after main advances
    let changed = read(&plan);
    let corrupt = f.batch_file(changed);
    let mut v: Value = serde_json::from_slice(&std::fs::read(&corrupt).unwrap()).unwrap();
    v["source"]["profile_hash"] = json!("tampered");
    std::fs::write(&corrupt, serde_json::to_vec(&v).unwrap()).unwrap();
    f.fail(
        &[
            "import",
            "apply",
            corrupt.to_str().unwrap(),
            "--op-id",
            "bad",
        ],
        "stale_plan",
    );
}
#[test]
fn profile_proposal_retry_rejection_and_schema_pinning() {
    let f = Fixture::new();
    let repo = f.dir.path().join("repo");
    import_fixture(&repo);
    let profile = f.dir.path().join("profile.yaml");
    std::fs::write(
        &profile,
        scrolls::profile::HANDBOOK.replace("literal: evidence", "literal: entity"),
    )
    .unwrap();
    let plan = plan_import(&f, &repo, "protected", Some(&profile));
    std::fs::write(&profile, "edited after planning").unwrap();
    let args = [
        "import",
        "apply",
        plan.to_str().unwrap(),
        "--op-id",
        "first",
    ];
    let p = f.ok(&args);
    let branch = p["branch"].as_str().unwrap();
    assert!(branch.starts_with("proposal/"));
    f.fail(&["show", "protected:chapter/one"], "unknown record");
    assert_eq!(f.ok(&args)["proposal_status"], "pending");
    let review = f.ok(&["review", branch]);
    assert_eq!(review["imports"]["files"].as_array().unwrap().len(), 4);
    assert!(review["imports"]["files"][0].get("bytes").is_none());
    f.ok(&["review", "reject", branch, "--reason", "test"]);
    assert_eq!(f.ok(&args)["proposal_status"], "rejected");
    let p = f.ok(&[
        "import",
        "apply",
        plan.to_str().unwrap(),
        "--op-id",
        "second",
    ]);
    let b = p["branch"].as_str().unwrap();
    let schema = f.ok(&["schema", "show"]);
    let mut next = schema["schema"].clone();
    next["instructions"] = json!("changed interpretation");
    let file = f.batch_file(next);
    let change = f.ok(&[
        "schema",
        "apply",
        file.to_str().unwrap(),
        "--expect",
        schema["schema_revision"].as_str().unwrap(),
    ]);
    let c = change["branch"].as_str().unwrap();
    f.ok(&["review", "accept", c, "--review-token", &f.review_token(c)]);
    f.fail(
        &["review", "accept", b, "--review-token", &f.review_token(b)],
        "stale_plan",
    );
    f.ok(&["review", "reject", b, "--reason", "replan"]);
    std::fs::write(
        &profile,
        scrolls::profile::HANDBOOK.replace("literal: evidence", "literal: entity"),
    )
    .unwrap();
    let plan = plan_import(&f, &repo, "protected", Some(&profile));
    let p = f.ok(&[
        "import",
        "apply",
        plan.to_str().unwrap(),
        "--op-id",
        "third",
    ]);
    let b = p["branch"].as_str().unwrap();
    f.ok(&["review", "accept", b, "--review-token", &f.review_token(b)]);
    assert_eq!(
        f.ok(&["show", "protected:chapter/one"])["record"]["kind"],
        "entity"
    );
}
#[test]
fn profile_readonly_rejects_dirty_journal_wrong_actor_and_stale_main() {
    let f = Fixture::new();
    let repo = f.dir.path().join("repo");
    import_fixture(&repo);
    let plan = plan_import(&f, &repo, "book", None);
    f.fail(
        &[
            "--actor",
            "bob",
            "import",
            "apply",
            plan.to_str().unwrap(),
            "--op-id",
            "wrong",
        ],
        "actor",
    );
    f.ok(&["new", "note", "--title", "Advance", "--body", ""]);
    f.fail(
        &[
            "import",
            "apply",
            plan.to_str().unwrap(),
            "--op-id",
            "stale",
        ],
        "stale_plan",
    );
    let root = f.dir.path().join("store");
    std::fs::write(root.join("migration-journal.json"), "{}").unwrap();
    let args = [
        "import",
        "plan",
        "git",
        repo.to_str().unwrap(),
        "--root",
        ".",
        "--rev",
        "HEAD",
        "--source",
        "book",
        "--profile",
        "handbook",
        "--out",
        plan.to_str().unwrap(),
    ];
    let before = persistent_files(&root);
    f.fail(&args, "recovery_required");
    assert_eq!(before, persistent_files(&root));
    std::fs::remove_file(root.join("migration-journal.json")).unwrap();
    {
        let s = Store::open(&root, &dolt(), false).unwrap();
        s.sql("INSERT INTO metadata VALUES ('unexplained','1')")
            .unwrap();
    }
    let before = persistent_files(&root);
    f.fail(&args, "dirty");
    assert_eq!(before, persistent_files(&root));
}

#[test]
fn migration_journal_recovers_only_owned_temporary_state() {
    for advanced in [false, true] {
        let f = Fixture::new();
        let root = f.dir.path().join("store");
        let main;
        {
            let s = Store::open(&root, &dolt(), false).unwrap();
            s.checkout("main").unwrap();
            s.transaction("DROP TABLE import_sources; DROP TABLE import_files; DROP TABLE import_members; REPLACE INTO metadata VALUES ('schema','3');","format3 fixture","test").unwrap();
            main = s.head("main").unwrap();
            let branch = format!("migration/{}", uuid::Uuid::new_v4().simple());
            let mut j = json!({"format":1,"branch":branch,"base":main,"target":null,"expected":{}});
            std::fs::write(
                root.join("migration-journal.json"),
                serde_json::to_vec(&j).unwrap(),
            )
            .unwrap();
            s.run(&["checkout", "-b", &branch, "main"]).unwrap();
            s.sql("CREATE TABLE import_sources (id VARCHAR(128) PRIMARY KEY,payload LONGTEXT NOT NULL)").unwrap();
            if advanced {
                s.sql("CREATE TABLE import_files (id VARCHAR(128) PRIMARY KEY,payload LONGTEXT NOT NULL); CREATE TABLE import_members (id VARCHAR(128) PRIMARY KEY,payload LONGTEXT NOT NULL)").unwrap();
                let mut expected = serde_json::Map::new();
                s.sql("REPLACE INTO metadata VALUES ('schema','4')")
                    .unwrap();
                for t in [
                    "records",
                    "edges",
                    "source_files",
                    "metadata",
                    "operations",
                    "registry",
                    "import_sources",
                    "import_files",
                    "import_members",
                ] {
                    let rows: std::collections::BTreeMap<String, String> = s
                        .sql(&format!("SELECT id,payload FROM {t} ORDER BY id"))
                        .unwrap()
                        .iter()
                        .map(|r| {
                            (
                                r["id"].as_str().unwrap().into(),
                                r["payload"].as_str().unwrap().into(),
                            )
                        })
                        .collect();
                    expected.insert(
                        t.into(),
                        json!(scrolls::model::digest(serde_json::to_vec(&rows).unwrap())),
                    );
                }
                j["expected"] = json!(expected);
                s.transaction(
                    &put("metadata", "migration_owner", &j).unwrap(),
                    "migration fixture",
                    "test",
                )
                .unwrap();
                j["target"] = json!(s.head(&branch).unwrap());
                std::fs::write(
                    root.join("migration-journal.json"),
                    serde_json::to_vec(&j).unwrap(),
                )
                .unwrap();
                s.checkout("main").unwrap();
                s.run(&["merge", "--ff-only", &branch]).unwrap();
            }
        }
        let data = f.ok(&["doctor"]);
        assert_eq!(data["schema_version"], if advanced { 4 } else { 3 });
        assert!(!root.join("migration-journal.json").exists());
        let s = Store::open(&root, &dolt(), false).unwrap();
        if !advanced {
            assert_eq!(s.head("main").unwrap(), main);
        }
        assert!(
            !s.branches()
                .unwrap()
                .iter()
                .any(|b| b.starts_with("migration/"))
        );
    }
    let f = Fixture::new();
    let root = f.dir.path().join("store");
    let main;
    {
        let s = Store::open(&root, &dolt(), false).unwrap();
        main = s.head("main").unwrap();
        let branch = format!("migration/{}", uuid::Uuid::new_v4().simple());
        let j = json!({"format":1,"branch":branch,"base":main,"target":null});
        std::fs::write(
            root.join("migration-journal.json"),
            serde_json::to_vec(&j).unwrap(),
        )
        .unwrap();
        s.run(&["checkout", "-b", &branch, "main"]).unwrap();
        s.sql("REPLACE INTO metadata VALUES ('unexpected','true')")
            .unwrap();
    }
    f.fail(&["doctor"], "unexpected migration working set");
    assert!(root.join("migration-journal.json").exists());
}

#[test]
fn prepared_import_rolls_back_all_collections_and_receipt() {
    let f = Fixture::new();
    let repo = f.dir.path().join("repo");
    import_fixture(&repo);
    let plan = plan_import(&f, &repo, "book", None);
    let plan: scrolls::source_import::Plan =
        serde_json::from_slice(&std::fs::read(plan).unwrap()).unwrap();
    let s = Store::open(&f.dir.path().join("store"), &dolt(), false).unwrap();
    s.checkout("main").unwrap();
    let before = s.graph().unwrap();
    let head = s.head("main").unwrap();
    let c = scrolls::source_import::build(&before, &plan.source).unwrap();
    let sql = scrolls::storage::graph_changes(&before, &c.graph).unwrap()
        + &scrolls::source_import::persist(&c).unwrap()
        + &put(
            "operations",
            "failed-import",
            &json!({"result":"must not persist"}),
        )
        .unwrap()
        + "INSERT INTO missing_table VALUES (1);";
    assert!(
        s.transaction(&sql, "failed prepared import", "alice")
            .is_err()
    );
    s.clean().unwrap();
    assert_eq!(s.head("main").unwrap(), head);
    assert!(s.graph().unwrap().records.is_empty());
    for table in ["import_sources", "import_files", "import_members"] {
        assert!(s.rows::<Value>(table).unwrap().is_empty());
    }
    assert!(
        s.one::<Value>("operations", "failed-import")
            .unwrap()
            .is_none()
    );
}
#[test]
fn custom_research_import_cli_without_additional_fields() {
    let f = Fixture {
        dir: tempfile::tempdir().unwrap(),
    };
    f.ok(&[
        "init",
        "--schema",
        concat!(env!("CARGO_MANIFEST_DIR"), "/examples/research.yaml"),
    ]);
    let schema_before = f.ok(&["schema", "show"]);
    {
        let s = Store::open(&f.dir.path().join("store"), &dolt(), false).unwrap();
        s.transaction("DROP TABLE import_sources; DROP TABLE import_files; DROP TABLE import_members; REPLACE INTO metadata VALUES ('schema','3');","format3 custom fixture","test").unwrap();
    }
    f.ok(&["migrate"]);
    assert_eq!(f.ok(&["schema", "show"]), schema_before);
    let repo = f.dir.path().join("repo");
    import_fixture(&repo);
    let mut p: Value = serde_yaml::from_str(scrolls::profile::HANDBOOK).unwrap();
    for r in p["documents"].as_array_mut().unwrap() {
        r["record"]["kind"] = json!({"literal":"trial"});
        r["record"]["fields"] = json!({"count":{"literal":0}});
    }
    let profile = f.batch_file(p);
    let plan = plan_import(&f, &repo, "research", Some(&profile));
    f.ok(&[
        "import",
        "apply",
        plan.to_str().unwrap(),
        "--op-id",
        "research",
    ]);
    let r = f.ok(&["show", "research:chapter/one", "--provenance"]);
    assert_eq!(r["record"]["fields"], json!({"count":0,"replicated":false}));
    assert_eq!(
        r["provenance"]["membership"]["metadata"]["tags"],
        json!(["test"])
    );
}

#[test]
fn import_namespace_races_are_serialized() {
    let f = Fixture::new();
    let repo = f.dir.path().join("repo");
    import_fixture(&repo);
    let profile = f.dir.path().join("protected.yaml");
    std::fs::write(
        &profile,
        scrolls::profile::HANDBOOK.replace("literal: evidence", "literal: entity"),
    )
    .unwrap();
    let first = plan_import(&f, &repo, "raced", Some(&profile));
    std::fs::write(
        &profile,
        scrolls::profile::HANDBOOK
            .replace("literal: evidence", "literal: entity")
            .replace("Handbook overview", "Changed overview"),
    )
    .unwrap();
    let second = plan_import(&f, &repo, "raced", Some(&profile));
    let children: Vec<_> = [(&first, "first"), (&second, "second")]
        .into_iter()
        .map(|(plan, op)| {
            f.command(&["import", "apply", plan.to_str().unwrap(), "--op-id", op])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let outputs: Vec<_> = children
        .into_iter()
        .map(|c| c.wait_with_output().unwrap())
        .collect();
    assert_eq!(outputs.iter().filter(|o| o.status.success()).count(), 1);
    let failed = outputs.iter().find(|o| !o.status.success()).unwrap();
    assert!(String::from_utf8_lossy(&failed.stdout).contains("source_update_unsupported"));
    assert_eq!(f.ok(&["proposals"]).as_array().unwrap().len(), 1);
    assert!(
        f.ok(&["doctor"])["import_sources"]
            .as_object()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn historical_checkout_cannot_bypass_newer_main_writer_fence() {
    let f = Fixture::new();
    let root = f.dir.path().join("store");
    {
        let s = Store::open(&root, &dolt(), false).unwrap();
        s.checkout("main").unwrap();
        s.run(&["checkout", "-b", "historical-format"]).unwrap();
        s.checkout("main").unwrap();
        s.transaction(
            &put("metadata", "schema", &99).unwrap(),
            "future writer fixture",
            "test",
        )
        .unwrap();
        s.checkout("historical-format").unwrap();
    }
    f.fail(
        &["new", "note", "--title", "Forbidden", "--body", ""],
        "unsupported store format 99",
    );
}

#[test]
fn migration_recovery_preserves_foreign_rows_in_new_tables() {
    let f = Fixture::new();
    let root = f.dir.path().join("store");
    {
        let s = Store::open(&root, &dolt(), false).unwrap();
        s.checkout("main").unwrap();
        s.transaction("DROP TABLE import_sources; DROP TABLE import_files; DROP TABLE import_members; REPLACE INTO metadata VALUES ('schema','3');","old format fixture","test").unwrap();
        let base = s.head("main").unwrap();
        let branch = format!("migration/{}", uuid::Uuid::new_v4().simple());
        let j = json!({"format":1,"base":base,"branch":branch,"target":null});
        std::fs::write(
            root.join("migration-journal.json"),
            serde_json::to_vec(&j).unwrap(),
        )
        .unwrap();
        s.run(&["checkout", "-b", &branch, "main"]).unwrap();
        s.sql("CREATE TABLE import_sources (id VARCHAR(128) PRIMARY KEY,payload LONGTEXT NOT NULL); INSERT INTO import_sources VALUES ('foreign','{}');").unwrap();
    }
    f.fail(&["doctor"], "unexpected migration table data");
    assert!(root.join("migration-journal.json").exists());
}

#[test]
fn readonly_snapshot_checks_recovery_after_acquiring_the_lock() {
    let f = Fixture::new();
    let root = f.dir.path().join("store");
    let lock = std::fs::File::open(root.join("lock")).unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    std::fs::write(root.join("review-journal.json"), "{}").unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let reader_root = root.clone();
    let reader = std::thread::spawn(move || {
        let result = Store::open_readonly(&reader_root, &dolt())
            .map(drop)
            .map_err(|e| e.to_string());
        send.send(result).unwrap();
    });
    assert!(matches!(
        receive.recv_timeout(std::time::Duration::from_millis(100)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    // The writer completes recovery before releasing its lock; the reader sees
    // that completed state, rather than racing its temporary journal.
    std::fs::remove_file(root.join("review-journal.json")).unwrap();
    fs2::FileExt::unlock(&lock).unwrap();
    receive
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap()
        .unwrap();
    reader.join().unwrap();
}

#[test]
fn imported_file_key_collision_never_overwrites_another_tuple() {
    let f = Fixture::new();
    let repo = f.dir.path().join("repo");
    import_fixture(&repo);
    let plan = plan_import(&f, &repo, "book", None);
    let parsed: scrolls::source_import::Plan =
        serde_json::from_slice(&std::fs::read(&plan).unwrap()).unwrap();
    let root = f.dir.path().join("store");
    let key;
    let foreign = json!({"source_id":"other","file":{"path":"foreign"}});
    {
        let s = Store::open(&root, &dolt(), false).unwrap();
        s.checkout("main").unwrap();
        let g = s.graph().unwrap();
        let c = scrolls::source_import::build(&g, &parsed.source).unwrap();
        key = c.files[0].key();
        s.transaction(
            &put("import_files", &key, &foreign).unwrap(),
            "synthetic collision fixture",
            "test",
        )
        .unwrap();
    }
    let plan = plan_import(&f, &repo, "book", None);
    f.fail(
        &[
            "import",
            "apply",
            plan.to_str().unwrap(),
            "--op-id",
            "collision",
        ],
        "duplicate",
    );
    let s = Store::open(&root, &dolt(), false).unwrap();
    s.checkout("main").unwrap();
    assert_eq!(s.one::<Value>("import_files", &key).unwrap(), Some(foreign));
    assert!(s.graph().unwrap().records.is_empty());
    assert!(s.one::<Value>("operations", "collision").unwrap().is_none());
    s.clean().unwrap();
}
