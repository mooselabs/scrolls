use super::*;

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
    // A fresh verification can run with no local Beads cache.
    let cache = f.dir.path().join("store/beads.json");
    std::fs::write(
        &cache,
        serde_json::to_vec(&json!({"cache":{(target):{"resolution":"cached-observation"}}}))
            .unwrap(),
    )
    .unwrap();
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
    let bead_path = "scrolls/beads/00000000-0000-4000-8000-000000000001/bd-a.md";
    let bead_page = std::fs::read_to_string(out.join(bead_path)).unwrap();
    assert!(bead_page.contains(rendered_path));
    assert!(bead_page.contains("cached-observation"));
    std::fs::remove_file(cache).unwrap();
    assert_eq!(manifest["outside_store"], json!([bead_path]));
    let verify = ["verify-export", "--dir", out.to_str().unwrap()];
    assert_eq!(f.ok(&verify)["mismatches"], json!([]));
    std::fs::remove_file(out.join(bead_path)).unwrap();
    std::fs::write(out.join(bead_path), "changed cache view").unwrap();
    f.fail(&verify, &format!("altered file {bead_path}"));
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
fn local_export_verifies_against_its_revision_and_reports_a_moved_head() {
    let f = Fixture::new();
    let evidence = f.ok(&[
        "new", "evidence", "--title", "Evidence", "--body", "Readback",
    ]);
    let out = f.dir.path().join("export");
    let dir = out.to_str().unwrap();
    f.ok(&["render", "--out", dir]);
    let args = ["verify-export", "--dir", dir];
    let fresh = f.ok(&args);
    assert_eq!(fresh["ref"], "main");
    assert_eq!(fresh["stale"], false);
    assert_eq!(fresh["mismatches"], json!([]));

    f.ok(&["new", "note", "--title", "Head moved"]);
    let stale = f.ok(&args);
    assert_eq!(stale["revision"], fresh["revision"]);
    assert_ne!(stale["head_revision"], fresh["revision"]);
    assert_eq!(stale["stale"], true);
    assert_eq!(stale["mismatches"], json!([]));
    std::fs::write(out.join("extra.md"), "extra").unwrap();
    f.fail(&args, "extra file extra.md");

    let proposal = f.supported(evidence["id"].as_str().unwrap(), "Proposal");
    let reference = proposal["branch"].as_str().unwrap();
    let out = f.dir.path().join("proposal");
    let dir = out.to_str().unwrap();
    f.ok(&["--proposal", reference, "render", "--out", dir]);
    let report = f.ok(&["verify-export", "--dir", dir]);
    assert_eq!(report["ref"], reference);
    assert_eq!(report["stale"], false);
}

#[test]
fn markdown_import_refuses_a_committed_export() {
    use std::fs;

    let f = Fixture::new();
    let repo = f.dir.path().join("repo");
    let docs = repo.join("docs");
    let out = docs.join("export");
    fs::create_dir_all(docs.join("source")).unwrap();
    fs::write(docs.join("source/note.md"), "# Source\n").unwrap();
    f.ok(&["render", "--out", out.to_str().unwrap()]);
    for args in [
        &["init", "-q"][..],
        &["add", "docs"],
        &["commit", "-qm", "committed export"],
    ] {
        let output = Command::new("git")
            .current_dir(&repo)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@localhost",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_file(out.join("RENDERED")).unwrap();

    let store_path = f.dir.path().join("store");
    let main_head = || {
        Store::open_readonly(&store_path, &dolt())
            .unwrap()
            .head("main")
            .unwrap()
    };
    let before = main_head();
    for selection in [&docs, &out] {
        f.fail(
            &[
                "import",
                "markdown",
                selection.to_str().unwrap(),
                "--rev",
                "HEAD",
            ],
            "refusing Scrolls export at docs/export. Edit through the store.",
        );
        assert_eq!(main_head(), before);
    }
    f.ok(&[
        "import",
        "markdown",
        docs.join("source").to_str().unwrap(),
        "--rev",
        "HEAD",
    ]);
}
