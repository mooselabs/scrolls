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
    assert!(
        policy::lint(&g, &scrolls::beads::Local::default())
            .contains(&format!("{}:missing-support", k.id))
    );
    g.add_edge(&e.id, "supports", &k.id, "", "alice");
    assert!(policy::lint(&g, &scrolls::beads::Local::default()).is_empty());
    assert!(
        query::context(&g, &k.id, 10)
            .unwrap_err()
            .to_string()
            .contains("at least")
    );
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
fn pinned_orientation_is_complete_and_exactly_accounted() {
    use serde_json::json;
    let mut g = Graph::default();
    for id in ["sc-b", "sc-a"] {
        let mut r = Record::new("note", id, &"é\n\"".repeat(6000), "alice").unwrap();
        r.id = id.into();
        g.records.insert(id.into(), r);
    }
    g.schema.pins = vec!["sc-b".into(), "sc-a".into(), "sc-b".into()];
    let packet = query::prime(&g, 10).unwrap();
    assert_eq!(packet["items"].as_array().unwrap().len(), 2);
    assert_eq!(packet["items"][0]["id"], "sc-b");
    let content = json!({"guide":packet["guide"],"instructions":packet["instructions"],"items":packet["items"]});
    let size = serde_json::to_vec(&content).unwrap().len();
    assert_eq!(packet["used_bytes"], size);
    assert_eq!(packet["over_budget_bytes"], size - 10);
    assert_eq!(query::prime(&g, 10).unwrap(), packet);
    assert!(query::context(&g, "sc-a", 10).is_err());
    g.schema.pins.push("missing".into());
    assert!(query::prime(&g, 10).is_err());
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
        assert!(
            policy::lint(&g, &scrolls::beads::Local::default())
                .iter()
                .any(|e| e.contains(&r.id))
        );
        assert!(
            policy::validate_change(
                &Graph::default(),
                &g,
                &Default::default(),
                &scrolls::beads::Local::default()
            )
            .is_err()
        );
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
        assert!(policy::lint(&g, &scrolls::beads::Local::default()).is_empty());
        let bad = json!({"triggers":[{"id":r.id,"globs":["["]}]});
        assert!(query::doctrine(&g, &bad, &[]).is_err());
        assert!(!policy::lint_routing(&bad).is_empty());
    }
}

#[test]
fn contexts_include_only_document_and_immediate_references() {
    let mut g = Graph::default();
    let a = Record::new("note", "A", "main body", "alice").unwrap();
    let mut b = Record::new("knowledge", "B", "neighbor body", "alice").unwrap();
    b.status = "retired".into();
    let c = Record::new("note", "C", "distant body", "alice").unwrap();
    for r in [&a, &b, &c] {
        g.records.insert(r.id.clone(), r.clone());
    }
    g.add_edge(&a.id, "related", &b.id, "", "alice");
    g.add_edge(&b.id, "related", &c.id, "", "alice");
    let data = query::context(&g, &a.id, 16000).unwrap();
    assert_eq!(data["items"][0]["body"], "main body");
    assert_eq!(data["items"][1]["status"], "retired");
    assert!(data["items"][1].get("body").is_none());
    assert_eq!(data["items"].as_array().unwrap().len(), 2);
    let size = serde_json::to_vec(&data["items"]).unwrap().len();
    assert_eq!(data["used_bytes"], size);
    assert_eq!(
        query::context(&g, &a.id, size).unwrap()["items"],
        data["items"]
    );
    assert_eq!(
        query::context(&g, &a.id, size - 1).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
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
                import_files: &[],
                out: &out,
                format,
                beads: &scrolls::beads::Local::default(),
                markdown_import: None,
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
            import_files: &[],
            out: &out,
            format: Format::Liskov,
            beads: &scrolls::beads::Local::default(),
            markdown_import: None,
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
                import_files: &[],
                out: &dir.path().join("bad"),
                format: Format::Liskov,
                beads: &scrolls::beads::Local::default(),
                markdown_import: None,
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
            import_files: &[],
            out: &out,
            format: Format::Liskov,
            beads: &scrolls::beads::Local::default(),
            markdown_import: None,
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
            import_files: &[],
            out: &bad,
            format: Format::Liskov,
            beads: &scrolls::beads::Local::default(),
            markdown_import: None,
            dry_run: false,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("numbered ADR successor"));
    assert!(!bad.exists());
}

