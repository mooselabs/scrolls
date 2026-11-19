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
    assert!(
        policy::lint(&g, &scrolls::beads::Local::default())
            .iter()
            .any(|e| e.contains("cycle"))
    );
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
    let (after, _, _) =
        mutation::apply(&g, &ops, "bob", &scrolls::beads::Local::default()).unwrap();
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
    assert!(policy::lint(&after, &scrolls::beads::Local::default()).is_empty());
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

#[test]
fn ontology_format_two_names_the_work_target() {
    let bundled = Schema::default();
    assert_eq!(bundled.format, 2);
    let mut value = serde_json::to_value(&bundled).unwrap();
    assert!(Schema::parse(&value.to_string()).is_ok());
    value["relations"]["related_work"]["external"] = json!(["beads"]);
    assert!(
        Schema::parse(&value.to_string())
            .unwrap_err()
            .to_string()
            .contains("unknown external type")
    );
    value = serde_json::to_value(&bundled).unwrap();
    value["format"] = json!(1);
    value["body_links"] = json!({"local":"related"});
    assert!(
        Schema::parse(&value.to_string())
            .unwrap_err()
            .to_string()
            .contains("unknown external type")
    );
    value["format"] = json!(3);
    assert!(
        Schema::parse(&value.to_string())
            .unwrap_err()
            .to_string()
            .contains("unsupported ontology format")
    );
    for example in [
        include_str!("../examples/handbook.yaml"),
        include_str!("../examples/research.yaml"),
    ] {
        assert_eq!(Schema::parse(example).unwrap().format, 2);
    }
    value = serde_json::to_value(bundled).unwrap();
    value["body_links"] = json!({"beads":"related_work"});
    assert!(
        Schema::parse(&value.to_string())
            .unwrap_err()
            .to_string()
            .contains("unknown body link type")
    );
    // Format 1 historically allowed unused keys; loading it must keep that meaning.
    let mut old = Schema::parse(include_str!("fixtures/knowledge-format-1.yaml")).unwrap();
    old.body_links
        .insert("unused".into(), "related_work".into());
    assert!(Schema::parse(&serde_json::to_string(&old).unwrap()).is_ok());
}

#[test]
fn invariant_free_revision_baselines() {
    // Captured from this packet's merge base, 2fc9b22, before adding invariants.
    for (text, revision) in [
        (
            include_str!("../schemas/knowledge.yaml"),
            "9843cecc9fa704c0ec73077c5a0e55f6b5300211c711baa26bba5290c6fc80cc",
        ),
        (
            include_str!("../examples/handbook.yaml"),
            "dca9183dae47a800d13de00edb84f78905f801d757fd01ea34225dc6ff592385",
        ),
        (
            include_str!("../examples/research.yaml"),
            "093f953cacd7e185e58b4e793566756ad50d02de333e3aa52b20da9fdef0c65b",
        ),
    ] {
        let schema = Schema::parse(text).unwrap();
        assert_eq!(schema.revision(), revision);
        assert!(
            serde_json::to_value(&schema)
                .unwrap()
                .get("invariants")
                .is_none()
        );
    }
}

