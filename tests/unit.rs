use scrolls::{import::*, model::*, policy, query};
#[test]
fn paths_and_frontmatter() {
    for p in ["../escape", "/absolute", "a/../../b", "a\\b"] {
        assert!(safe_path(p).is_err())
    }
    assert!(safe_path("raw/photo.png").is_ok());
    assert_eq!(
        resolve_reference("knowledge/a.md", "../raw/x.md#anchor"),
        Some("raw/x.md".into())
    );
    assert_eq!(resolve_reference("a.md", "../outside"), None);
    let (fm, body) =
        frontmatter("---\ntitle: hello\nsource_documents:\n  - ../raw/x.md\n---\n\nbody\n");
    assert_eq!(fm["title"], "hello");
    assert_eq!(body, "\nbody\n");
}
#[test]
fn policy_and_budget() {
    let mut g = Graph::default();
    let e = Record::new("evidence", "source", "facts", "alice").unwrap();
    let mut k = Record::new("knowledge", "result", "x".repeat(200).as_str(), "alice").unwrap();
    k.status = "current".into();
    g.records.insert(e.id.clone(), e.clone());
    g.records.insert(k.id.clone(), k.clone());
    assert!(policy::lint(&g).contains(&format!("{}:missing-support", k.id)));
    g.add_edge(&e.id, "supports", &k.id, "", "alice");
    assert!(policy::lint(&g).is_empty());
    let packet = query::context(&g, &k.id, 10).unwrap();
    assert!(packet["items"].as_array().unwrap().is_empty());
    assert!(packet["used_bytes"].as_u64().unwrap() <= 10);
    assert!(transition("evidence", "archived", "captured").is_err());
}
#[test]
fn legacy_index_parser_preserves_legacy_scalar_rules() {
    let text = "---\ntitle: \"Literal \\u2014 and \\\"quote\\\"\"\n  continuation ignored\nsource_documents:\n- ../raw/a.md\nstatus: canonical\n---\n";
    let fm = scrolls::render::legacy_frontmatter(text);
    assert_eq!(fm["title"], "Literal \\u2014 and \\\"quote\\\"");
    assert_eq!(fm["source_documents"], serde_json::json!(["../raw/a.md"]));
    let fm = scrolls::render::legacy_frontmatter("---\ntitle: PR #123 evidence\n---\n");
    assert_eq!(fm["title"], "PR #123 evidence");
}

#[test]
fn native_entity_names_resolve_and_ambiguous_names_fail() {
    let mut g = Graph::default();
    let a = Record::new("entity", "Billing", "", "alice").unwrap();
    g.records.insert(a.id.clone(), a.clone());
    assert_eq!(g.resolve("billing").unwrap(), a.id);
    let b = Record::new("entity", "Billing", "", "alice").unwrap();
    g.records.insert(b.id.clone(), b);
    assert!(g.resolve("billing").is_err());
}

#[test]
fn human_presentation_preserves_body_lines_and_neutralizes_controls() {
    let data = serde_json::json!({"revision":"abc","branch":"main","data":{"record":{"id":"sc-id","kind":"evidence","title":"Facts","status":"captured","revision":1,"owner":"alice","body":"first\nsecond\u{1b}[2J"},"edges":[]}});
    let text = scrolls::presentation::human(&data);
    assert!(text.contains("first\nsecond"));
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("Dolt revision: abc"));
}

