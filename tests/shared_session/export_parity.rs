use super::*;

#[test]
fn shared_editable_markdown_fidelity_edit_review_render_and_source_free_replay() {
    let s = Server::empty();
    s.init(None);
    let repo = editable_markdown_source(&s);
    let base = s.main_head();
    let plan = editable_plan(&s, &repo, "markdown");
    assert_eq!(s.main_head(), base);
    let args = [
        "--op-id",
        "markdown",
        "import",
        "apply",
        plan.to_str().unwrap(),
    ];
    let dry = s.call(
        "reader",
        &[
            "--dry-run",
            "--op-id",
            "markdown",
            "import",
            "apply",
            plan.to_str().unwrap(),
        ],
        true,
    );
    assert_eq!(dry["data"]["result"]["import"]["files"], 6);
    assert_eq!(s.main_head(), base);
    let imported = s.call("writer", &args, true);
    assert_eq!(imported["data"]["import"]["files"], 6);
    assert_eq!(
        s.call("reader", &["doctor"], true)["data"]["markdown_health"]["files"],
        6
    );
    let original: Vec<String> = s
        .raw("main")
        .query("SELECT payload FROM source_files ORDER BY id")
        .unwrap();
    assert_eq!(original.len(), 6);
    for payload in original {
        let file: scrolls::model::SourceFile = serde_json::from_str(&payload).unwrap();
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&file.bytes)
            .unwrap();
        assert_eq!(bytes, fs::read(repo.join("docs").join(&file.path)).unwrap());
        if file.path == "raw/exec.md" {
            assert_eq!(file.mode, 0o100755);
        }
    }
    let knowledge = s.call("reader", &["show", "knowledge/one.md"], true);
    let id = knowledge["data"]["record"]["id"].as_str().unwrap();
    let edit = s.call(
        "author",
        &[
            "--op-id",
            "edit-imported",
            "--role",
            "agent",
            "edit",
            id,
            "--expect",
            "1",
            "--body",
            "Reviewed replacement",
        ],
        true,
    );
    assert!(
        edit["data"]["branch"]
            .as_str()
            .unwrap()
            .starts_with("proposal/")
    );
    let branch = edit["data"]["branch"].as_str().unwrap();
    assert_ne!(
        s.call("reader", &["show", id], true)["data"]["record"]["body"],
        "Reviewed replacement"
    );
    let review = s.call("reviewer", &["review", branch], true);
    s.call(
        "reviewer",
        &[
            "--op-id",
            "accept-imported",
            "review",
            "accept",
            branch,
            "--review-token",
            review["data"]["review_token"].as_str().unwrap(),
        ],
        true,
    );
    assert_eq!(
        s.call("reader", &["show", id], true)["data"]["record"]["revision"],
        2
    );
    s.call(
        "stale",
        &[
            "--op-id", "stale", "edit", id, "--expect", "1", "--body", "stale",
        ],
        false,
    );
    s.call(
        "frozen",
        &[
            "--op-id",
            "frozen",
            "edit",
            "BKLG-20261020-abcd",
            "--expect",
            "1",
            "--body",
            "change",
        ],
        false,
    );
    s.call(
        "frozen",
        &[
            "--op-id",
            "frozen-q",
            "edit",
            "Q-20261020-efgh",
            "--expect",
            "1",
            "--body",
            "change",
        ],
        false,
    );
    s.call("reader", &["show", "ADR-0001"], true);
    s.call(
        "author",
        &[
            "--op-id",
            "new-note",
            "--role",
            "agent",
            "new",
            "note",
            "--title",
            "New work note",
            "--body",
            "Native note",
        ],
        true,
    );
    let out = s.dir.path().join("rendered");
    s.call(
        "exporter",
        &[
            "render",
            "--format",
            "liskov",
            "--out",
            out.to_str().unwrap(),
        ],
        true,
    );
    assert!(!out.join("assets/a.bin").exists());
    let text = fs::read_to_string(out.join("raw/one.md")).unwrap();
    assert!(text.contains("https://github.com/example/docs/blob/"));
    assert!(text.contains("`[literal](../../run.sh)`"));
    assert!(text.contains("$Format:%H$"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(out.join("raw/exec.md"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o555
        );
    }
    let copied = s.dir.path().join("copied.json");
    fs::copy(&plan, &copied).unwrap();
    fs::remove_file(plan).unwrap();
    fs::remove_dir_all(repo).unwrap();
    let verification = s.call(
        "exporter",
        &["verify-export", "--dir", out.to_str().unwrap()],
        true,
    );
    assert_eq!(verification["data"]["mismatches"], json!([]));
    assert_eq!(verification["data"]["stale"], false);
    let replay = s.call(
        "second-client",
        &[
            "--op-id",
            "markdown",
            "import",
            "apply",
            copied.to_str().unwrap(),
        ],
        true,
    );
    assert_eq!(replay["data"], imported["data"]);
    assert_eq!(
        s.call(
            "second-client",
            &[
                "--op-id",
                "same-source",
                "import",
                "apply",
                copied.to_str().unwrap()
            ],
            true
        )["data"]["identical_replay"],
        true
    );
    assert_eq!(
        s.call("reader", &["show", id], true)["data"]["record"]["body"],
        "Reviewed replacement"
    );
}

#[test]
fn shared_markdown_plan_refuses_a_committed_export() {
    let s = Server::empty();
    s.init(None);
    let repo = editable_markdown_source(&s);
    let docs = repo.join("docs");
    let out = docs.join("export");
    s.call(
        "exporter",
        &["render", "--out", out.to_str().unwrap()],
        true,
    );
    for args in [
        &["add", "docs/export"][..],
        &["commit", "-qm", "committed export"],
    ] {
        let output = Command::new("git")
            .current_dir(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let before = s.main_head();
    let plan = s.dir.path().join("markdown.plan.json");
    let error = s.call(
        "planner",
        &[
            "import",
            "plan",
            "markdown",
            docs.to_str().unwrap(),
            "--rev",
            "HEAD",
            "--git-web-url",
            "https://github.com/example/docs",
            "--out",
            plan.to_str().unwrap(),
        ],
        false,
    );
    assert!(
        error
            .to_string()
            .contains("refusing Scrolls export at docs/export. Edit through the store."),
        "{error}"
    );
    assert_eq!(s.main_head(), before);
    assert!(!plan.exists());
}
