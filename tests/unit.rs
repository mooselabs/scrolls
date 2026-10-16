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
