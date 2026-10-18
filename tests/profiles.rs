use scrolls::{
    model::*,
    profile::{self, Profile},
    schema::Schema,
    source_import::*,
};
use serde_json::json;
use std::{collections::BTreeMap, path::Path, process::Command};
#[test]
fn strict_configuration_and_codecs() {
    let s = Schema::default();
    Profile::parse(profile::PEP, &s).unwrap();
    Profile::parse(profile::HANDBOOK, &s).unwrap();
    for bad in [
        "a: 1\na: 2",
        "a: &a hi\nb: *a",
        "a: !custom hi",
        "a: {<<: {b: 1}}",
        "a: [",
    ] {
        assert!(profile::strict_yaml(bad).is_err(), "{bad}");
    }
    assert!(profile::strict_yaml(&format!("{}0{}", "[".repeat(65), "]".repeat(65))).is_err());
    let r = profile::decode(
        "rst_headers",
        "pep.rst",
        b"PEP: 0008\r\nTitle: Hello\r\n  world\r\nUnknown: zero\r\n\r\nBody\r\n",
    )
    .unwrap();
    assert_eq!(r.document["headers"]["title"], "Hello world");
    assert_eq!(r.body, "Body\r\n");
    assert_eq!(r.spans.len(), 4);
    for bad in [
        "Title: x\nTITLE: y\n\nbody",
        " orphan\n\nbody",
        "bad\n\nbody",
        "Title: x",
    ] {
        assert!(profile::decode("rst_headers", "x.rst", bad.as_bytes()).is_err());
    }
    for bad in ["---\nx: 1\nx: 2\n---\nx", "---\n[]\n---\nx", "---\nx: 1"] {
        assert!(profile::decode("markdown_frontmatter", "x.md", bad.as_bytes()).is_err());
    }
    let d = profile::decode(
        "markdown_frontmatter",
        "a.md",
        "---\ntitle: 世界\n---\nBody\n".as_bytes(),
    )
    .unwrap();
    assert_eq!(d.metadata["title"], "世界");
    assert_eq!(d.body, "Body\n");
    let p = Profile::parse(profile::HANDBOOK, &s).unwrap();
    let mut p = serde_json::to_value(p).unwrap();
    p["documents"][0]["values"]["flag"] = json!({"from":"/frontmatter/flag","default":true});
    let p = Profile::parse(&p.to_string(), &s).unwrap();
    for flag in [json!(null), json!(false), json!(0), json!("")] {
        let doc = json!({"frontmatter":{"id":"a","title":"Title","flag":flag}});
        assert_eq!(p.documents[0].values(&doc, "source").unwrap()["flag"], flag);
    }
    assert_eq!(
        p.documents[0]
            .values(&json!({"frontmatter":{"id":"a","title":"T"}}), "source")
            .unwrap()["flag"],
        true
    );
    let p = Profile::parse(profile::PEP, &s).unwrap();
    let mut d = json!({"headers":{"pep":"0008","title":" T ","status":"Draft","type":"Standards Track","author":"A, B","created":"now"}});
    assert_eq!(p.documents[0].values(&d, "s").unwrap()["number"], 8);
    d["headers"]["pep"] = json!("18446744073709551616");
    assert!(p.documents[0].values(&d, "s").is_err());
    assert_eq!(
        profile::template(
            "PEP-{number:04d}",
            &BTreeMap::from([("number".into(), json!(8))])
        )
        .unwrap(),
        "PEP-0008"
    );
    assert!(Profile::parse(&profile::PEP.replace("supersedes", "absent"), &s).is_err());
    // Known relations must accept the projected kind even without source targets.
    assert!(Profile::parse(&profile::PEP.replace("supersedes", "answers"), &s).is_err());
    assert!(Profile::parse(&profile::PEP.replace("supersedes", "produced_by"), &s).is_err());
    assert!(Profile::parse(&profile::PEP.replace("{number:04d}", "{number:999d}"), &s).is_err());
}
fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
fn fixture() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    git(t.path(), &["init", "-q"]);
    git(t.path(), &["config", "user.name", "Test"]);
    git(t.path(), &["config", "user.email", "test@example.org"]);
    std::fs::create_dir(t.path().join("chapters")).unwrap();
    std::fs::write(
        t.path().join("chapters/one.md"),
        "---\nid: one\ntitle: One\ntags: [a]\n---\nBody [untouched](sc-missing)\n",
    )
    .unwrap();
    std::fs::write(t.path().join("README.md"), "# Overview\n").unwrap();
    std::fs::write(
        t.path().join(".gitattributes"),
        "chapters/one.md export-ignore\nREADME.md export-subst\n",
    )
    .unwrap();
    git(t.path(), &["add", "."]);
    git(t.path(), &["commit", "-qm", "fixture"]);
    t
}
#[test]
fn raw_git_identity_references_and_ownership() {
    let t = fixture();
    let g = Graph::default();
    let s = source(
        t.path(),
        ".",
        "HEAD",
        "test",
        profile::HANDBOOK,
        &g.schema,
        "alice",
        "human",
    )
    .unwrap();
    let c = build(&g, &s).unwrap();
    assert_eq!(c.source.coverage["documents"], 2);
    assert_eq!(c.files.len(), 3);
    assert_eq!(
        c.source.candidate_digest,
        build(&g, &s).unwrap().source.candidate_digest
    );
    let id = record_id("test", "chapter/one");
    assert!(c.graph.records[&id].body.contains("sc-missing"));
    assert!(scrolls::policy::lint(&c.graph).is_empty());
    std::fs::write(t.path().join("chapters/one.md"), "broken uncommitted").unwrap();
    assert_eq!(
        c.source.candidate_digest,
        build(&g, &s).unwrap().source.candidate_digest
    );
    git(t.path(), &["checkout", "--", "chapters/one.md"]);
    git(t.path(), &["mv", "chapters/one.md", "chapters/renamed.md"]);
    git(t.path(), &["commit", "-qm", "rename"]);
    let renamed = source(
        t.path(),
        ".",
        "HEAD",
        "test",
        profile::HANDBOOK,
        &g.schema,
        "alice",
        "human",
    )
    .unwrap();
    assert!(build(&g, &renamed).unwrap().graph.records.contains_key(&id));
    let mut modified = c.graph.clone();
    modified.records.get_mut(&id).unwrap().body.push('x');
    assert!(ownership(&c.graph, &modified).is_err());
    let mut mechanical = c.graph.clone();
    mechanical.records.get_mut(&id).unwrap().revision += 1;
    mechanical.records.get_mut(&id).unwrap().actor = "bob".into();
    ownership(&c.graph, &mechanical).unwrap();
    let overlap = profile::HANDBOOK.replace("include: [README.md]", "include: ['**/*.md']");
    let s = source(
        t.path(),
        ".",
        "HEAD",
        "other",
        &overlap,
        &g.schema,
        "alice",
        "human",
    )
    .unwrap();
    assert!(build(&g, &s).unwrap_err_text().contains("ambiguous_rule"));
}
trait ErrText {
    fn unwrap_err_text(self) -> String;
}
impl<T> ErrText for anyhow::Result<T> {
    fn unwrap_err_text(self) -> String {
        match self {
            Err(e) => format!("{e:#}"),
            Ok(_) => panic!("expected error"),
        }
    }
}

