use scrolls::{
    model::*,
    mutation, policy, query,
    schema::{Direction, Schema},
};
use serde_json::json;
#[test]
fn field_types_defaults_and_closed_schemas() {
    let schema = Schema::parse(include_str!("../examples/research.yaml")).unwrap();
    let mut r = schema.new_record("trial", "Trial", "", "alice").unwrap();
    assert_eq!(r.fields["replicated"], false);
    assert_eq!(r.confidence, "");
    assert_eq!(r.review_every, 0);
    assert!(schema.validate_record(&r).is_err());
    r.fields["count"] = json!(0);
    schema.validate_record(&r).unwrap();
    for (key, bad) in [
        ("count", json!(1.5)),
        ("score", json!("1")),
        ("replicated", json!(1)),
        ("observed", json!("yesterday")),
        ("extra", json!(1)),
    ] {
        let mut bad_record = r.clone();
        bad_record.fields[key] = bad;
        assert!(schema.validate_record(&bad_record).is_err(), "{key}");
    }
    let handbook = Schema::parse(include_str!("../examples/handbook.yaml")).unwrap();
    let mut page = handbook.new_record("page", "Page", "", "alice").unwrap();
    page.fields["paths"] = json!(["src/**", "["]);
    assert!(handbook.validate_record(&page).is_err());
    page.fields["paths"] = json!(["src/**"]);
    page.fields["updated"] = json!("2026-02-30");
    assert!(handbook.validate_record(&page).is_err());
    page.fields["updated"] = json!("2026-10-16");
    handbook.validate_record(&page).unwrap();
    let mut value = serde_json::to_value(handbook).unwrap();
    value["kinds"]["page"]["fields"]["audience"]["default"] = json!("invalid");
    assert!(Schema::parse(&value.to_string()).is_err());
    value["unknown"] = json!(true);
    assert!(Schema::parse(&value.to_string()).is_err());
}
#[test]
fn predicates_traversal_and_applicability_are_deterministic() {
    let schema = Schema::parse(include_str!("../examples/handbook.yaml")).unwrap();
    let mut g = Graph {
        schema,
        ..Default::default()
    };
    for name in ["one", "two", "three"] {
        let mut r = g.schema.new_record("page", name, name, "alice").unwrap();
        r.id = format!("sc-{name}");
        r.fields["updated"] = json!("2026-01-01");
        r.fields["paths"] = json!(["src/**/*.rs"]);
        g.records.insert(r.id.clone(), r);
    }
    g.add_edge("sc-one", "chapter", "sc-two", "", "alice");
    g.add_edge("sc-two", "chapter", "sc-three", "", "alice");
    for (direction, count) in [
        (Direction::Incoming, 1),
        (Direction::Outgoing, 3),
        (Direction::Both, 3),
    ] {
        assert_eq!(
            query::traverse(&g, "sc-one", None, 2, direction, false).unwrap()["items"]
                .as_array()
                .unwrap()
                .len(),
            count
        );
    }
    assert_eq!(
        query::traverse(&g, "sc-one", None, 1, Direction::Both, false).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    g.add_edge("sc-three", "chapter", "sc-one", "", "alice");
    assert!(policy::lint(&g).iter().any(|e| e.contains("cycle")));
    assert_eq!(
        query::traverse(&g, "sc-one", None, 100, Direction::Both, false).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let q = json!({"op":"all","args":[{"op":"compare","field":"owner","cmp":"eq","value":"$actor"},{"op":"path_glob","field":"fields.paths"},{"op":"exists","field":"fields.updated"},{"op":"date_plus","field":"fields.updated","days":30,"cmp":"lt","value":"$now"},{"op":"relation","relation":"chapter","direction":"outgoing","min":1,"max":1,"where":{"op":"compare","field":"title","cmp":"eq","value":"two"}}]});
    let result = query::evaluate(
        &g,
        &json!({}),
        &q.to_string(),
        "alice",
        &["src/a.rs".into()],
        Some("2026-10-16T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["items"][0]["id"], "sc-one");
    let routing =
        json!({"triggers":[{"id":"sc-one","globs":["docs/**"],"scope_clause":"shared contract"}]});
    let result = query::evaluate(
        &g,
        &routing,
        "op: applicability",
        "alice",
        &["docs/a.md".into()],
        Some("2026-10-16T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["diagnostics"][0]["scope_clause"], "shared contract");
    g.records.get_mut("sc-three").unwrap().fields["paths"] = json!("[");
    assert!(query::evaluate(&g, &routing, "op: applicability", "alice", &[], None).is_err());
    g.records.get_mut("sc-three").unwrap().aliases = vec!["ambiguous".into()];
    g.records.get_mut("sc-two").unwrap().aliases = vec!["ambiguous".into()];
    assert!(g.resolve("ambiguous").is_err());
}
#[test]
fn relinking_cannot_change_guard_authority_and_batches_use_initial_revisions() {
    let mut g = Graph::default();
    let mut a = Record::new("answer", "Response", "", "bob").unwrap();
    a.id = "sc-answer".into();
    let mut q1 = Record::new("question", "Original", "", "alice").unwrap();
    q1.id = "sc-q1".into();
    let mut q2 = Record::new("question", "Replacement", "", "bob").unwrap();
    q2.id = "sc-q2".into();
    for r in [a, q1, q2] {
        g.records.insert(r.id.clone(), r);
    }
    g.add_edge("sc-answer", "answers", "sc-q1", "", "bob");
    let ops=mutation::parse(&json!([
        {"op":"unlink","from":"sc-answer","rel":"answers","to":"sc-q1","expect":1,"expect_to":1},
        {"op":"link","from":"sc-answer","rel":"answers","to":"sc-q2","expect":1,"expect_to":1},
        {"op":"status","id":"sc-answer","state":"accepted","expect":1},
        {"op":"status","id":"sc-q2","state":"answered","expect":1}
    ]).to_string()).unwrap();
    let (after, _, _) = mutation::apply(&g, &ops, "bob", &Default::default()).unwrap();
    assert_eq!(after.records["sc-answer"].revision, 2);
    assert!(mutation::guards(&g.schema, &g, &after, &ops, "bob", "human").is_err());
    assert!(
        mutation::requirements(&g, &after, &ops)
            .unwrap()
            .iter()
            .any(|r| r.check("bob", "human").is_err())
    );
}

#[test]
fn explicit_null_defaults_survive_schema_storage_roundtrip() {
    let mut v =
        serde_json::to_value(Schema::parse(include_str!("../examples/handbook.yaml")).unwrap())
            .unwrap();
    v["kinds"]["page"]["fields"]["optional_json"] =
        json!({"type":"json","default":null,"required":true});
    let schema = Schema::parse(&v.to_string()).unwrap();
    let encoded = serde_json::to_string(&schema).unwrap();
    let loaded: Schema = serde_json::from_str(&encoded).unwrap();
    assert_eq!(schema, loaded);
    let record = loaded.new_record("page", "Title", "", "alice").unwrap();
    assert_eq!(
        record.fields.get("optional_json"),
        Some(&serde_json::Value::Null)
    );
    loaded.validate_record(&record).unwrap();
    assert!(
        query::evaluate(
            &Graph::default(),
            &json!({}),
            "op: relation\nrelation: nonexistent",
            "alice",
            &[],
            None
        )
        .is_err()
    );
}

#[test]
fn already_accepted_answers_cannot_be_swapped_around_owner_guards() {
    let mut before = Graph::default();
    for (id, kind, owner, status) in [
        ("sc-q1", "question", "alice", "answered"),
        ("sc-q2", "question", "bob", "answered"),
        ("sc-a1", "answer", "alice", "accepted"),
        ("sc-a2", "answer", "bob", "accepted"),
    ] {
        let mut r = Record::new(kind, id, "", owner).unwrap();
        r.id = id.into();
        r.status = status.into();
        before.records.insert(id.into(), r);
    }
    before.add_edge("sc-a1", "answers", "sc-q1", "", "alice");
    before.add_edge("sc-a2", "answers", "sc-q2", "", "bob");
    let mut after = before.clone();
    after.edges.clear();
    after.add_edge("sc-a1", "answers", "sc-q2", "", "bob");
    after.add_edge("sc-a2", "answers", "sc-q1", "", "bob");
    assert!(policy::lint(&after).is_empty());
    assert!(
        before
            .schema
            .guards(&before, &after, "bob", "human")
            .is_err()
    );
    assert!(
        mutation::requirements(&before, &after, &[])
            .unwrap()
            .iter()
            .any(|r| r.check("bob", "human").is_err())
    );
}