#[test]
fn qualified_bead_references_policy_and_body_links() {
    use scrolls::beads::BeadRef;
    let a = "beads://00000000-0000-4000-8000-000000000001/bd-a.1";
    let b = "beads://00000000-0000-4000-8000-000000000002/bd-a.1";
    assert_ne!(BeadRef::parse(a).unwrap(), BeadRef::parse(b).unwrap());
    for bad in [
        "bd-a",
        "beads://missing/bd-a",
        "beads://00000000-0000-4000-8000-000000000001/../x",
    ] {
        assert!(BeadRef::parse(bad).is_err());
    }
    let mut g = Graph::default();
    let q = Record::new("question", "Gate", &format!("[work]({a})"), "alice").unwrap();
    g.records.insert(q.id.clone(), q.clone());
    policy::extract_links(&mut g, &q.id, "alice", &scrolls::beads::Local::default());
    assert_eq!(g.edges.len(), 1);
    assert!(policy::lint(&g, &scrolls::beads::Local::default()).is_empty());
    g.add_edge(&q.id, "blocks", a, "", "alice");
    policy::extract_links(&mut g, &q.id, "alice", &scrolls::beads::Local::default());
    assert_eq!(g.edges.len(), 2);
    g.add_edge(&q.id, "implemented_by", b, "", "alice");
    assert!(!policy::lint(&g, &scrolls::beads::Local::default()).is_empty());
}

