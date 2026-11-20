use super::Fixture;
use serde_json::{Value, json};

fn record(f: &Fixture) -> Value {
    let (ok, output) = f.call(&[
        "new",
        "note",
        "--title",
        "Deployment notes",
        "--body",
        "initial",
    ]);
    assert!(ok, "{output}");
    output
}

fn edit(f: &Fixture, id: &str, expect: &str, body: &str) -> Value {
    let (ok, output) = f.call(&["edit", id, "--expect", expect, "--body", body]);
    assert!(ok, "{output}");
    output
}

fn head(f: &Fixture) -> Value {
    let (ok, output) = f.call(&["list"]);
    assert!(ok, "{output}");
    output["revision"].clone()
}

#[test]
fn labelled_show_and_consumption_use_the_historical_snapshot() {
    let f = Fixture::new();
    let record = record(&f);
    let id = record["data"]["id"].as_str().unwrap();
    let first = edit(&f, id, "1", "first edit");
    let first_revision = first["revision"].as_str().unwrap();
    f.ok(&[
        "label",
        "move",
        id,
        "stable",
        "--to",
        first_revision,
        "--expect-absent",
    ]);
    let latest = edit(&f, id, "2", "second edit");
    assert_ne!(latest["revision"], first_revision);

    let (ok, output) = f.call(&[
        "--session",
        "stable-read",
        "show",
        "Deployment notes",
        "--label",
        "stable",
    ]);
    assert!(ok, "{output}");
    assert_eq!(output["revision"], first_revision);
    assert_eq!(output["data"]["label"], "stable");
    assert_eq!(output["data"]["resolved_revision"], first_revision);
    assert_eq!(output["data"]["record"]["id"], id);
    assert_eq!(output["data"]["record"]["body"], "first edit");
    assert_eq!(output["data"]["record"]["revision"], 2);
    assert!(
        scrolls::presentation::human(&output)
            .contains(&format!("Label stable resolved to {first_revision}"))
    );
    assert_eq!(f.ok(&["show", id])["record"]["body"], "second edit");

    let audit = f.ok(&["consumed", "--for-session", "stable-read"]);
    assert_eq!(audit.as_array().unwrap().len(), 1);
    assert_eq!(audit[0]["revision"], first_revision);
    assert_eq!(audit[0]["record_revisions"], json!({id:2}));
    assert_eq!(audit[0]["ids"], json!([id]));
    assert_eq!(head(&f), latest["revision"]);
}

#[test]
fn label_moves_are_atomic_retryable_and_guarded_by_the_previous_target() {
    let f = Fixture::new();
    let record = record(&f);
    let id = record["data"]["id"].as_str().unwrap();
    let first = record["revision"].as_str().unwrap();
    let created = f.ok(&[
        "label",
        "move",
        id,
        "stable",
        "--to",
        first,
        "--expect-absent",
    ]);
    assert!(created["previous_target"].is_null());
    f.ok(&[
        "label",
        "move",
        id,
        "initial",
        "--to",
        first,
        "--expect-absent",
    ]);
    let edit = edit(&f, id, "1", "updated");
    let second = edit["revision"].as_str().unwrap();
    let args = [
        "--op-id",
        "move-stable",
        "label",
        "move",
        id,
        "stable",
        "--to",
        second,
        "--expect",
        first,
    ];
    let mut dry_run = vec!["--dry-run"];
    dry_run.extend(args);
    let preview = f.ok(&dry_run);
    assert_eq!(preview["dry_run"], true);
    assert_eq!(head(&f), second);
    assert_eq!(
        f.ok(&["label", "list", id])["labels"],
        json!({"initial":first,"stable":first})
    );

    let receipt = f.ok(&args);
    assert_eq!(receipt, preview["result"]);
    assert_eq!(
        receipt,
        json!({
            "op_id":"move-stable","actor":"alice","role":"human","id":id,
            "label":"stable","previous_target":first,"new_target":second
        })
    );
    let committed = head(&f);
    assert_ne!(committed, second);
    {
        let store = super::Store::open(&f.dir.path().join("store"), &super::dolt(), false).unwrap();
        assert_eq!(
            store
                .merge_base(second, committed.as_str().unwrap())
                .unwrap(),
            second
        );
        let history = store.sql("SELECT commit_hash FROM dolt_log").unwrap();
        assert_eq!(history[0]["commit_hash"], committed);
        assert_eq!(history[1]["commit_hash"], second);
        assert_eq!(
            store
                .one::<Value>("operations", "move-stable")
                .unwrap()
                .unwrap()["result"],
            receipt
        );
    }
    assert_eq!(f.ok(&args), receipt);
    f.fail(
        &[
            "label", "move", id, "stable", "--to", first, "--expect", first,
        ],
        "stale_label",
    );
    f.fail(
        &[
            "label",
            "move",
            id,
            "stable",
            "--to",
            first,
            "--expect-absent",
        ],
        "stale_label",
    );
    assert_eq!(head(&f), committed);
    assert_eq!(
        f.ok(&["label", "list", id])["labels"],
        json!({"initial":first,"stable":second})
    );
    assert_eq!(f.ok(&["show", id])["record"]["revision"], 2);
}