#[test]
fn mandatory_doctrine_is_complete_deterministic_and_exactly_accounted() {
    use serde_json::json;
    let mut g = Graph::default();
    for id in ["sc-b", "sc-a"] {
        let mut r = Record::new("doctrine", id, &"é\n\"".repeat(6000), "author").unwrap();
        r.id = id.into();
        r.status = "active".into();
        r.fields = json!({"glob":"**/*.rs"});
        r.original = Some(json!({"body":r.body}));
        g.records.insert(r.id.clone(), r);
    }
    let mut primer = Record::new("primer", "optional", "optional", "author").unwrap();
    primer.status = "current".into();
    g.records.insert(primer.id.clone(), primer.clone());
    g.add_edge("sc-a", "applies_to", "glob:**/*.rs", "", "author");
    let routing =
        json!({"triggers":[{"id":"sc-a","globs":["**/*.rs"],"scope_clause":"a judgement"}]});
    let packet = query::prime(&g, &routing, &["src/a.rs".into()], "author", 16000).unwrap();
    let items = packet["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], "sc-a");
    for item in items {
        assert_eq!(item["body"], g.records[item["id"].as_str().unwrap()].body);
        assert!(item.get("original").is_none());
    }
    let size = serde_json::to_vec(items).unwrap().len();
    assert_eq!(packet["used_bytes"], size);
    assert_eq!(packet["mandatory_bytes"], size);
    assert_eq!(packet["over_budget_bytes"], size - 16000);
    assert_eq!(packet["omitted"], json!([primer.id]));
    assert_eq!(packet["undecided_doctrine"].as_array().unwrap().len(), 1);
    assert_eq!(
        query::prime(&g, &routing, &["src/a.rs".into()], "author", 16000).unwrap(),
        packet
    );
    assert!(
        query::context(&g, "sc-a", 16000).unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(scrolls::presentation::human(&json!({"data":packet})).contains("budget exceeded"));
    for id in ["sc-a", "sc-b"] {
        g.records.get_mut(id).unwrap().body = "small rule".into();
    }
    let all = query::prime(&g, &routing, &["src/a.rs".into()], "author", 16000).unwrap();
    assert_eq!(all["items"].as_array().unwrap().len(), 3);
    let exact = serde_json::to_vec(&all["items"]).unwrap().len();
    assert_eq!(all["used_bytes"], exact);
    assert_eq!(
        query::prime(&g, &routing, &["src/a.rs".into()], "author", exact).unwrap()["items"],
        all["items"]
    );
    let omitted = query::prime(&g, &routing, &["src/a.rs".into()], "author", exact - 1).unwrap();
    assert_eq!(omitted["items"].as_array().unwrap().len(), 2);
    assert_eq!(omitted["over_budget_bytes"], 0);
    assert_eq!(omitted["mandatory_bytes"], omitted["used_bytes"]);
}

#[test]
fn trigger_parser_routing_and_lint_share_validation() {
    use serde_json::json;
    let mut r = Record::new("doctrine", "rule", "required", "alice").unwrap();
    r.status = "active".into();
    for fields in [
        json!({"glob":"["}),
        json!({"glob":3}),
        json!({"glob":["ok",false]}),
        json!({"glob":" "}),
        json!({"glob":null}),
        json!({"scope_clause":""}),
        json!({"scope_clause":[]}),
    ] {
        r.fields = fields;
        let g = Graph {
            records: [(r.id.clone(), r.clone())].into(),
            ..Default::default()
        };
        assert!(query::doctrine(&g, &json!({}), &[]).is_err());
        assert!(policy::lint(&g).iter().any(|e| e.contains(&r.id)));
        assert!(policy::validate_change(&Graph::default(), &g, &Default::default()).is_err());
    }
    for fields in [
        json!({"glob":[]}),
        json!({"glob":"other/**"}),
        json!({"scope_clause":"shared contract"}),
    ] {
        r.fields = fields;
        let g = Graph {
            records: [(r.id.clone(), r.clone())].into(),
            ..Default::default()
        };
        let result = query::doctrine(&g, &json!({}), &["src/a.rs".into()]).unwrap();
        assert_eq!(result["matched"], json!([]));
        assert!(policy::lint(&g).is_empty());
        let bad = json!({"triggers":[{"id":r.id,"globs":["["]}]});
        assert!(query::doctrine(&g, &bad, &[]).is_err());
        assert!(!policy::lint_routing(&bad).is_empty());
    }
}

#[test]
fn answered_question_groups_are_atomic_and_unambiguous() {
    use serde_json::json;
    let mut g = Graph::default();
    let mut q = Record::new("question", "Question", "question text", "alice").unwrap();
    q.status = "answered".into();
    q.fields = json!({"asked_by":"alice"});
    g.records.insert(q.id.clone(), q.clone());
    assert!(query::prime(&g, &json!({}), &[], "alice", 16000).is_err());
    let mut a = Record::new("answer", "Answer", "accepted answer", "bob").unwrap();
    a.status = "accepted".into();
    a.revision = 7;
    g.records.insert(a.id.clone(), a.clone());
    g.add_edge(&a.id, "answers", &q.id, "", "bob");
    for status in ["offered", "declined"] {
        let mut alt = Record::new("answer", "Alternative", "excluded", "charlie").unwrap();
        alt.status = status.into();
        g.add_edge(&alt.id, "answers", &q.id, "", "charlie");
        g.records.insert(alt.id.clone(), alt);
    }
    let full = query::prime(&g, &json!({}), &[], "alice", 16000).unwrap();
    assert_eq!(full["items"].as_array().unwrap().len(), 2);
    assert_eq!(full["items"][0]["accepted_answer_id"], a.id);
    assert_eq!(full["items"][1]["question_id"], q.id);
    assert_eq!(full["items"][1]["actor"], "bob");
    assert_eq!(full["items"][1]["revision"], 7);
    let size = serde_json::to_vec(&full["items"]).unwrap().len();
    assert_eq!(full["used_bytes"], size);
    assert_eq!(
        query::prime(&g, &json!({}), &[], "alice", size).unwrap()["items"],
        full["items"]
    );
    assert_eq!(
        query::prime(&g, &json!({}), &[], "alice", size - 1).unwrap()["omitted"],
        json!([q.id, a.id])
    );
    let mut duplicate = a.clone();
    duplicate.id = "sc-duplicate".into();
    g.add_edge(&duplicate.id, "answers", &q.id, "", "bob");
    g.records.insert(duplicate.id.clone(), duplicate);
    assert!(query::prime(&g, &json!({}), &[], "alice", 16000).is_err());
    g.records.get_mut(&q.id).unwrap().frozen = true;
    assert_eq!(
        query::prime(&g, &json!({}), &[], "alice", 16000).unwrap()["items"],
        json!([])
    );
}

#[test]
fn every_native_lifecycle_exports_losslessly_with_stable_paths() {
    use scrolls::{
        cli::Format,
        render::{Options, render},
    };
    use serde_json::{Value, json};
    let mut g = Graph::default();
    for kind in KINDS {
        for status in lifecycle(kind).unwrap() {
            let mut r = Record::new(kind, &format!("{kind} {status}"), "body", "alice").unwrap();
            r.status = status.to_string();
            r.created = "2020-01-02T03:04:05Z".into();
            r.fields = json!({"native_details":{"kept":true}});
            g.records.insert(r.id.clone(), r);
        }
    }
    let ids: Vec<_> = g.records.keys().cloned().collect();
    g.records.get_mut(&ids[0]).unwrap().body = format!("[other]({}#anchor)", ids[1]);
    g.add_edge(&ids[0], "related", &ids[1], "preserved", "bob");
    let dir = tempfile::tempdir().unwrap();
    for format in [Format::Native, Format::Liskov] {
        let out = dir.path().join(format!("{format:?}"));
        render(
            &g,
            &[],
            None,
            &[],
            "rev",
            Options {
                out: &out,
                format,
                dry_run: false,
            },
        )
        .unwrap();
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(out.join("SCROLLS-MANIFEST.json")).unwrap())
                .unwrap();
        let graph: Value =
            serde_json::from_slice(&std::fs::read(out.join("scrolls/graph.json")).unwrap())
                .unwrap();
        assert_eq!(graph["records"], json!(g.records));
        assert_eq!(graph["edges"], json!(g.edges));
        assert_eq!(
            manifest["records"].as_object().unwrap().len(),
            g.records.len()
        );
        for r in g.records.values() {
            let path = manifest["records"][&r.id].as_str().unwrap();
            assert!(path.contains(&r.id));
            if matches!(r.kind.as_str(), "evidence" | "note") {
                assert!(path.contains("2020-01-02"));
            }
            if r.kind == "decision" {
                assert!(path.starts_with("scrolls/decision/"));
                assert!(r.metadata.is_none());
            }
            let text = std::fs::read_to_string(out.join(path)).unwrap();
            assert!(text.contains(&format!("Lifecycle: `{}`", r.status)));
            let fm = frontmatter(&text).0;
            assert!(["canonical", "working", "raw"].contains(&fm["status"].as_str().unwrap()));
            assert_eq!(fm["related_repos"], json!([]));
            if r.id == ids[0] {
                assert!(!text.contains(&format!("]({}#", ids[1])));
                assert!(text.contains(".md#anchor)"));
            }
        }
        let index = std::fs::read_to_string(out.join("scrolls/README.md")).unwrap();
        assert!(ids.iter().all(|id| index.contains(id)));
    }
}

#[test]
fn legacy_number_derivation_is_read_only_and_export_collisions_fail() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use scrolls::{
        cli::Format,
        render::{Options, render},
    };
    let mut r = Record::new("decision", "Imported", "body", "old").unwrap();
    r.path = Some("decisions/adr/0236-old.md".into());
    r.aliases = vec!["ADR-0236".into()];
    r.original = Some(serde_json::to_value(&r).unwrap());
    let before = r.clone();
    r.normalize_metadata();
    assert_eq!(r.metadata.as_ref().unwrap().number, "0236");
    assert_eq!(r.revision, before.revision);
    assert_eq!(r.actor, before.actor);
    assert_eq!(r.original, before.original);
    let g = Graph {
        records: [(r.id.clone(), r.clone())].into(),
        ..Default::default()
    };
    assert_eq!(g.resolve("ADR-0236").unwrap(), r.id);
    let bytes = b"original bytes\r\n";
    let file = SourceFile {
        path: r.path.clone().unwrap(),
        bytes: STANDARD.encode(bytes),
        sha256: digest(bytes),
        git_oid: "oid".into(),
        mode: 0o100644,
    };
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("legacy");
    render(
        &g,
        std::slice::from_ref(&file),
        None,
        &[],
        "rev",
        Options {
            out: &out,
            format: Format::Liskov,
            dry_run: false,
        },
    )
    .unwrap();
    assert_eq!(std::fs::read(out.join(r.path.unwrap())).unwrap(), bytes);
    let collision = SourceFile {
        path: "scrolls/graph.json".into(),
        ..file
    };
    assert!(
        render(
            &g,
            &[collision],
            None,
            &[],
            "rev",
            Options {
                out: &dir.path().join("bad"),
                format: Format::Liskov,
                dry_run: false
            }
        )
        .unwrap_err()
        .to_string()
        .contains("collision")
    );
    assert!(!dir.path().join("bad").exists());
}