#[test]
fn bead_context_is_direct_references_including_notes_and_history() {
    let target = "beads://00000000-0000-4000-8000-000000000001/bd-a";
    let mut g = Graph::default();
    for (kind, status) in [("note", "recorded"), ("knowledge", "retired")] {
        let mut r = Record::new(kind, kind, "body", "alice").unwrap();
        r.status = status.into();
        g.add_edge(&r.id, "related_work", target, "", "alice");
        g.records.insert(r.id.clone(), r);
    }
    let data = query::bead_context(&g, target, 16000).unwrap();
    assert_eq!(data["items"].as_array().unwrap().len(), 2);
    assert!(
        data["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v.get("body").is_none())
    );
    assert_eq!(
        query::bead_context(&g, target, 2).unwrap()["items"],
        serde_json::json!([])
    );
}

#[test]
fn format_one_beads_is_the_work_target() {
    use scrolls::{
        beads::{Binding, Local},
        mutation,
        schema::Schema,
    };
    let schema = Schema::parse(include_str!("fixtures/knowledge-format-1.yaml")).unwrap();
    let revision = schema.revision();
    let stored = serde_json::to_string(&schema).unwrap();
    let loaded: Schema = serde_json::from_str(&stored).unwrap();
    assert_eq!(loaded.revision(), revision);
    assert_eq!(loaded.format, 1);
    let target = "beads://00000000-0000-4000-8000-000000000001/bd-a";
    let mut local = Local::default();
    local.bindings.insert(
        "work".into(),
        Binding {
            project_id: "00000000-0000-4000-8000-000000000001".into(),
            directory: Default::default(),
            bd: Default::default(),
            timeout: 1,
        },
    );
    // Both ontology names resolve the same local binding and canonical target.
    for schema in [loaded, Schema::default()] {
        let mut g = Graph {
            schema,
            ..Default::default()
        };
        let note = g
            .schema
            .new_record("note", "Work", &format!("[work]({target})"), "alice")
            .unwrap();
        let id = note.id.clone();
        g.records.insert(id.clone(), note);
        policy::extract_links(&mut g, &id, "alice", &scrolls::beads::Local::default());
        assert_eq!(g.edges.len(), 1);
        let edge = g.edges.values().next().unwrap();
        assert_eq!(edge.rel, "related_work");
        assert_eq!(edge.to, target);
        policy::edge_valid(&g, edge, &scrolls::beads::Local::default()).unwrap();
        g.edges.clear();
        let ops = mutation::parse(
            &serde_json::json!([
                {"op":"link", "from":id, "rel":"related_work", "to":"work:bd-a", "expect":1}
            ])
            .to_string(),
        )
        .unwrap();
        let (after, _, _) = mutation::apply(&g, &ops, "alice", &local).unwrap();
        assert_eq!(after.edges.len(), 1);
        assert_eq!(after.edges.values().next().unwrap().to, target);
        assert_eq!(after.schema.revision(), g.schema.revision());
        assert!(policy::lint(&after, &scrolls::beads::Local::default()).is_empty());
    }
}

#[test]
fn work_target_uses_the_supplied_adapter() {
    use scrolls::{mutation, read, work_ref::Adapter};
    use serde_json::{Value, json};

    struct Example;
    impl Adapter for Example {
        fn canonicalize(&self, reference: &str) -> Option<anyhow::Result<String>> {
            let id = reference
                .strip_prefix("example://")
                .or_else(|| reference.strip_prefix("old-example://"))?;
            Some((|| {
                anyhow::ensure!(
                    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()),
                    "invalid example reference"
                );
                Ok(format!("example://{id}"))
            })())
        }
        fn resolve_local(&self, input: &str) -> Option<anyhow::Result<String>> {
            self.canonicalize(&format!("example://{}", input.strip_prefix("job:")?))
        }
        fn summary(&self, reference: &str) -> Value {
            json!({"reference":reference,"title":"Example work","status":"open"})
        }
    }
    let work = Example;
    let mut g = Graph::default();
    assert_eq!(g.schema.format, 2);
    let r = Record::new("note", "Work", "", "alice").unwrap();
    let id = r.id.clone();
    g.records.insert(id.clone(), r);
    let link = |target: &str| mutation::Mutation::Link {
        from: id.clone(),
        rel: "related_work".into(),
        to: target.into(),
        why: String::new(),
        expect: Some(1),
        expect_to: None,
    };
    for input in ["example://a", "old-example://a", "job:a"] {
        let (after, _, _) = mutation::apply(&g, &[link(input)], "alice", &work).unwrap();
        let edge = after.edges.values().next().unwrap();
        assert_eq!(edge.to, "example://a");
        policy::edge_valid(&after, edge, &work).unwrap();
        assert!(policy::lint(&after, &work).is_empty());
    }
    assert!(mutation::apply(&g, &[link("example://")], "alice", &work).is_err());
    assert!(
        mutation::apply(
            &g,
            &[link("beads://00000000-0000-4000-8000-000000000001/bd-a")],
            "alice",
            &work
        )
        .is_err()
    );

    // Stored aliases are refused even though link-time input accepts them.
    let mut stored = g.clone();
    stored.add_edge(&id, "related_work", "old-example://a", "", "alice");
    assert!(policy::edge_valid(&stored, stored.edges.values().next().unwrap(), &work).is_err());
    assert!(policy::validate_change(&g, &stored, &Default::default(), &work).is_err());

    let edit = |body: &str| mutation::Mutation::Edit {
        id: id.clone(),
        expect: Some(1),
        title: None,
        body: Some(body.into()),
        owner: None,
        fields: Default::default(),
        confidence: None,
        review_every: None,
        last_reviewed: None,
    };
    let (after, _, _) =
        mutation::apply(&g, &[edit("[work](example://a)")], "alice", &work).unwrap();
    assert_eq!(after.edges.values().next().unwrap().rel, "related_work");
    assert_eq!(after.edges.values().next().unwrap().to, "example://a");
    assert!(mutation::apply(&g, &[edit("[work](example://)")], "alice", &work).is_err());
    let (aliased, _, _) =
        mutation::apply(&g, &[edit("[work](old-example://a)")], "alice", &work).unwrap();
    assert_eq!(aliased.edges.values().next().unwrap().to, "example://a");
    let routing = json!({});
    let view = read::View {
        graph: &after,
        routing: &routing,
        work: &work,
        store_id: None,
        actor: "alice",
    };
    let document = view
        .execute(read::Operation::Document {
            id: &id,
            provenance: false,
        })
        .unwrap();
    assert_eq!(
        document["related_work"][0]["bead"],
        work.summary("example://a")
    );
    let packet = query::bead_packet(&after, "example://a", &work, 16000).unwrap();
    assert_eq!(packet["items"][0]["bead"], work.summary("example://a"));
    assert_eq!(packet["items"][1]["id"], id);

    // Cardinality lint is independent of the work scheme.
    let mut knowledge = Record::new("knowledge", "Finding", "", "alice").unwrap();
    knowledge.status = "current".into();
    let knowledge_id = knowledge.id.clone();
    stored.records.insert(knowledge_id.clone(), knowledge);
    stored.add_edge(&knowledge_id, "implemented_by", "example://a", "", "alice");
    assert!(policy::lint(&stored, &work).contains(&format!("{knowledge_id}:missing-support")));
}

