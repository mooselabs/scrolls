use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::Command};
struct Fixture {
    dir: tempfile::TempDir,
    repo: PathBuf,
    store: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(repo.join(".beads")).unwrap();
        fs::write(
            repo.join(".beads/metadata.json"),
            r#"{"project_id":"00000000-0000-4000-8000-000000000001"}"#,
        )
        .unwrap();
        fs::write(repo.join("fake.json"),json!({"issues":{"bd-a":{"id":"bd-a","title":"A bead","status":"closed","updated_at":"2026-10-16T00:00:00Z","metadata":{"keep":123,"scrolls/another_store":{"keep":true}}}}}).to_string()).unwrap();
        let store = dir.path().join("scrolls");
        let f = Self { dir, repo, store };
        f.ok(&["init"]);
        f.ok(&[
            "beads",
            "bind",
            "work",
            "--directory",
            f.repo.to_str().unwrap(),
            "--bd",
            &format!("{}/tests/fixtures/fake-bd.py", env!("CARGO_MANIFEST_DIR")),
            "--timeout",
            "1",
        ]);
        f
    }
    fn call(&self, args: &[&str]) -> (bool, Value) {
        let o = Command::new(env!("CARGO_BIN_EXE_scrolls"))
            .args([
                "--json",
                "--actor",
                "alice",
                "--role",
                "human",
                "--store",
                self.store.to_str().unwrap(),
                "--dolt",
                &format!("{}/.tools/dolt", env!("CARGO_MANIFEST_DIR")),
            ])
            .args(args)
            .output()
            .unwrap();
        let v = serde_json::from_slice(&o.stdout)
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&o.stderr)));
        (o.status.success(), v)
    }
    fn ok(&self, args: &[&str]) -> Value {
        let (ok, v) = self.call(args);
        assert!(ok, "{args:?}: {v}");
        v["data"].clone()
    }
    fn fail(&self, args: &[&str], message: &str) -> Value {
        let (ok, v) = self.call(args);
        assert!(!ok, "{args:?}: {v}");
        assert!(v.to_string().contains(message), "{v}");
        v
    }
    fn state(&self) -> Value {
        serde_json::from_slice(&fs::read(self.repo.join("fake.json")).unwrap()).unwrap()
    }
    fn set(&self, k: &str, v: Value) {
        let mut s = self.state();
        s[k] = v;
        fs::write(self.repo.join("fake.json"), s.to_string()).unwrap();
    }
    fn local(&self) -> Value {
        serde_json::from_slice(&fs::read(self.store.join("beads.json")).unwrap()).unwrap()
    }
}
#[test]
fn explicit_refresh_publish_recovery_and_offline_navigation() {
    let f = Fixture::new();
    let doc = f.ok(&[
        "new",
        "evidence",
        "--title",
        "Readback",
        "--body",
        "Observed outcome",
    ]);
    let id = doc["id"].as_str().unwrap();
    f.ok(&["link", id, "produced_by", "work:bd-a", "--expect", "1"]);
    let before = fs::read(f.repo.join("calls.jsonl")).unwrap();
    assert_eq!(
        f.ok(&["show", id])["related_work"][0]["bead"]["resolution"],
        "unresolved"
    );
    f.ok(&["beads", "refs", "work:bd-a"]);
    f.ok(&["context", "--bead", "work:bd-a"]);
    assert_eq!(before, fs::read(f.repo.join("calls.jsonl")).unwrap());
    let local = f.local();
    f.ok(&["--dry-run", "beads", "refresh", "work:bd-a"]);
    assert_eq!(local, f.local());
    let refreshed = f.ok(&["beads", "refresh", "--all"]);
    assert_eq!(refreshed["results"][0]["title"], "A bead");
    let summary = f.ok(&["show", id])["related_work"][0]["bead"].clone();
    assert!(summary["observed_at"].is_string());
    let before = f.state();
    let local = f.local();
    f.ok(&["--dry-run", "beads", "publish", "--all"]);
    assert_eq!(before, f.state());
    assert_eq!(local, f.local());
    f.set("interrupt_after_write", json!(true));
    f.fail(
        &["--op-id", "pub", "beads", "publish", "work:bd-a"],
        "interruption",
    );
    f.set("interrupt_after_write", json!(false));
    let retry = f.ok(&["--op-id", "pub", "beads", "publish", "work:bd-a"]);
    assert_eq!(retry["results"][0]["action"], "unchanged");
    let meta = f.state()["issues"]["bd-a"]["metadata"].clone();
    assert_eq!(meta["keep"], 123);
    assert_eq!(meta["concurrent"]["preserved"], true);
    assert_eq!(meta["scrolls/another_store"]["keep"], true);
    let key = meta
        .as_object()
        .unwrap()
        .keys()
        .find(|k| k.starts_with("scrolls/") && k.as_str() != "scrolls/another_store")
        .unwrap();
    let uri = meta[key]["documents"][0]["uri"].as_str().unwrap();
    assert_eq!(f.ok(&["show", uri])["record"]["id"], id);
    assert_eq!(f.state()["issues"]["bd-a"]["status"], "closed");
    f.fail(&["beads", "refresh", "work:bd"], "exact Beads issue");
    f.set("offline", json!(true));
    f.fail(&["beads", "refresh", "--all"], "unavailable");
    let cached = f.ok(&["show", id])["related_work"][0]["bead"].clone();
    assert_eq!(cached["title"], "A bead");
    assert_eq!(cached["observed_at"], summary["observed_at"]);
    assert_eq!(cached["resolution"], "lookup_failed");
    f.set("offline", json!(false));
    f.set("malformed", json!(true));
    f.fail(&["beads", "refresh", "--all"], "invalid Beads JSON");
    f.set("malformed", json!(false));
    f.set("timeout", json!(true));
    f.fail(&["beads", "refresh", "--all"], "timed out");
    f.set("timeout", json!(false));
    f.ok(&["unlink", id, "produced_by", "work:bd-a", "--expect", "2"]);
    // Lose receipts: Beads discovery must still find and remove our stale key.
    let mut local = f.local();
    local["publications"] = json!({});
    local["operations"] = json!({});
    fs::write(f.store.join("beads.json"), local.to_string()).unwrap();
    let removed = f.ok(&["beads", "publish", "--all"]);
    assert_eq!(removed["results"][0]["action"], "remove");
    assert!(f.state()["issues"]["bd-a"]["metadata"].get(key).is_none());
    assert_eq!(f.state()["issues"]["bd-a"]["metadata"]["keep"], 123);
    let another = f.ok(&[
        "--op-id",
        "another-doc",
        "new",
        "evidence",
        "--title",
        "More",
        "--body",
        "facts",
    ]);
    let another = another["id"].as_str().unwrap();
    f.ok(&["link", another, "produced_by", "work:bd-a", "--expect", "1"]);
    f.ok(&[
        "link",
        another,
        "produced_by",
        "work:bd-zmissing",
        "--expect",
        "2",
    ]);
    f.fail(
        &["--op-id", "another-doc", "beads", "publish", "--all"],
        "different arguments",
    );
    let mixed = f.fail(
        &["--op-id", "mixed", "beads", "publish", "--all"],
        "not found",
    );
    assert_eq!(mixed["data"]["failed"], 1);
    assert!(
        mixed["data"]["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["verified"] == true)
    );
    f.fail(
        &[
            "--op-id",
            "mixed",
            "new",
            "note",
            "--title",
            "Collision",
            "--body",
            "",
        ],
        "different arguments",
    );
    f.ok(&[
        "unlink",
        another,
        "produced_by",
        "work:bd-zmissing",
        "--expect",
        "3",
    ]);
    assert_eq!(f.ok(&["beads", "publish", "--all"])["failed"], 0);
    fs::write(
        f.repo.join(".beads/metadata.json"),
        r#"{"project_id":"00000000-0000-4000-8000-000000000002"}"#,
    )
    .unwrap();
    f.fail(&["beads", "refresh", "work:bd-a"], "identity mismatch");
    f.fail(
        &[
            "beads",
            "bind",
            "work",
            "--directory",
            f.repo.to_str().unwrap(),
            "--bd",
            &format!("{}/tests/fixtures/fake-bd.py", env!("CARGO_MANIFEST_DIR")),
        ],
        "cannot rebind",
    );
    assert!(f.dir.path().exists());
}