#[test]
fn native_supersession_cannot_publish_incompatible_legacy_status() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use scrolls::{
        cli::Format,
        render::{Options, render},
    };
    use serde_json::json;
    let mut old = Record::new("decision", "Legacy", "legacy body", "author").unwrap();
    old.path = Some("decisions/adr/0001-legacy.md".into());
    old.aliases = vec!["ADR-0001".into()];
    old.status = "accepted".into();
    old.fields = json!({"doc_type":"adr","status":"accepted"});
    old.original = Some(serde_json::to_value(&old).unwrap());
    let new = Record::new("decision", "Native", "native body", "author").unwrap();
    let mut g = Graph {
        records: [(old.id.clone(), old.clone()), (new.id.clone(), new.clone())].into(),
        ..Default::default()
    };
    g.add_edge(&new.id, "supersedes", &old.id, "replacement", "author");
    let bytes = b"unchanged legacy bytes\n";
    let files = [SourceFile {
        path: old.path.clone().unwrap(),
        bytes: STANDARD.encode(bytes),
        sha256: digest(bytes),
        git_oid: "oid".into(),
        mode: 0o100644,
    }];
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("supplemental");
    render(
        &g,
        &files,
        None,
        &[],
        "rev",
        Options {
            out: &out,
            format: Format::Liskov,
            dry_run: false,
        },
    )
    .unwrap();
    assert_eq!(std::fs::read(out.join(old.path.unwrap())).unwrap(), bytes);
    g.records.get_mut(&old.id).unwrap().status = "superseded".into();
    let bad = dir.path().join("incompatible");
    let error = render(
        &g,
        &files,
        None,
        &[],
        "rev",
        Options {
            out: &bad,
            format: Format::Liskov,
            dry_run: false,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("numbered ADR successor"));
    assert!(!bad.exists());
}