#[test]
fn reference_occurrences_collapse_and_imported_edges_remain_owned() {
    let t = fixture();
    for (n, refs) in [(1, "Replaces: 2\nRequires: 2\n"), (2, "Superseded-By: 1\n")] {
        std::fs::write(t.path().join(format!("pep-{n:04}.rst")),format!("PEP: {n}\nTitle: PEP {n}\nStatus: Draft\nType: Standards Track\nAuthor: A\nCreated: Today\n{refs}\nBody\n")).unwrap();
    }
    git(t.path(), &["add", "."]);
    git(t.path(), &["commit", "-qm", "peps"]);
    let g = Graph::default();
    let s = source(
        t.path(),
        ".",
        "HEAD",
        "peps",
        profile::PEP,
        &g.schema,
        "alice",
        "human",
    )
    .unwrap();
    let c = build(&g, &s).unwrap();
    assert_eq!(c.source.coverage["header_references"], 3);
    assert_eq!(c.source.coverage["edges"], 1);
    assert_eq!(c.source.coverage["unmapped_references"], 1);
    let mut changed = c.graph.clone();
    changed.edges.clear();
    assert!(ownership(&c.graph, &changed).is_err());
    let edge = c.graph.edges.values().next().unwrap();
    let local = scrolls::beads::Local::default();
    let ops=scrolls::mutation::parse(&json!([{"op":"link","from":edge.from,"rel":edge.rel,"to":edge.to,"expect":1,"expect_to":1}]).to_string()).unwrap();
    let (after, affected, _) = scrolls::mutation::apply(&c.graph, &ops, "bob", &local).unwrap();
    assert!(affected.is_empty());
    assert_eq!(after.edges, c.graph.edges);
    let mut bad = serde_json::to_value(Profile::parse(profile::PEP, &g.schema).unwrap()).unwrap();
    bad["documents"][0]["references"][0]["target_key"] = json!("pep/999");
    bad["documents"][0]["references"][0]["on_missing"] = json!("error");
    let s = source(
        t.path(),
        ".",
        "HEAD",
        "peps",
        &bad.to_string(),
        &g.schema,
        "alice",
        "human",
    )
    .unwrap();
    assert!(
        build(&g, &s)
            .unwrap_err_text()
            .contains("unresolved_reference")
    );
}
#[test]
fn custom_research_projection_and_unsupported_files() {
    let t = fixture();
    let schema = Schema::parse(include_str!("../examples/research.yaml")).unwrap();
    let mut p =
        serde_json::to_value(Profile::parse(profile::HANDBOOK, &Schema::default()).unwrap())
            .unwrap();
    for r in p["documents"].as_array_mut().unwrap() {
        r["record"]["kind"] = json!({"literal":"trial"});
        r["record"]["fields"] = json!({"count":{"literal":0}});
    }
    let g = Graph {
        schema,
        ..Graph::default()
    };
    let s = source(
        t.path(),
        ".",
        "HEAD",
        "research",
        &p.to_string(),
        &g.schema,
        "alice",
        "human",
    )
    .unwrap();
    let c = build(&g, &s).unwrap();
    for r in c.graph.records.values() {
        assert_eq!(r.fields["count"], 0);
        assert_eq!(r.fields["replicated"], false);
        assert_eq!(r.kind, "trial");
    }
    p["documents"][0]["record"]["fields"]["bad"] = json!({"literal":"x"});
    assert!(Profile::parse(&p.to_string(), &g.schema).is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("README.md", t.path().join("link")).unwrap();
        git(t.path(), &["add", "link"]);
        git(t.path(), &["commit", "-qm", "unsupported"]);
        let s = source(
            t.path(),
            ".",
            "HEAD",
            "s",
            profile::HANDBOOK,
            &Schema::default(),
            "alice",
            "human",
        )
        .unwrap();
        assert!(
            build(&Graph::default(), &s)
                .unwrap_err_text()
                .contains("unsupported_file_type")
        );
    }
}

