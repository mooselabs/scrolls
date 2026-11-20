use super::Server;
use mysql::prelude::Queryable;
use scrolls::storage::literal;
use serde_json::json;
use std::fs;

#[test]
fn shared_reads_consume_the_label_target_and_shared_moves_are_refused() {
    let s = Server::empty();
    s.init(None);
    let old = s.call(
        "a",
        &[
            "--op-id",
            "record",
            "new",
            "note",
            "--title",
            "Notes",
            "--body",
            "published",
        ],
        true,
    );
    let id = old["data"]["id"].as_str().unwrap();
    let target = old["revision"].as_str().unwrap();
    let latest = s.call(
        "a",
        &[
            "--op-id", "edit", "edit", id, "--expect", "1", "--body", "changed",
        ],
        true,
    );
    let before = latest["revision"].as_str().unwrap();
    let move_args = [
        "label",
        "move",
        id,
        "stable",
        "--to",
        target,
        "--expect-absent",
    ];
    assert!(
        s.call("b", &move_args, false)["error"]["message"]
            .as_str()
            .unwrap()
            .contains("shared_unsupported")
    );
    assert_eq!(
        s.call("b", &["label", "list", id], true)["revision"],
        before
    );

    // Seed the local writer's row format to qualify the shared read path.
    let key = format!("label:{}:stable", scrolls::model::digest(id));
    let row = scrolls::storage::put("registry", &key, &target).unwrap();
    s.raw("main").query_drop(format!(
        "{row} CALL DOLT_ADD('.'); CALL DOLT_COMMIT('-m','label fixture','--author','Test <test@localhost>')"
    )).unwrap();

    let listed = s.call("b", &["label", "list", id], true);
    assert_eq!(listed["data"]["labels"], json!({"stable":target}));
    let read = s.call("b", &["show", id, "--label", "stable"], true);
    assert_eq!(read["revision"], target);
    assert_eq!(read["data"]["resolved_revision"], target);
    assert_eq!(read["data"]["record"]["body"], "published");
    assert_eq!(read["data"]["record"]["revision"], 1);
    assert_eq!(read["consumed"]["revision"], target);
    assert_eq!(read["consumed"]["records"], json!({id:1}));
    assert_eq!(
        read["consumed"]["store_id"],
        s.call("b", &["show", id], true)["consumed"]["store_id"]
    );
    assert!(!s.dir.path().join("b/consumed.jsonl").exists());
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
