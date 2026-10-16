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
    let entity = f.ok(&["new", "entity", "--title", "Shared", "--body", "entity"]);
    let entity_id = entity["id"].as_str().unwrap();
    f.ok(&["review", "accept", entity["branch"].as_str().unwrap()]);
    let baseline = f.ok(&["show", entity_id])["record"].clone();
    let mut children = vec![];
    for actor in ["agent-one", "agent-two"] {
        children.push(
            f.command(&[
                "--actor", actor, "--role", "agent", "new", "evidence", "--title", actor, "--body",
                "facts", "--about", entity_id,
            ])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
        );
    }
    let mut evidence = vec![];
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(result["data"]["branch"], "main");
        assert_eq!(result["data"]["records"].as_object().unwrap().len(), 1);
        evidence.push(result["data"]["id"].as_str().unwrap().to_string());
    }
    let mut branches = vec![];
    for (n, e) in evidence.iter().enumerate() {
        let result = f.ok(&[
            "--role",
            "agent",
            "new",
            "knowledge",
            "--title",
            &format!("independent {n}"),
            "--body",
            "supported",
            "--supports",
            e,
            "--about",
            entity_id,
        ]);
        assert!(result["records"].get(entity_id).is_none());
        branches.push(result["branch"].as_str().unwrap().to_string());
    }
    for b in &branches {
        f.ok(&["review", "accept", b]);
    }
    assert_eq!(f.ok(&["show", entity_id])["record"], baseline);
    let q = f.ok(&["--role", "agent", "ask", "Direct?", "--about", entity_id]);
    assert_eq!(q["branch"], "main");
    assert_eq!(q["records"].as_object().unwrap().len(), 1);
    let e = f.ok(&[
        "--role",
        "agent",
        "new",
        "evidence",
        "--title",
        "Body ref",
        "--body",
        &format!("[entity]({entity_id}#anchor)"),
    ]);
    let id = e["id"].as_str().unwrap();
    assert_eq!(e["branch"], "main");
    assert_eq!(e["records"].as_object().unwrap().len(), 1);
    let unlink = [
        "--op-id",
        "unlink-reference",
        "unlink",
        id,
        "related",
        entity_id,
        "--expect",
        "1",
        "--expect-to",
        "1",
    ];
    let removed = f.ok(&unlink);
    assert_eq!(removed, f.ok(&unlink));
    assert_eq!(removed["records"], json!({id:2}));
    f.fail(
        &["link", id, "related", entity_id, "--expect", "1"],
        "stale",
    );
    f.fail(
        &[
            "link",
            id,
            "related",
            entity_id,
            "--expect",
            "2",
            "--expect-to",
            "999",
        ],
        "stale",
    );
    let link = [
        "--op-id",
        "link-reference",
        "link",
        id,
        "related",
        entity_id,
        "--expect",
        "2",
    ];
    let added = f.ok(&link);
    assert_eq!(added, f.ok(&link));
    assert_eq!(added["records"], json!({id:3}));
    assert_eq!(f.ok(&["show", entity_id])["record"], baseline);
    let protected = f.ok(&[
        "--role",
        "agent",
        "edit",
        entity_id,
        "--expect",
        "1",
        "--body",
        "protected",
    ]);
    assert_ne!(protected["branch"], "main");
    let protected_link = f.ok(&["link", entity_id, "related", id, "--expect", "1"]);
    assert_ne!(protected_link["branch"], "main");
    f.fail(
        &["link", &evidence[0], "supports", entity_id, "--expect", "2"],
        "expect-to",
    );
    // A frozen target can be referenced, but never changed or made an assertion owner.
    {
        let s = Store::open(&f.dir.path().join("store"), &dolt(), false).unwrap();
        s.checkout("main").unwrap();
        let mut frozen = s.graph().unwrap().records[entity_id].clone();
        frozen.frozen = true;
        s.transaction(
            &put("records", entity_id, &frozen).unwrap(),
            "fixture frozen history",
            "test",
        )
        .unwrap();
    }
    let frozen = f.ok(&["show", entity_id])["record"].clone();
    f.ok(&[
        "unlink",
        id,
        "related",
        entity_id,
        "--expect",
        "3",
        "--expect-to",
        "1",
    ]);
    f.ok(&[
        "link",
        id,
        "related",
        entity_id,
        "--expect",
        "4",
        "--expect-to",
        "1",
    ]);
    f.ok(&["ask", "History?", "--about", entity_id]);
    f.fail(
        &["link", entity_id, "related", id, "--expect", "1"],
        "immutable",
    );
    f.fail(
        &["edit", entity_id, "--expect", "1", "--body", "illegal"],
        "immutable",
    );
    assert_eq!(f.ok(&["show", entity_id])["record"], frozen);
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
    f.ok(&["review", "accept", branch]);
    let packet = f.ok(&["prime", "--for", "src/main.rs"]);
    assert_eq!(packet["items"].as_array().unwrap().len(), 1);
    assert!(packet["over_budget_bytes"].as_u64().unwrap() > 0);
    assert_eq!(
        packet["used_bytes"],
        serde_json::to_vec(&packet["items"]).unwrap().len()
    );
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
    f.fail(&["review", "accept", bad_branch], "glob");
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
    f.fail(&["prime"], "glob");
    f.fail(&["doctrine"], "glob");
    assert!(
        f.call(&["lint"]).1["data"]["errors"]
            .to_string()
            .contains("glob")
    );
    f.fail(&["status", id, "active", "--expect", "2"], "glob");
}

#[test]
fn prime_delivers_answer_attribution_and_audits_both_ids() {
    let f = Fixture::new();
    let q = f.ok(&["ask", "Accepted answer?"]);
    let q = q["id"].as_str().unwrap();
    let a = f.ok(&[
        "--actor",
        "bob",
        "answer",
        q,
        "accepted text",
        "--expect",
        "1",
    ]);
    let a = a["id"].as_str().unwrap();
    let other = f.ok(&["answer", q, "alternative", "--expect", "2"]);
    let other = other["id"].as_str().unwrap();
    f.ok(&["accept", a, "--expect", "1", "--expect-question", "3"]);
    let accepted = f.ok(&["show", a])["record"].clone();
    let p = f.ok(&["--session", "paired", "prime"]);
    assert_eq!(p["items"].as_array().unwrap().len(), 2);
    assert_eq!(p["items"][0]["accepted_answer_id"], a);
    assert_eq!(p["items"][1]["actor"], accepted["actor"]);
    assert_eq!(p["items"][1]["revision"], accepted["revision"]);
    assert!(!p["items"].to_string().contains(other));
    let audit = f.ok(&["consumed", "--for-session", "paired"]);
    assert!(audit.to_string().contains(q));
    assert!(audit.to_string().contains(a));
    let size = serde_json::to_vec(&p["items"]).unwrap().len();
    assert_eq!(
        f.ok(&["prime", "--budget", &(size - 1).to_string()])["omitted"],
        json!([q, a])
    );
}