#[test]
fn named_invariants_validate_their_selection_and_predicates() {
    let base = serde_json::to_value(Schema::default()).unwrap();
    let rule = json!({"name":"named-rule", "description":"Evidence has a title", "kinds":["evidence"],
        "require":{"op":"exists","field":"title"}});
    let mut valid = base.clone();
    valid["invariants"] = json!([rule.clone()]);
    Schema::parse(&valid.to_string()).unwrap();
    let mut cases = vec![];
    let mut duplicate = valid.clone();
    duplicate["invariants"] = json!([rule.clone(), rule.clone()]);
    cases.push(duplicate);
    let mut collision = valid.clone();
    collision["kinds"]["evidence"]["cardinality"] = json!([{
        "name":"named-rule", "relation":"supports", "direction":"outgoing", "min":0
    }]);
    cases.push(collision);
    let mut old_format = valid.clone();
    // Convert the format-2 work spellings as well, isolating the invariant error.
    old_format =
        serde_json::from_str(&old_format.to_string().replace("\"work\"", "\"beads\"")).unwrap();
    old_format["format"] = json!(1);
    cases.push(old_format);
    let mut kind = valid.clone();
    kind["invariants"][0]["kinds"] = json!(["unknown"]);
    cases.push(kind);
    let mut no_selection = valid.clone();
    no_selection["invariants"][0]["kinds"] = json!([]);
    cases.push(no_selection);
    for expr in [
        json!({"op":"relation", "relation":"unknown"}),
        json!({"op":"compare", "field":"kind", "cmp":"eq", "value":"unknown"}),
        json!({"op":"compare", "field":"owner", "cmp":"eq", "value":"$actor"}),
        json!({"op":"compare", "field":"created", "cmp":"eq", "value":"$now"}),
        json!({"op":"date_plus", "field":"created", "days":1,"cmp":"lt", "value":"$now"}),
        json!({"op":"path_glob", "field":"fields.paths"}),
        json!({"op":"applicability"}),
        json!({"op":"relation", "relation":"supports", "where":{"op":"not","arg":{"op":"applicability"}}}),
    ] {
        for key in ["when", "require"] {
            let mut invalid = valid.clone();
            invalid["invariants"][0][key] = json!({"op":"all", "args":[expr.clone()]});
            cases.push(invalid);
        }
    }
    for invalid in cases {
        let error = format!("{:#}", Schema::parse(&invalid.to_string()).unwrap_err());
        assert!(error.contains("named-rule"), "{error}: {invalid}");
    }
    // Predicate-only selection and every deterministic operator round-trip.
    valid["invariants"][0]["kinds"] = json!([]);
    valid["invariants"][0]["when"] =
        json!({"op":"compare","field":"kind","cmp":"eq","value":"evidence"});
    valid["invariants"][0]["require"] = json!({"op":"any","args":[
        {"op":"not","arg":{"op":"exists","field":"fields.date"}},
        {"op":"date_plus","field":"fields.date","days":{"field":"review_every"},"cmp":"le","value":"2030-01-01"}
    ]});
    let schema = Schema::parse(&valid.to_string()).unwrap();
    assert_eq!(
        schema,
        Schema::parse(&serde_yaml::to_string(&schema).unwrap()).unwrap()
    );
}

#[test]
fn invariant_names_cannot_mask_builtin_lint_rules() {
    for name in [
        "invalid-status",
        "required-fields",
        "confidence",
        "review_every",
        "last_reviewed",
        "supports-cycle",
        "future-relation-cycle",
    ] {
        for configured_envelope in [false, true] {
            let mut value = serde_json::to_value(Schema::default()).unwrap();
            value["envelope_fields"] = if configured_envelope {
                json!({"confidence":{"type":"string"}, "review_every":{"type":"integer"},
                    "last_reviewed":{"type":"string"}})
            } else {
                json!({})
            };
            value["invariants"] = json!([{"name":name, "description":"No outgoing support",
                "kinds":["evidence"],
                "require":{"op":"relation","relation":"supports","direction":"outgoing","max":0}}]);
            let error = format!("{:#}", Schema::parse(&value.to_string()).unwrap_err());
            assert!(error.contains(name), "{error}");
            assert!(error.contains("built-in lint rule"), "{error}");
        }
    }
}

#[test]
fn invariant_metadata_roundtrip_preserves_revision() {
    use scrolls::{
        storage::Store,
        store::{Change, ChangeSet, KnowledgeStore},
    };
    let dir = tempfile::tempdir().unwrap();
    let binary = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".tools/dolt");
    let store = Store::open(&dir.path().join("store"), &binary, true).unwrap();
    let mut value = serde_json::to_value(Schema::default()).unwrap();
    value["invariants"] = json!([{"name":"has-title", "description":"Title is present", "kinds":["evidence"],
        "when":{"op":"all","args":[]},"require":{"op":"exists","field":"title"}}]);
    let schema = Schema::parse(&value.to_string()).unwrap();
    store
        .commit(
            &ChangeSet::from(Change::put("metadata", "ontology", &schema).unwrap()),
            "configure invariants",
            "alice",
        )
        .unwrap();
    drop(store);
    let reopened = Store::open(&dir.path().join("store"), &binary, false).unwrap();
    let loaded: Schema = reopened.one("metadata", "ontology").unwrap().unwrap();
    assert_eq!(loaded, schema);
    assert_eq!(loaded.revision(), schema.revision());
}

