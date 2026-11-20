use super::{Fixture, Store, dolt, json, put};
use scrolls::model::DEFAULT_TENANT;

#[path = "../support/pinned_scrolls.rs"]
mod pinned_scrolls;

#[test]
fn knowledge_store_accept_checks_graph_even_when_caller_approves() {
    use scrolls::{
        model::{Proposal, Record, Role},
        store::{KnowledgeStore, OperationContext, ReviewRequest},
    };
    let f = Fixture::new();
    let s = Store::open(&f.dir.path().join("store"), &dolt(), false).unwrap();
    let store: &dyn KnowledgeStore = &s;
    let base = store.head("main").unwrap();
    let main = s.graph().unwrap();
    let proposal = Proposal {
        branch: "proposal/unchecked".into(),
        base: base.clone(),
        actor: "alice".into(),
        status: "pending".into(),
        reason: None,
    };
    let record = Record::new("knowledge", "unchecked", "body", "alice").unwrap();
    s.run(&["checkout", "-b", &proposal.branch]).unwrap();
    // Simulate an external Dolt writer, outside the engine's write contract.
    s.transaction(
        &(put("records", &record.id, &record).unwrap()
            + &put("metadata", &proposal.branch, &proposal).unwrap()),
        "external proposal",
        "alice",
    )
    .unwrap();
    let proposed = store.head(&proposal.branch).unwrap();
    s.checkout("main").unwrap();
    let mut context = OperationContext {
        tenant: "other".parse().unwrap(),
        op_id: "accept-unchecked".into(),
        args_hash: "unchecked-hash".into(),
        explicit_op_id: true,
        actor: Some("alice".into()),
        role: Role::Human,
        dry_run: false,
        command: json!({"review":{"action":"accept"}}),
        review: Some(ReviewRequest {
            action: "accept".into(),
            token: Some("caller approves".into()),
            reason: None,
        }),
        recovery: None,
    };
    let branches = store.branches().unwrap();
    let refused = store
        .accept(&context, &main, &proposal, (&base, &proposed), &mut || {
            panic!("wrong tenant reached validation")
        })
        .unwrap_err();
    assert_eq!(
        refused.to_string(),
        "tenant_mismatch: operation tenant other differs from store tenant default"
    );
    assert_eq!(store.branches().unwrap(), branches);
    assert_eq!(store.head("main").unwrap(), base);
    assert_eq!(store.head(&proposal.branch).unwrap(), proposed);
    assert!(!s.root.join("review-journal.json").exists());

    context.tenant = DEFAULT_TENANT;
    let error = store
        .accept(&context, &main, &proposal, (&base, &proposed), &mut || {
            Ok(())
        })
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains(&format!("{}:missing-support", record.id)),
        "{error:#}"
    );
    assert_eq!(store.head("main").unwrap(), base);
    assert_eq!(store.head(&proposal.branch).unwrap(), proposed);
    assert!(
        store
            .receipt("accept-unchecked", "unchecked-hash")
            .unwrap()
            .is_none()
    );
}

#[test]
fn pre_tenant_dolt_store_reports_default_without_changing_any_rows() {
    use std::{collections::BTreeMap, process::Command};

    let legacy = pinned_scrolls::build_0_3_0();
    let f = Fixture {
        dir: tempfile::tempdir().unwrap(),
    };
    let root = f.dir.path().join("store");
    let binary = dolt();
    for args in [
        &["init"][..],
        &[
            "new",
            "note",
            "--title",
            "Legacy note",
            "--body",
            "Old writer",
        ],
    ] {
        let output = Command::new(legacy.path().join("debug/scrolls"))
            .args(["--json", "--actor", "alice", "--role", "human", "--store"])
            .arg(&root)
            .arg("--dolt")
            .arg(&binary)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Fresh 0.3.0 stores have an empty registry. Seed a row written by
    // that version's migration so the comparison also exercises its payload.
    let registry_row = put(
        "registry",
        "rel:related_work",
        &json!({"relation":"related_work","requires_why":false}),
    )
    .unwrap();
    let seed = Command::new(&binary)
        .current_dir(root.join("dolt"))
        .env("DOLT_ROOT_PATH", root.join("dolt-home"))
        .env("DOLT_DISABLE_EVENT_FLUSH", "1")
        .args([
            "sql",
            "-q",
            &format!(
                "{registry_row} CALL DOLT_ADD('registry'); CALL DOLT_COMMIT('-m','legacy registry','--author','Fixture <fixture@localhost>');"
            ),
        ])
        .output()
        .unwrap();
    assert!(
        seed.status.success(),
        "{}",
        String::from_utf8_lossy(&seed.stderr)
    );

    let snapshot = || {
        let store = Store::open_readonly(&root, &binary).unwrap();
        let rows = store
            .sql("SELECT TABLE_NAME AS scrolls_table FROM information_schema.tables WHERE table_schema = DATABASE() AND table_type = 'BASE TABLE'")
            .unwrap()
            .into_iter()
            .map(|row| {
                let table = row["scrolls_table"].as_str().expect("table inventory row");
                let mut rows = store.read_rows(table, None).unwrap();
                rows.sort_unstable();
                (table.to_owned(), rows)
            })
            .collect::<BTreeMap<_, _>>();
        (store.head("main").unwrap(), rows)
    };
    let before = snapshot();
    assert_eq!(before.1.len(), 9);
    assert!(!before.1["metadata"].is_empty());
    assert!(!before.1["registry"].is_empty());
    assert_eq!(f.ok(&["doctor"])["tenant"], DEFAULT_TENANT.as_str());
    assert_eq!(
        f.ok(&["show", "Legacy note"])["record"]["body"],
        "Old writer"
    );
    assert_eq!(snapshot(), before);
}