#[test]
fn work_relations_are_selected_by_the_ontology() {
    use scrolls::{beads, schema::Schema, work_ref};
    let target = "beads://00000000-0000-4000-8000-000000000001/bd-a";
    for schema in [
        Schema::default(),
        Schema::parse(include_str!("fixtures/knowledge-format-1.yaml")).unwrap(),
    ] {
        let mut g = Graph {
            schema,
            ..Default::default()
        };
        let rule = g.schema.relations.remove("related_work").unwrap();
        g.schema.relations.insert("tracks_work".into(), rule);
        // A familiar relation name with a different external type carries no work.
        g.schema
            .relations
            .get_mut("implemented_by")
            .unwrap()
            .external = vec!["url".into()];
        for relation in g.schema.body_links.values_mut() {
            if relation == "related_work" {
                *relation = "tracks_work".into();
            }
        }
        g.schema.validate().unwrap();
        assert!(work_ref::relation(&g.schema, "tracks_work"));
        assert!(!work_ref::relation(&g.schema, "implemented_by"));
        let r = Record::new(
            "note",
            "Custom work relation",
            &format!("[work]({target})"),
            "alice",
        )
        .unwrap();
        let id = r.id.clone();
        g.records.insert(id.clone(), r);
        policy::extract_links(&mut g, &id, "alice", &beads::Local::default());
        assert!(policy::lint(&g, &beads::Local::default()).is_empty());
        let refs = beads::refs(&g, target);
        assert_eq!(refs[0]["relations"], serde_json::json!(["tracks_work"]));
        assert_eq!(
            work_ref::related(&g, &id, &beads::Local::default())[0]["relation"],
            "tracks_work"
        );
    }
}

#[test]
fn graph_write_checks_schema_pins_and_changed_rules_with_row_identities() {
    let work = scrolls::beads::Local::default();
    let mut before = Graph::default();
    let record = Record::new("knowledge", "historical debt", "body", "alice").unwrap();
    before.records.insert(record.id.clone(), record.clone());
    // The same rule on the same record can remain in debt during an unrelated write.
    policy::validate_change(&before, &before, &Default::default(), &work).unwrap();
    let mut after = before.clone();
    after.schema.kinds.get_mut("knowledge").unwrap().cardinality[0].min += 1;
    after.schema.pins.push("sc-missing-pin".into());
    after.schema.kinds.get_mut("evidence").unwrap().initial = "missing-state".into();
    let error = policy::validate_change(&before, &after, &Default::default(), &work).unwrap_err();
    let refusal = error.downcast_ref::<policy::WriteRefusal>().unwrap();
    assert!(
        refusal
            .violations
            .contains(&format!("{}:missing-support", record.id))
    );
    assert!(refusal.violations.contains("ontology:pin:sc-missing-pin"));
    assert!(
        refusal
            .violations
            .iter()
            .any(|v| v.starts_with("ontology:schema:"))
    );
    let mut after = before.clone();
    let second = Record::new("knowledge", "new violation", "body", "alice").unwrap();
    after.records.insert(second.id.clone(), second.clone());
    let error = policy::validate_change(&before, &after, &Default::default(), &work).unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<policy::WriteRefusal>()
            .unwrap()
            .violations,
        [format!("{}:missing-support", second.id)].into()
    );
}