#[test]
fn label_moves_refuse_agents_proposals_and_invalid_targets() {
    let f = Fixture::new();
    let empty_main = head(&f);
    let record = record(&f);
    let id = record["data"]["id"].as_str().unwrap();
    let main = record["revision"].as_str().unwrap();
    let schema = f.ok(&["schema", "show"]);
    let mut config = schema["schema"].clone();
    config["pins"] = json!([id]);
    let file = f.batch_file(config);
    let (ok, proposal) = f.call(&[
        "schema",
        "apply",
        file.to_str().unwrap(),
        "--expect",
        schema["schema_revision"].as_str().unwrap(),
    ]);
    assert!(ok, "{proposal}");
    let branch = proposal["branch"].as_str().unwrap();
    assert!(branch.starts_with("proposal/"), "{proposal}");
    let unaccepted = proposal["revision"].as_str().unwrap();
    for (target, message) in [
        (unaccepted, "exact revision on main history"),
        ("main", "exact revision on main history"),
        (empty_main.as_str().unwrap(), "does not exist at revision"),
    ] {
        f.fail(
            &[
                "label",
                "move",
                id,
                "stable",
                "--to",
                target,
                "--expect-absent",
            ],
            message,
        );
    }
    for (flag, value, message) in [
        ("--role", "agent", "label moves require --role human"),
        ("--proposal", branch, "label moves operate on main"),
    ] {
        f.fail(
            &[
                flag,
                value,
                "label",
                "move",
                id,
                "stable",
                "--to",
                main,
                "--expect-absent",
            ],
            message,
        );
    }
    f.fail(
        &[
            "label",
            "move",
            id,
            "Bad:Name",
            "--to",
            main,
            "--expect-absent",
        ],
        "invalid label",
    );
    assert_eq!(head(&f), main);
    assert_eq!(f.ok(&["review", branch])["proposal_revision"], unaccepted);
    assert_eq!(f.ok(&["label", "list", id])["labels"], json!({}));
}

#[test]
fn a_store_without_label_rows_reads_without_migration() {
    let f = Fixture::new();
    let record = record(&f);
    let id = record["data"]["id"].as_str().unwrap();
    let main = record["revision"].as_str().unwrap();
    assert_eq!(f.ok(&["label", "list", id]), json!({"id":id,"labels":{}}));
    let (ok, missing) = f.call(&["show", id, "--label", "missing"]);
    assert!(!ok);
    let message = missing["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(id) && message.contains("missing"),
        "{missing}"
    );
    assert_eq!(f.ok(&["doctor"])["schema_version"], 4);
    assert_eq!(head(&f), main);
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
