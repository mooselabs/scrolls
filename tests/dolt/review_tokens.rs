use super::{Fixture, Stdio, Store, Value, dolt, json, put};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use support::{acceptance_fingerprint, legacy_token};

#[path = "../support/review_tokens.rs"]
mod support;

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
    let bytes = URL_SAFE_NO_PAD
        .decode(token.strip_prefix("v2.").unwrap())
        .unwrap();
    let payload: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(payload["proposal"], branch);
    assert_eq!(payload["main_revision"], review["main_revision"]);
    assert_eq!(payload["proposal_revision"], review["proposal_revision"]);
    let human = scrolls::presentation::human(&json!({"data":review}));
    assert!(human.find("Graph changes:").unwrap() < human.find("Review token:").unwrap());
    assert!(human.contains(&format!("--review-token {token}")));
    assert!(human.contains(review["main_revision"].as_str().unwrap()));
    assert!(human.contains(review["proposal_revision"].as_str().unwrap()));
    let old_token = legacy_token(branch, &review);
    let stale_payload = std::str::from_utf8(&bytes).unwrap().replacen(
        &format!(r#""proposal_revision":{}"#, payload["proposal_revision"]),
        r#""proposal_revision":"short""#,
        1,
    );
    let stale = format!("v2.{}", URL_SAFE_NO_PAD.encode(stale_payload));
    let refresh = format!(
        "run scrolls review {branch} again for a v2 token: unsupported review token version v1"
    );
    let before = review_state(&f);
    for dry in [false, true] {
        let prefix = if dry { vec!["--dry-run"] } else { vec![] };
        let mut args = prefix.clone();
        args.extend(["review", "accept", branch]);
        f.fail(&args, "requires --review-token");
        for (bad, error) in [
            ("garbage", "malformed"),
            ("v2.%%%", "malformed"),
            ("v2.e30", "malformed"),
            ("v3.e30", "unsupported review token version v3"),
            (&stale, "proposal revision changed from short"),
            (&old_token, &refresh),
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
        assert_eq!(before, review_state(&f));
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

#[test]
fn completed_v1_review_receipts_replay_unchanged() {
    let f = Fixture::new();
    let proposal = f.ok(&[
        "new",
        "entity",
        "--title",
        "Previously reviewed",
        "--body",
        "",
    ]);
    let branch = proposal["branch"].as_str().unwrap();
    let review = f.ok(&["review", branch]);
    let token = review["review_token"].as_str().unwrap();
    let old_token = legacy_token(branch, &review);
    let op = "historical-v1";
    let mut result = f.ok(&[
        "--op-id",
        op,
        "review",
        "accept",
        branch,
        "--review-token",
        token,
    ]);
    result["review_token"] = json!(old_token);
    let (command, args_hash) = acceptance_fingerprint(branch, Some(&old_token));
    {
        let s = Store::open(&f.dir.path().join("store"), &dolt(), false).unwrap();
        s.checkout("main").unwrap();
        let receipt = json!({
            "op_id": op, "args_hash": args_hash, "actor": "alice", "command": command,
            "result": result, "committed_at": "2026-10-16T00:00:00Z",
        });
        s.transaction(
            &put("operations", op, &receipt).unwrap(),
            "historical v1 receipt fixture",
            "test",
        )
        .unwrap();
    }
    let fresh = f.review_token(branch);
    let before = review_state(&f);
    let replay = f.ok(&[
        "--op-id",
        op,
        "review",
        "accept",
        branch,
        "--review-token",
        &old_token,
    ]);
    assert_eq!(
        serde_json::to_vec(&replay).unwrap(),
        serde_json::to_vec(&result).unwrap()
    );
    f.fail(
        &[
            "--op-id",
            op,
            "review",
            "accept",
            branch,
            "--review-token",
            &fresh,
        ],
        "different arguments",
    );
    assert_eq!(before, review_state(&f));
}

#[test]
fn standalone_controller_requires_a_verifier_and_human_review_stays_cooperative() {
    let f = Fixture::new();
    let proposal = f.ok(&["new", "entity", "--title", "Review", "--body", "draft"]);
    let branch = proposal["branch"].as_str().unwrap();
    let token = f.review_token(branch);
    f.fail(
        &[
            "--role",
            "controller",
            "review",
            "accept",
            branch,
            "--review-token",
            &token,
        ],
        "controller_verifier_required: controller role requires a host credential verifier",
    );
    let accepted = f.ok(&[
        "--op-id",
        "standalone-accept",
        "review",
        "accept",
        branch,
        "--review-token",
        &token,
    ]);
    assert_eq!(accepted["proposal"]["status"], "accepted");
    let store = Store::open_readonly(&f.dir.path().join("store"), &dolt()).unwrap();
    let receipt = store
        .one::<Value>("operations", "standalone-accept")
        .unwrap()
        .unwrap();
    assert_eq!(receipt["role"], "Human");
}