#[test]
fn a_historical_field_failure_does_not_hide_a_new_field_rule() {
    let work = scrolls::beads::Local::default();
    let mut before = Graph::default();
    let record = Record::new("evidence", "historical", "body", "alice").unwrap();
    before.records.insert(record.id.clone(), record.clone());
    let required =
        serde_yaml::from_str::<scrolls::schema::Field>("type: string\nrequired: true").unwrap();
    before
        .schema
        .kinds
        .get_mut("evidence")
        .unwrap()
        .fields
        .insert("aaa".into(), required.clone());
    let mut after = before.clone();
    after
        .schema
        .kinds
        .get_mut("evidence")
        .unwrap()
        .fields
        .insert("bbb".into(), required);
    assert_eq!(
        policy::lint(&after, &work),
        [format!("{}:required-field:aaa", record.id)].into()
    );
    let error = policy::validate_change(&before, &after, &Default::default(), &work).unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<policy::WriteRefusal>()
            .unwrap()
            .violations,
        [format!("{}:required-field:bbb", record.id)].into()
    );
}

#[test]
fn builtin_debt_cannot_hide_a_new_invariant_violation() {
    use serde_json::json;
    let work = scrolls::beads::Local::default();
    let mut before = Graph::default();
    let mut record = before
        .schema
        .new_record("evidence", "Legacy", "", "alice")
        .unwrap();
    record.id = "sc-legacy".into();
    record.status = "bogus".into();
    before.records.insert(record.id.clone(), record);
    let target = before
        .schema
        .new_record("knowledge", "Target", "", "alice")
        .unwrap();
    let target_id = target.id.clone();
    before.records.insert(target_id.clone(), target);
    // Exercise the write check on a caller-supplied schema, bypassing parse.
    before.schema.invariants.push(
        serde_json::from_value(json!({
            "name":"invalid-status", "description":"No outgoing support", "kinds":["evidence"],
            "require":{"op":"relation","relation":"supports","direction":"outgoing","max":0}
        }))
        .unwrap(),
    );
    let mut after = before.clone();
    after.add_edge("sc-legacy", "supports", &target_id, "", "alice");
    assert!(policy::lint(&before, &work).contains("sc-legacy:invalid-status"));
    assert!(policy::lint(&after, &work).contains("sc-legacy:invalid-status"));
    let error = format!(
        "{:#}",
        policy::validate_change(&before, &after, &Default::default(), &work).unwrap_err()
    );
    assert!(error.contains("invariant invalid-status"), "{error}");
    assert!(error.contains("built-in lint rule"), "{error}");
}

#[test]
fn invariant_tolerance_uses_definition_and_fails_closed_on_evaluation_errors() {
    use serde_json::json;
    let work = scrolls::beads::Local::default();
    let mut before = Graph::default();
    let mut value = serde_json::to_value(&before.schema).unwrap();
    value["invariants"] = json!([{"name":"has-score", "description":"A score of zero is valid", "kinds":["evidence"],
        "require":{"op":"compare","field":"fields.score","cmp":"ge","value":0}}]);
    before.schema = scrolls::schema::Schema::parse(&value.to_string()).unwrap();
    let mut record = before
        .schema
        .new_record("evidence", "Legacy", "", "alice")
        .unwrap();
    record.id = "sc-legacy".into();
    before.records.insert(record.id.clone(), record.clone());
    assert!(policy::lint(&before, &work).contains("sc-legacy:has-score"));
    let mut after = before.clone();
    let unrelated = after
        .schema
        .new_record("note", "Unrelated", "", "alice")
        .unwrap();
    after.records.insert(unrelated.id.clone(), unrelated);
    policy::validate_change(&before, &after, &Default::default(), &work).unwrap();
    after.schema.invariants[0].description = "Changed definition".into();
    assert!(policy::validate_change(&before, &after, &Default::default(), &work).is_err());
    let mut repaired = before.clone();
    repaired.records.get_mut("sc-legacy").unwrap().fields["score"] = json!(0);
    assert!(!policy::lint(&repaired, &work).contains("sc-legacy:has-score"));
    assert!(policy::validate_change(&repaired, &before, &Default::default(), &work).is_err());
    // A failing selection expression must not silently exclude a record.
    before.schema.invariants[0].when = Some(before.schema.invariants[0].require.clone());
    before.schema.invariants[0].require =
        serde_json::from_value(json!({"op":"all","args":[]})).unwrap();
    assert!(policy::lint(&before, &work).contains("sc-legacy:has-score"));
    before.schema.invariants[0].skip_frozen = true;
    before.records.get_mut("sc-legacy").unwrap().frozen = true;
    assert!(!policy::lint(&before, &work).contains("sc-legacy:has-score"));
}