#[test]
fn relocation_empty_sets_and_aliases_precede_beads_shorthand() {
    let t = fixture();
    let g = Graph::default();
    let s = source(
        t.path(),
        ".",
        "HEAD",
        "book",
        profile::HANDBOOK,
        &g.schema,
        "alice",
        "human",
    )
    .unwrap();
    let c = build(&g, &s).unwrap();
    let clone = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .args(["clone", "-q"])
            .arg(t.path())
            .arg(clone.path())
            .status()
            .unwrap()
            .success()
    );
    let relocated = source(
        clone.path(),
        ".",
        "HEAD",
        "book",
        profile::HANDBOOK,
        &g.schema,
        "alice",
        "human",
    )
    .unwrap();
    assert_eq!(
        build(&g, &relocated).unwrap().source.candidate_digest,
        c.source.candidate_digest
    );
    let empty = profile::HANDBOOK
        .replace("chapters/*.md", "absent/*.md")
        .replace("include: [README.md]", "include: [absent.md]");
    let s = source(
        t.path(),
        ".",
        "HEAD",
        "empty",
        &empty,
        &g.schema,
        "alice",
        "human",
    )
    .unwrap();
    assert!(
        build(&g, &s)
            .unwrap_err_text()
            .contains("empty_document_set")
    );
    let mut g = c.graph;
    g.schema
        .relations
        .get_mut("related")
        .unwrap()
        .external
        .push("beads".into());
    let note = g
        .schema
        .new_record("note", "chapter:one", "local", "alice")
        .unwrap();
    let id = note.id.clone();
    g.records.insert(id.clone(), note);
    let ops = scrolls::mutation::parse(
        &json!([{"op":"link","from":id,"rel":"related","to":"chapter:one","expect":1}]).to_string(),
    )
    .unwrap();
    let (after, _, _) =
        scrolls::mutation::apply(&g, &ops, "alice", &scrolls::beads::Local::default()).unwrap();
    assert_eq!(
        after.edges.values().next().unwrap().to,
        record_id("book", "chapter/one")
    );
}