#[test]
fn documented_invariant_accepts_a_published_public_page_with_a_staff_chapter() {
    let docs = include_str!("../docs/commands.md");
    let example = docs
        .split("```yaml\ninvariants:")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let schema = Schema::parse(&format!(
        "{}\ninvariants:{}",
        include_str!("../examples/handbook.yaml"),
        example
    ))
    .unwrap();
    let mut graph = Graph {
        schema,
        ..Graph::default()
    };
    let mut public = graph
        .schema
        .new_record("page", "Public", "", "alice")
        .unwrap();
    public.status = "published".into();
    public.fields["audience"] = json!("public");
    let mut staff = graph
        .schema
        .new_record("page", "Staff", "", "alice")
        .unwrap();
    staff.status = "published".into();
    graph.records.insert(public.id.clone(), public.clone());
    graph.records.insert(staff.id.clone(), staff.clone());
    let work = scrolls::beads::Local::default();
    assert_eq!(
        policy::lint(&graph, &work),
        [format!("{}:published-chapter", public.id)].into()
    );
    graph.add_edge(&public.id, "chapter", &staff.id, "", "alice");
    assert!(policy::lint(&graph, &work).is_empty());
}

#[test]
fn alias_sequence_declaration_is_validated() {
    let mut value: serde_json::Value =
        serde_yaml::from_str(include_str!("../examples/handbook.yaml")).unwrap();
    value["kinds"]["page"]["alias_sequence"] = json!({"prefix":"ADR-", "width":4});
    let schema = Schema::parse(&value.to_string()).unwrap();
    let stored = serde_json::to_string(&schema).unwrap();
    let restored: Schema = serde_json::from_str(&stored).unwrap();
    assert_eq!(schema, restored);
    for bad in [
        json!({"prefix":"", "width":4}),
        json!({"prefix":"ADR-", "width":0}),
        serde_json::Value::Null,
    ] {
        let mut invalid = value.clone();
        invalid["kinds"]["page"]["alias_sequence"] = bad;
        assert!(Schema::parse(&invalid.to_string()).is_err());
    }
    let mut duplicate = value.clone();
    duplicate["kinds"]["other"] = duplicate["kinds"]["page"].clone();
    assert!(
        Schema::parse(&duplicate.to_string())
            .unwrap_err()
            .to_string()
            .contains("duplicate alias sequence prefix")
    );
    value["format"] = json!(1);
    assert!(
        Schema::parse(&value.to_string())
            .unwrap_err()
            .to_string()
            .contains("unknown field alias_sequence")
    );
}

#[test]
fn sequence_alias_is_immutable_once_on_main() {
    let mut value: serde_json::Value =
        serde_yaml::from_str(include_str!("../examples/handbook.yaml")).unwrap();
    value["kinds"]["page"]["alias_sequence"] = json!({"prefix":"ADR-", "width":4});
    let mut before = Graph {
        schema: Schema::parse(&value.to_string()).unwrap(),
        ..Default::default()
    };
    let mut r = before
        .schema
        .new_record("page", "Held", "", "alice")
        .unwrap();
    r.aliases = vec!["ADR-0001".into()];
    let id = r.id.clone();
    before.records.insert(id.clone(), r);
    let work = scrolls::beads::Local::default();
    for aliases in [vec![], vec!["ADR-0002".into()]] {
        let mut after = before.clone();
        after.records.get_mut(&id).unwrap().aliases = aliases;
        assert!(
            policy::validate_change(&before, &after, &Default::default(), &work)
                .unwrap_err()
                .to_string()
                .contains("immutable sequence alias")
        );
    }
    let mut after = before.clone();
    after.schema.kinds.get_mut("page").unwrap().alias_sequence = None;
    assert!(
        policy::validate_change(&before, &after, &Default::default(), &work)
            .unwrap_err()
            .to_string()
            .contains("immutable alias sequence")
    );
    let mut after = before.clone();
    after.records.get_mut(&id).unwrap().title = "Edited".into();
    policy::validate_change(&before, &after, &Default::default(), &work).unwrap();
}
