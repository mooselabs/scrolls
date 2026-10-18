//! Pinned source snapshots, deterministic plans, provenance and ownership.
use crate::{
    cli::*,
    model::*,
    policy,
    profile::{self, Profile},
    storage::{Store, put},
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Command, Stdio},
};
pub const FILE_LIMIT: u64 = 64 * 1024 * 1024;
pub const TOTAL_LIMIT: u64 = 1024 * 1024 * 1024;
pub const FILE_COUNT: usize = 100_000;
pub const CONTRACT: &str = "scrolls.import.v1:profile1:markdown1:rst1:git-raw1";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub source_id: String,
    pub repository: String,
    pub commit: String,
    pub tree: String,
    pub root: String,
    pub profile: String,
    pub profile_hash: String,
    pub compiled_hash: String,
    pub schema_hash: String,
    pub contract: String,
    pub candidate_digest: String,
    pub actor: String,
    pub role: String,
    pub imported_at: String,
    pub op_id: String,
    pub coverage: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct File {
    pub source_id: String,
    pub file: SourceFile,
    pub classification: String,
}
impl File {
    pub fn key(&self) -> String {
        tuple("scrolls.import.file.v1", &self.source_id, &self.file.path)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub source_id: String,
    pub key: String,
    pub record_id: String,
    pub path: String,
    pub codec: String,
    pub rule: String,
    pub metadata: Value,
    pub spans: Vec<Value>,
    pub baseline: Record,
    pub references: Vec<Value>,
    pub edges: BTreeMap<String, Edge>,
}
impl Member {
    pub fn row_key(&self) -> String {
        tuple("scrolls.import.member.v1", &self.source_id, &self.key)
    }
}
fn tuple(version: &str, source: &str, key: &str) -> String {
    digest(serde_json::to_vec(&json!([version, source, key])).unwrap())
}
pub fn record_id(source: &str, key: &str) -> String {
    format!("sc-{}", tuple("scrolls.import.record.v1", source, key))
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub format: u32,
    pub store_id: String,
    pub main_revision: String,
    pub schema_hash: String,
    pub actor: String,
    pub role: String,
    pub source: Source,
    pub limits: Value,
    pub changes: Value,
    pub digest: String,
}
pub fn limits() -> Value {
    json!({"profile_bytes":profile::METADATA_LIMIT,"metadata_bytes":profile::METADATA_LIMIT,"file_bytes":FILE_LIMIT,"total_bytes":TOTAL_LIMIT,"files":FILE_COUNT,"depth":profile::DEPTH_LIMIT})
}
impl Plan {
    pub fn seal(&mut self) -> Result<()> {
        self.digest.clear();
        self.digest = digest(serde_json::to_vec(self)?);
        Ok(())
    }
    pub fn verify(&self) -> Result<()> {
        let mut copy = self.clone();
        copy.seal()?;
        ensure!(
            copy.digest == self.digest
                && self.format == 1
                && self.limits == limits()
                && self.source.contract == CONTRACT,
            "stale_plan: plan integrity or interpreter mismatch; replan"
        );
        ensure!(
            self.actor == self.source.actor
                && self.role == self.source.role
                && self.schema_hash == self.source.schema_hash,
            "stale_plan: inconsistent plan binding"
        );
        Ok(())
    }
}
pub struct Candidate {
    pub graph: Graph,
    pub files: Vec<File>,
    pub source: Source,
}
fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    ensure!(
        out.status.success(),
        "source_read: git failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(out.stdout)
}
fn git_text(repo: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(git(repo, args)?)?.trim().into())
}
pub fn path(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && !path.starts_with('/')
            && !path.contains(['\\', '\0', '\n', '\r'])
            && path
                .split('/')
                .all(|s| !s.is_empty() && s != "." && s != "..")
            && path.split('/').count() <= 64,
        "unsafe_path: {path}; select a safe root-relative path"
    );
    Ok(())
}
pub fn namespace(s: &str) -> Result<()> {
    ensure!(
        !s.is_empty()
            && s.len() <= 64
            && s.bytes().next().unwrap().is_ascii_alphanumeric()
            && s.bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(&c)),
        "invalid_profile: source ID must be 1-64 lowercase ASCII letters, digits, dots, underscores or hyphens"
    );
    Ok(())
}
#[allow(clippy::too_many_arguments)] // Explicit source invocation and attribution, no ambient state.
pub fn source(
    repo: &Path,
    root: &str,
    rev: &str,
    id: &str,
    text: &str,
    schema: &crate::schema::Schema,
    actor: &str,
    role: &str,
) -> Result<Source> {
    namespace(id)?;
    if root != "." {
        path(root)?;
    }
    let repo = repo.canonicalize()?;
    let commit = git_text(
        &repo,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{rev}^{{commit}}"),
        ],
    )?;
    let tree = git_text(
        &repo,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &if root == "." {
                format!("{commit}^{{tree}} ").trim().into()
            } else {
                format!("{commit}:{root}")
            },
        ],
    )?;
    ensure!(
        git_text(&repo, &["cat-file", "-t", &tree])? == "tree",
        "source_read: root must be a Git tree"
    );
    let p = Profile::parse(text, schema)?;
    Ok(Source {
        source_id: id.into(),
        repository: repo
            .to_str()
            .context("source_read: non-UTF8 repository")?
            .into(),
        commit,
        tree,
        root: root.into(),
        profile: text.into(),
        profile_hash: digest(text),
        compiled_hash: p.hash()?,
        schema_hash: schema.revision(),
        contract: CONTRACT.into(),
        candidate_digest: String::new(),
        actor: actor.into(),
        role: role.into(),
        imported_at: String::new(),
        op_id: String::new(),
        coverage: Value::Null,
    })
}
/// Inventory tree entries before requesting any raw blob. Git export attributes are ignored.
fn files(source: &Source, p: &Profile) -> Result<Vec<File>> {
    let repo = Path::new(&source.repository);
    let raw = git(repo, &["ls-tree", "-r", "-z", "-l", &source.tree])?;
    let mut entries = vec![];
    let mut total = 0u64;
    for row in raw.split(|c| *c == 0).filter(|s| !s.is_empty()) {
        let tab = row
            .iter()
            .position(|c| *c == b'\t')
            .context("source_read: invalid tree entry")?;
        let name =
            std::str::from_utf8(&row[tab + 1..]).context("unsafe_path: Git path must be UTF-8")?;
        path(name)?;
        let header = std::str::from_utf8(&row[..tab])?;
        let h: Vec<_> = header.split_whitespace().collect();
        ensure!(
            h.len() == 4 && h[1] == "blob" && ["100644", "100755"].contains(&h[0]),
            "unsupported_file_type: {name}; only regular Git files are supported"
        );
        let size: u64 = h[3].parse()?;
        ensure!(
            size <= FILE_LIMIT,
            "input_limit: {name} exceeds 64 MiB; narrow source root"
        );
        total = total
            .checked_add(size)
            .context("input_limit: size overflow")?;
        ensure!(
            total <= TOTAL_LIMIT,
            "input_limit: source exceeds 1 GiB; split the source"
        );
        ensure!(
            entries.len() < FILE_COUNT,
            "input_limit: source exceeds 100000 files; split the source"
        );
        entries.push((
            name.to_string(),
            h[2].to_string(),
            u32::from_str_radix(h[0], 8)?,
            size,
        ));
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let outcome = (|| -> Result<Vec<File>> {
        let mut input = child.stdin.take().context("source_read: batch stdin")?;
        let mut output = BufReader::new(child.stdout.take().context("source_read: batch stdout")?);
        let mut out = vec![];
        for (name, oid, mode, size) in entries {
            writeln!(input, "{oid}")?;
            input.flush()?;
            let mut header = String::new();
            output.read_line(&mut header)?;
            ensure!(
                header.trim() == format!("{oid} blob {size}"),
                "source_read: unexpected blob response for {name}"
            );
            let mut bytes = vec![0; size as usize];
            output.read_exact(&mut bytes)?;
            let mut newline = [0];
            output.read_exact(&mut newline)?;
            ensure!(newline == *b"\n", "source_read: invalid blob separator");
            let classification = if p.select(&name)?.is_some() {
                "document"
            } else {
                "asset"
            };
            out.push(File {
                source_id: source.source_id.clone(),
                file: SourceFile {
                    path: name,
                    sha256: digest(&bytes),
                    git_oid: oid,
                    mode,
                    bytes: STANDARD.encode(&bytes),
                },
                classification: classification.into(),
            });
        }
        drop(input);
        Ok(out)
    })();
    if outcome.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let out = outcome?;
    ensure!(status.success(), "source_read: cat-file failed");
    Ok(out)
}
pub fn build(before: &Graph, source: &Source) -> Result<Candidate> {
    ensure!(
        source.schema_hash == before.schema.revision() && source.contract == CONTRACT,
        "stale_plan: schema or interpreter changed; replan"
    );
    let p = Profile::parse(&source.profile, &before.schema)?;
    ensure!(
        source.profile_hash == digest(&source.profile) && source.compiled_hash == p.hash()?,
        "stale_plan: profile digest mismatch"
    );
    // Re-resolve the immutable object to verify root/tree binding in a supplied plan.
    let pinned = self::source(
        Path::new(&source.repository),
        &source.root,
        &source.commit,
        &source.source_id,
        &source.profile,
        &before.schema,
        &source.actor,
        &source.role,
    )?;
    ensure!(
        pinned.commit == source.commit && pinned.tree == source.tree,
        "stale_plan: source object binding mismatch"
    );
    let files = files(source, &p)?;
    let mut graph = before.clone();
    let mut members = BTreeMap::new();
    let mut values = BTreeMap::new();
    let mut by_rule = BTreeMap::<String, usize>::new();
    let mut by_codec = BTreeMap::<String, usize>::new();
    let mut kinds = BTreeMap::<String, usize>::new();
    let mut assets = BTreeMap::<String, usize>::new();
    let mut ignored = BTreeMap::<String, usize>::new();
    let mut bytes = 0usize;
    let date = git_text(
        Path::new(&source.repository),
        &["show", "-s", "--format=%cI", &source.commit],
    )?;
    for f in &files {
        let raw = STANDARD.decode(&f.file.bytes)?;
        bytes += raw.len();
        let Some(rule) = p.select(&f.file.path)? else {
            *assets
                .entry(
                    Path::new(&f.file.path)
                        .extension()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .into(),
                )
                .or_default() += 1;
            continue;
        };
        let decoded = profile::decode(&rule.codec, &f.file.path, &raw).with_context(|| {
            format!(
                "parse_error: path={} rule={}; repair matched document",
                f.file.path, rule.id
            )
        })?;
        let v = rule
            .values(&decoded.document, &source.source_id)
            .with_context(|| format!("parse_error: path={} rule={}", f.file.path, rule.id))?;
        let location = |field: &str| format!("path={} rule={} field={field}", f.file.path, rule.id);
        let key = rule
            .identity
            .key
            .text(&v)
            .with_context(|| location("identity.key"))?;
        ensure!(
            !members.contains_key(&key),
            "duplicate_identity: {} key={key}; use distinct keys",
            f.file.path
        );
        let id = record_id(&source.source_id, &key);
        ensure!(
            !graph.records.contains_key(&id),
            "duplicate_identity: existing record {id}; sources cannot adopt records"
        );
        let kind = rule
            .record
            .kind
            .text(&v)
            .with_context(|| location("kind"))?;
        let title = rule
            .record
            .title
            .text(&v)
            .with_context(|| location("title"))?;
        let mut r =
            graph
                .schema
                .new_record(&kind, &title, &rule.body(&decoded.body), &source.actor)?;
        r.id = id.clone();
        r.created = date.clone();
        if !graph
            .schema
            .envelope_fields
            .get("last_reviewed")
            .is_some_and(|f| f.default.is_some())
        {
            r.last_reviewed = date.clone();
        }
        if let Some(e) = &rule.record.owner {
            r.owner = e.text(&v).with_context(|| location("owner"))?;
        }
        for (f, e) in &rule.record.fields {
            if let Some(value) = e.eval(&v).with_context(|| location(f))? {
                r.fields[f] = value;
            }
        }
        let mut aliases = BTreeSet::from([
            format!("{}:{key}", source.source_id),
            format!("{}:{}", source.source_id, f.file.path),
        ]);
        for e in &rule.identity.aliases {
            let a = e.text(&v).with_context(|| location("aliases"))?;
            aliases.insert(format!("{}:{a}", source.source_id));
            aliases.insert(a);
        }
        r.aliases = aliases.into_iter().collect();
        graph
            .schema
            .validate_record(&r)
            .with_context(|| format!("schema_mismatch: path={} rule={}", f.file.path, rule.id))?;
        if let Some(meta) = decoded.metadata.as_object() {
            for k in meta.keys() {
                let pointer = format!(
                    "/{}/{}",
                    if rule.codec == "rst_headers" {
                        "headers"
                    } else {
                        "frontmatter"
                    },
                    k.replace('~', "~0").replace('/', "~1")
                );
                if !rule.values.values().any(|s| s.from == pointer) {
                    *ignored.entry(k.clone()).or_default() += 1;
                }
            }
        }
        *by_rule.entry(rule.id.clone()).or_default() += 1;
        *by_codec.entry(rule.codec.clone()).or_default() += 1;
        *kinds.entry(kind).or_default() += 1;
        members.insert(
            key.clone(),
            Member {
                source_id: source.source_id.clone(),
                key: key.clone(),
                record_id: id.clone(),
                path: f.file.path.clone(),
                codec: rule.codec.clone(),
                rule: rule.id.clone(),
                metadata: decoded.metadata,
                spans: decoded.spans,
                baseline: r.clone(),
                references: vec![],
                edges: BTreeMap::new(),
            },
        );
        values.insert(key, v);
        graph.records.insert(id, r);
    }
    ensure!(
        members.len() >= p.coverage.min_documents,
        "empty_document_set: mapped {} documents; need at least {}; check root and selectors",
        members.len(),
        p.coverage.min_documents
    );
    for (rule, min) in &p.coverage.min_by_rule {
        ensure!(
            by_rule.get(rule).copied().unwrap_or(0) >= *min,
            "empty_document_set: rule {rule} needs {min} documents"
        );
    }
    let ids: BTreeMap<_, _> = members
        .iter()
        .map(|(k, m)| (k.clone(), m.record_id.clone()))
        .collect();
    let (mut mapped, mut unmapped, mut unresolved) = (0, 0, 0);
    let mut diagnostics = vec![];
    for (key, m) in &mut members {
        let rule = p.documents.iter().find(|r| r.id == m.rule).unwrap();
        for assertion in &rule.references {
            let Some(v) = values[key].get(&assertion.value) else {
                continue;
            };
            let targets = v.as_array().cloned().unwrap_or_else(|| vec![v.clone()]);
            for target in targets {
                let target_key = profile::template(
                    &assertion.target_key,
                    &BTreeMap::from([("target".into(), target)]),
                )?;
                let target_id = ids.get(&target_key);
                if target_id.is_none() {
                    unresolved += 1;
                    ensure!(
                        assertion.on_missing != "error",
                        "unresolved_reference: {} {} target {target_key}; repair target or retain explicitly",
                        m.path,
                        assertion.source_relation
                    );
                    diagnostics.push(json!({"code":"unresolved_reference","severity":"warning","path":m.path,"rule":m.rule,"field":assertion.source_relation,"reason":target_key,"remedy":"supply target in source or retain assertion"}));
                }
                if assertion.relation.is_some() {
                    mapped += 1
                } else {
                    unmapped += 1
                };
                let mut occurrence = json!({"rule":assertion.id,"source_relation":assertion.source_relation,"target_key":target_key,"target_id":target_id,"direction":assertion.direction,"relation":assertion.relation,"path":m.path,"commit":source.commit});
                if let (Some(rel), Some(to)) = (&assertion.relation, target_id) {
                    let (from, to) = if assertion.direction == "incoming" {
                        (to.clone(), m.record_id.clone())
                    } else {
                        (m.record_id.clone(), to.clone())
                    };
                    let e = Edge {
                        from,
                        rel: rel.clone(),
                        to,
                        why: format!(
                            "source {} {}@{}",
                            assertion.source_relation, m.path, source.commit
                        ),
                        actor: source.actor.clone(),
                        created: date.clone(),
                    };
                    let edge_key = e.key();
                    let e = graph.edges.entry(edge_key.clone()).or_insert(e).clone();
                    m.edges.insert(edge_key.clone(), e);
                    occurrence["edge_key"] = json!(edge_key);
                }
                m.references.push(occurrence);
            }
        }
    }
    let aliases = members
        .values()
        .map(|m| m.baseline.aliases.len())
        .sum::<usize>();
    for m in members.into_values() {
        graph.import_members.insert(m.record_id.clone(), m);
    }
    // A temporary manifest allows namespace and ownership validation before persistence.
    graph
        .import_sources
        .insert(source.source_id.clone(), source.clone());
    validate(&graph)?;
    policy::validate_change(before, &graph, &BTreeSet::new())?;
    let protected = graph
        .import_members
        .values()
        .filter(|m| m.source_id == source.source_id)
        .any(|m| graph.schema.protected(&m.baseline, &source.role))
        || graph
            .schema
            .guards(before, &graph, &source.actor, &source.role)
            .is_err();
    let coverage = json!({"files":files.len(),"bytes":bytes,"documents":ids.len(),"documents_by_rule":by_rule,"documents_by_codec":by_codec,"kinds":kinds,"assets_by_extension":assets,"ignored_metadata":ignored,"aliases":aliases,"mapped_references":mapped,"unmapped_references":unmapped,"unresolved_references":unresolved,"header_references":mapped+unmapped,"edges":graph.edges.len()-before.edges.len(),"review_required":protected,"reference_coverage":"declared metadata only; body links and RST roles are not extracted","diagnostics":diagnostics});
    let content = json!({"members":graph.import_members.values().filter(|m|m.source_id==source.source_id).collect::<Vec<_>>(),"files":files,"coverage":coverage});
    let mut source = source.clone();
    source.coverage = coverage;
    source.candidate_digest = digest(serde_json::to_vec(&content)?);
    graph
        .import_sources
        .insert(source.source_id.clone(), source.clone());
    Ok(Candidate {
        graph,
        files,
        source,
    })
}
pub fn captured(r: &Record) -> Record {
    let mut r = r.clone();
    r.revision = 0;
    r.actor.clear();
    r
}
pub fn validate(g: &Graph) -> Result<()> {
    let mut per_source = BTreeMap::<(String, String), String>::new();
    for (id, m) in &g.import_members {
        ensure!(
            id == &m.record_id
                && m.record_id == record_id(&m.source_id, &m.key)
                && g.import_sources.contains_key(&m.source_id),
            "source_ownership: invalid membership {id}"
        );
        let r = g
            .records
            .get(id)
            .context("source_ownership: captured record missing")?;
        ensure!(
            captured(r) == captured(&m.baseline),
            "source_ownership: {id} captured properties cannot be modified; create a local note"
        );
        for (k, e) in &m.edges {
            ensure!(
                k == &e.key() && g.edges.get(k) == Some(e),
                "source_ownership: imported assertion {k} cannot be modified or removed"
            );
        }
        for a in &r.aliases {
            ensure!(
                !g.records.contains_key(a) || a == id,
                "duplicate_alias: alias shadows record ID {a}"
            );
            if let Some(prior) = per_source.insert((m.source_id.clone(), a.clone()), id.clone()) {
                ensure!(prior == *id, "duplicate_alias: {}:{a}", m.source_id);
            }
            if a.starts_with(&format!("{}:", m.source_id)) {
                for other in g.records.values().filter(|r| r.id != *id) {
                    ensure!(
                        !other.aliases.contains(a),
                        "duplicate_alias: qualified alias {a} shadows {}",
                        other.id
                    );
                }
            }
        }
    }
    for (k, s) in &g.import_sources {
        ensure!(k == &s.source_id, "source_ownership: invalid source key");
        namespace(k)?;
    }
    Ok(())
}
pub fn ownership(before: &Graph, after: &Graph) -> Result<()> {
    for (id, m) in &before.import_members {
        ensure!(
            after.import_members.get(id) == Some(m),
            "source_ownership: membership cannot be changed"
        );
    }
    for (id, s) in &before.import_sources {
        ensure!(
            after.import_sources.get(id) == Some(s),
            "source_update_unsupported: accepted source cannot change"
        );
    }
    validate(after)
}
pub fn provenance(g: &Graph, id: &str) -> Value {
    g.import_members
        .get(id)
        .map(|m| json!({"membership":m,"source":g.import_sources.get(&m.source_id)}))
        .unwrap_or(Value::Null)
}
pub fn summary(g: &Graph, files: &[File]) -> Value {
    json!({"sources":g.import_sources,"members":g.import_members,"files":files.iter().map(|f|json!({"source_id":f.source_id,"path":f.file.path,"sha256":f.file.sha256,"git_oid":f.file.git_oid,"mode":f.file.mode,"classification":f.classification})).collect::<Vec<_>>()})
}
pub fn write_plan(out: &Path, plan: &Plan) -> Result<()> {
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut f = tempfile::NamedTempFile::new_in(parent)?;
    f.write_all(&serde_json::to_vec_pretty(plan)?)?;
    f.as_file().sync_all()?;
    f.persist_noclobber(out)
        .context("plan destination must be fresh")?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
pub fn persist(candidate: &Candidate) -> Result<String> {
    let mut sql = put(
        "import_sources",
        &candidate.source.source_id,
        &candidate.source,
    )?;
    for f in &candidate.files {
        sql.push_str(&put("import_files", &f.key(), f)?);
    }
    for m in candidate
        .graph
        .import_members
        .values()
        .filter(|m| m.source_id == candidate.source.source_id)
    {
        sql.push_str(&put("import_members", &m.row_key(), m)?);
    }
    Ok(sql)
}
pub fn same(a: &Source, b: &Source) -> bool {
    a.source_id == b.source_id
        && a.commit == b.commit
        && a.tree == b.tree
        && a.root == b.root
        && a.profile_hash == b.profile_hash
        && a.compiled_hash == b.compiled_hash
        && a.schema_hash == b.schema_hash
        && a.contract == b.contract
}
/// Handle import profile commands before the ordinary opener, keeping planning read-only.
pub fn run(cli: &Cli, command: &ImportCommand) -> Result<Value> {
    let envelope = |data: Value| json!({"schema_version":2,"ok":true,"data":data});
    if let ImportCommand::Profiles { command } = command {
        match command {
            ProfileCommand::List => {
                return Ok(envelope(
                    json!({"profiles":[{"name":"pep","description":"Python Enhancement Proposals"},{"name":"handbook","description":"Markdown handbook"}]}),
                ));
            }
            ProfileCommand::Show { name } => {
                return Ok(envelope(
                    json!({"name":name,"yaml":profile::bundled(name)?}),
                ));
            }
            ProfileCommand::Validate { file } => {
                let s = Store::open_readonly(&cli.store, &cli.dolt)?;
                let rev = s.head("main")?;
                let g = s.graph_at(&rev)?;
                let text = read_profile(file)?;
                let p = Profile::parse(&text, &g.schema)?;
                return Ok(envelope(
                    json!({"valid":true,"profile":p,"schema_revision":g.schema.revision(),"compiled_hash":p.hash()?}),
                ));
            }
        }
    }
    ensure!(
        cli.proposal.is_none(),
        "import only operates on main; omit --proposal"
    );
    let actor = cli
        .actor
        .as_deref()
        .context("planning/apply requires --actor")?;
    ensure!(
        !actor.trim().is_empty() && !actor.contains(['\n', '\r', '<', '>']),
        "invalid actor"
    );
    match command {
        ImportCommand::Plan {
            command:
                PlanCommand::Git {
                    repo,
                    root,
                    rev,
                    source: source_id,
                    profile: profile_name,
                    profile_file,
                    out,
                },
        } => {
            let text = if let Some(name) = profile_name {
                profile::bundled(name)?.to_string()
            } else {
                read_profile(
                    profile_file
                        .as_ref()
                        .context("select --profile or --profile-file")?,
                )?
            };
            let (store_id, main_revision, g) = {
                let s = Store::open_readonly(&cli.store, &cli.dolt)?;
                let rev = s.head("main")?;
                ensure!(
                    s.one_at::<u32>("metadata", "schema", &rev)? == Some(4),
                    "store format 4 required; run migrate"
                );
                (
                    s.one_at::<String>("metadata", "store_id", &rev)?
                        .context("missing store UUID")?,
                    rev.clone(),
                    s.graph_at(&rev)?,
                )
            };
            let source = source(
                repo,
                root,
                rev,
                source_id,
                &text,
                &g.schema,
                actor,
                cli.role.name(),
            )?;
            let mut base = g.clone();
            if let Some(existing) = base.import_sources.get(source_id) {
                ensure!(
                    same(existing, &source),
                    "source_update_unsupported: source {source_id} already has a different snapshot; use an independent namespace"
                );
                remove_source(&mut base, source_id);
            }
            let candidate = build(&base, &source)?;
            let changes = planned_changes(&candidate);
            let mut plan = Plan {
                format: 1,
                store_id,
                main_revision,
                schema_hash: g.schema.revision(),
                actor: actor.into(),
                role: cli.role.name().into(),
                source: candidate.source,
                limits: limits(),
                changes,
                digest: String::new(),
            };
            plan.seal()?;
            let parent = out
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."))
                .canonicalize()?;
            ensure!(
                !parent.starts_with(cli.store.canonicalize()?),
                "invalid_plan_destination: plan must be outside the store"
            );
            write_plan(out, &plan)?;
            Ok(envelope(
                json!({"plan":out,"digest":plan.digest,"candidate_digest":plan.source.candidate_digest,"coverage":plan.source.coverage,"source":plan.source.source_id,"main_revision":plan.main_revision}),
            ))
        }
        ImportCommand::Apply { plan: file } => {
            ensure!(
                fs::metadata(file)?.len() <= TOTAL_LIMIT,
                "input_limit: plan exceeds 1 GiB"
            );
            let bytes = fs::read(file)?;
            let plan: Plan = serde_json::from_slice(&bytes).context("stale_plan: invalid plan")?;
            plan.verify()?;
            ensure!(
                actor == plan.actor && cli.role.name() == plan.role,
                "stale_plan: actor or role differs; replan"
            );
            let op = cli
                .op_id
                .as_deref()
                .context("import apply requires --op-id ID")?;
            let hash = digest(serde_json::to_vec(
                &json!({"import_plan":plan.digest,"actor":actor,"role":cli.role.name()}),
            )?);
            let before = {
                let s = Store::open_readonly(&cli.store, &cli.dolt)?;
                let rev = s.head("main")?;
                ensure!(
                    s.one_at::<String>("metadata", "store_id", &rev)?.as_deref()
                        == Some(&plan.store_id),
                    "stale_plan: wrong store UUID"
                );
                // Completed operation retry precedes every stale-base and source-object check.
                if let Some(result) = s.receipt(op, &hash)? {
                    return Ok(envelope(replay(&s, result)?));
                }
                let g = s.graph_at(&rev)?;
                if let Some(existing) = g.import_sources.get(&plan.source.source_id) {
                    ensure!(
                        same(existing, &plan.source)
                            && existing.candidate_digest == plan.source.candidate_digest,
                        "source_update_unsupported: source already accepted with different content"
                    );
                    let result = json!({"identical_replay":true,"source":existing.source_id,"branch":"main","op_id":op,"original_op_id":existing.op_id});
                    drop(s);
                    return Ok(envelope(noop_receipt(cli, &plan, op, &hash, result)?));
                }
                for proposal in s.proposals()?.into_iter().filter(|p| p.status == "pending") {
                    let pending = s.graph_at(&proposal.branch)?;
                    if let Some(existing) = pending.import_sources.get(&plan.source.source_id) {
                        ensure!(
                            same(existing, &plan.source)
                                && existing.candidate_digest == plan.source.candidate_digest,
                            "source_update_unsupported: pending namespace has different snapshot; resolve {}",
                            proposal.branch
                        );
                        let receipt: Value = s
                            .one_at("operations", &existing.op_id, &proposal.branch)?
                            .context("missing pending import receipt")?;
                        let mut result = receipt["result"].clone();
                        result["original_op_id"] = result["op_id"].clone();
                        result["op_id"] = json!(op);
                        drop(s);
                        return Ok(envelope(noop_receipt(cli, &plan, op, &hash, result)?));
                    }
                }
                ensure!(
                    rev == plan.main_revision && g.schema.revision() == plan.schema_hash,
                    "stale_plan: main or schema changed; replan"
                );
                g
            };
            let mut candidate = build(&before, &plan.source)?;
            ensure!(
                candidate.source.candidate_digest == plan.source.candidate_digest
                    && candidate.source.coverage == plan.source.coverage
                    && planned_changes(&candidate) == plan.changes,
                "stale_plan: recomputed candidate differs; replan"
            );
            let s = Store::open(&cli.store, &cli.dolt, false)?;
            s.checkout("main")?;
            if let Some(result) = s.receipt(op, &hash)? {
                return Ok(envelope(replay(&s, result)?));
            }
            ensure!(
                s.head("main")? == plan.main_revision
                    && s.one::<String>("metadata", "store_id")?.as_deref() == Some(&plan.store_id),
                "stale_plan: main changed during parsing; replan"
            );
            // Pending proposals do not advance main, so repeat namespace checks under the write lock.
            for p in s.proposals()?.into_iter().filter(|p| p.status == "pending") {
                ensure!(
                    !s.graph_at(&p.branch)?
                        .import_sources
                        .contains_key(&plan.source.source_id),
                    "source_update_unsupported: namespace reserved by {}; retry to inspect the pending import",
                    p.branch
                );
            }
            let protected = candidate.source.coverage["review_required"] == true;
            let target = if protected {
                format!("proposal/{}", uuid::Uuid::new_v4().simple())
            } else {
                "main".into()
            };
            policy::validate_change(&before, &candidate.graph, &BTreeSet::new())?;
            if !protected {
                candidate
                    .graph
                    .schema
                    .guards(&before, &candidate.graph, actor, cli.role.name())?;
            }
            let result = json!({"source":candidate.source.source_id,"branch":target,"op_id":op,"candidate_digest":candidate.source.candidate_digest,"coverage":candidate.source.coverage});
            if cli.dry_run {
                return Ok(envelope(json!({"dry_run":true,"result":result})));
            }
            candidate.source.imported_at = now();
            candidate.source.op_id = op.into();
            candidate
                .graph
                .import_sources
                .insert(candidate.source.source_id.clone(), candidate.source.clone());
            let mut sql = crate::storage::graph_changes(&before, &candidate.graph)?;
            sql.push_str(&persist(&candidate)?);
            if protected {
                let p = Proposal {
                    branch: target.clone(),
                    base: plan.main_revision.clone(),
                    actor: actor.into(),
                    status: "pending".into(),
                    reason: None,
                };
                sql.push_str(&put("metadata", &target, &p)?);
            }
            sql.push_str(&put("operations",op,&json!({"op_id":op,"args_hash":hash,"actor":actor,"command":cli.command,"result":result,"committed_at":now()}))?);
            if protected {
                s.run(&["checkout", "-b", &target, "main"])?;
            }
            s.transaction(
                &sql,
                &format!("import source {} by {actor}", candidate.source.source_id),
                actor,
            )?;
            Ok(envelope(result))
        }
        _ => bail!("unsupported import command"),
    }
}
fn read_profile(path: &Path) -> Result<String> {
    let meta = fs::metadata(path)?;
    ensure!(
        meta.len() <= profile::METADATA_LIMIT as u64,
        "input_limit: profile exceeds 1 MiB"
    );
    Ok(fs::read_to_string(path)?)
}
fn replay(s: &Store, mut result: Value) -> Result<Value> {
    if let Some(branch) = result["branch"].as_str() {
        if let Some(p) = s.proposals()?.iter().find(|p| p.branch == branch) {
            result["proposal_status"] = json!(p.status);
        }
    }
    Ok(result)
}
fn remove_source(g: &mut Graph, source: &str) {
    let ids: BTreeSet<_> = g
        .import_members
        .values()
        .filter(|m| m.source_id == source)
        .map(|m| m.record_id.clone())
        .collect();
    let edges: BTreeSet<_> = g
        .import_members
        .values()
        .filter(|m| m.source_id == source)
        .flat_map(|m| m.edges.keys().cloned())
        .collect();
    g.records.retain(|id, _| !ids.contains(id));
    g.import_members.retain(|id, _| !ids.contains(id));
    g.edges.retain(|k, _| !edges.contains(k));
    g.import_sources.remove(source);
}

/// Stable machine-readable diagnostics alongside the human error chain.
pub fn diagnostic(error: &anyhow::Error) -> Value {
    let reason = format!("{error:#}");
    let codes = [
        "input_limit",
        "unsafe_path",
        "ambiguous_rule",
        "duplicate_identity",
        "duplicate_alias",
        "schema_mismatch",
        "unresolved_reference",
        "empty_document_set",
        "source_update_unsupported",
        "unsupported_file_type",
        "source_ownership",
        "stale_plan",
        "recovery_required",
        "missing_value",
        "parse_error",
        "invalid_profile",
    ];
    let code = codes
        .into_iter()
        .find(|c| reason.contains(&format!("{c}:")))
        .unwrap_or("import_error");
    let context = |key: &str| {
        reason
            .split(&format!("{key}="))
            .nth(1)
            .map(|s| s.split([' ', ';', ':']).next().unwrap_or(s))
    };
    json!({"code":code,"severity":"error","path":context("path"),"rule":context("rule"),"field":context("field"),"span":context("span"),"reason":reason,"remedy":match code {"stale_plan"=>"Create a fresh plan against the current store, schema and actor.","source_ownership"=>"Keep captured content unchanged; create a local record or relationship.","source_update_unsupported"=>"Resolve any pending proposal; changed snapshots require an independent source namespace.","recovery_required"=>"Run doctor and inspect the recovery journal before planning.","input_limit"=>"Split the source or reduce metadata; profiles cannot raise engine limits.",_=>"Correct the profile or source value at the reported location, then plan again."}})
}

pub fn health(g: &Graph, files: &[File]) -> Result<Value> {
    let mut paths = BTreeSet::new();
    let mut bytes = 0usize;
    for f in files {
        namespace(&f.source_id)?;
        path(&f.file.path)?;
        ensure!(
            g.import_sources.contains_key(&f.source_id)
                && paths.insert((f.source_id.clone(), f.file.path.clone())),
            "source_ownership: orphan or duplicate source file"
        );
        let raw = STANDARD.decode(&f.file.bytes)?;
        bytes += raw.len();
        ensure!(
            digest(&raw) == f.file.sha256 && [0o100644, 0o100755].contains(&f.file.mode),
            "source_ownership: original checksum/mode mismatch {}",
            f.file.path
        );
        ensure!(
            ["document", "asset"].contains(&f.classification.as_str()),
            "source_ownership: invalid file classification"
        );
    }
    for m in g.import_members.values() {
        ensure!(
            paths.contains(&(m.source_id.clone(), m.path.clone())),
            "source_ownership: missing original for {}",
            m.record_id
        );
    }
    for s in g.import_sources.values() {
        ensure!(
            files.iter().filter(|f| f.source_id == s.source_id).count() as u64
                == s.coverage["files"].as_u64().context("invalid coverage")?,
            "source_ownership: file coverage mismatch"
        );
    }
    Ok(
        json!({"sources":g.import_sources.len(),"files":files.len(),"documents":g.import_members.len(),"bytes":bytes,"checksums_valid":true}),
    )
}

fn noop_receipt(cli: &Cli, plan: &Plan, op: &str, hash: &str, result: Value) -> Result<Value> {
    if cli.dry_run {
        return Ok(json!({"dry_run":true,"result":result}));
    }
    let s = Store::open(&cli.store, &cli.dolt, false)?;
    s.checkout("main")?;
    if let Some(existing) = s.receipt(op, hash)? {
        return replay(&s, existing);
    }
    ensure!(
        s.one::<String>("metadata", "store_id")?.as_deref() == Some(&plan.store_id),
        "stale_plan: wrong store"
    );
    let branch = result["branch"].as_str().context("replay branch")?;
    if branch != "main" {
        ensure!(
            s.proposals()?
                .iter()
                .any(|p| p.branch == branch && p.status == "pending"),
            "stale_plan: proposal changed; retry with a fresh plan"
        );
    }
    let g = s.graph_at(branch)?;
    let source = g
        .import_sources
        .get(&plan.source.source_id)
        .context("stale_plan: source disappeared")?;
    ensure!(
        same(source, &plan.source) && source.candidate_digest == plan.source.candidate_digest,
        "source_update_unsupported: source differs"
    );
    let sql = put(
        "operations",
        op,
        &json!({"op_id":op,"args_hash":hash,"actor":plan.actor,"command":cli.command,"result":result,"committed_at":now()}),
    )?;
    // A receipt changes no captured data. Keeping it on the source branch preserves proposal isolation.
    s.checkout(branch)?;
    s.transaction(&sql, "record identical source import retry", &plan.actor)?;
    replay(&s, result)
}

fn planned_changes(candidate: &Candidate) -> Value {
    json!({"documents":candidate.graph.import_members.values().filter(|m|m.source_id==candidate.source.source_id).map(|m|json!({"id":m.record_id,"key":m.key,"path":m.path,"codec":m.codec,"rule":m.rule,"kind":m.baseline.kind,"title":m.baseline.title,"status":m.baseline.status,"owner":m.baseline.owner,"fields":m.baseline.fields,"aliases":m.baseline.aliases,"body_sha256":digest(&m.baseline.body),"references":m.references})).collect::<Vec<_>>(),"files":candidate.files.iter().map(|f|json!({"path":f.file.path,"sha256":f.file.sha256,"git_oid":f.file.git_oid,"mode":f.file.mode,"classification":f.classification})).collect::<Vec<_>>()})
}
