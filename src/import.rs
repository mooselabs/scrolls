use crate::{model::*, policy};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Cursor,
    path::{Component, Path},
    process::Command,
};

pub struct Imported {
    pub graph: Graph,
    pub files: Vec<SourceFile>,
    pub info: ImportInfo,
    pub routing: Value,
}
fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let o = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    if !o.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr)
        );
    }
    Ok(o.stdout)
}
pub fn safe_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.contains('\\')
        || path.contains('\0')
        || Path::new(path)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("unsafe relative path {path:?}");
    }
    Ok(())
}
pub fn frontmatter(text: &str) -> (Value, String) {
    if !text.starts_with("---\n") && !text.starts_with("---\r\n") {
        return (json!({}), text.into());
    }
    let start = text.find('\n').unwrap() + 1;
    let mut end = None;
    let mut offset = start;
    for line in text[start..].split_inclusive('\n') {
        if line.trim() == "---" {
            end = Some((offset, offset + line.len()));
            break;
        }
        offset += line.len();
    }
    let Some((end, body)) = end else {
        return (json!({}), text.into());
    };
    let yaml = &text[start..end];
    let value = serde_yaml::from_str::<Value>(yaml)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| {
            // Legacy docs intentionally admit plain scalars and block lists that strict YAML rejects.
            let mut map = serde_json::Map::new();
            let mut key = String::new();
            for line in yaml.lines() {
                if let Some(item) = line.strip_prefix("  - ") {
                    if let Some(v) = map.get_mut(&key).and_then(Value::as_array_mut) {
                        v.push(json!(item.trim().trim_matches(['\'', '"'])));
                    }
                } else if !line.starts_with(' ') {
                    if let Some((k, v)) = line.split_once(':') {
                        key = k.into();
                        let v = v.trim();
                        let parsed = if v.is_empty() {
                            json!([])
                        } else {
                            serde_yaml::from_str::<Value>(v)
                                .unwrap_or(json!(v.trim_matches(['\'', '"'])))
                        };
                        map.insert(key.clone(), parsed);
                    }
                }
            }
            Value::Object(map)
        });
    (value, text[body..].into())
}
pub fn strings(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Value::String(s) => vec![s.clone()],
        _ => vec![],
    }
}
fn scalar(v: &Value, key: &str, default: &str) -> String {
    v[key].as_str().unwrap_or(default).into()
}
pub fn resolve_reference(path: &str, reference: &str) -> Option<String> {
    if reference.contains("://") || reference.starts_with('/') || reference.starts_with("mailto:") {
        return None;
    }
    let reference = reference.split('#').next()?.split('?').next()?;
    let mut parts: Vec<_> = path.split('/').collect();
    parts.pop();
    for p in reference.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}
pub fn import(dir: &Path, rev: &str, actor: &str) -> Result<Imported> {
    let dir = dir.canonicalize()?;
    let root = String::from_utf8(git(&dir, &["rev-parse", "--show-toplevel"])?)?
        .trim()
        .to_string();
    let root = Path::new(&root);
    let commit = String::from_utf8(git(
        root,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )?)?
    .trim()
    .to_string();
    let prefix = dir
        .strip_prefix(root)?
        .to_str()
        .context("UTF8 source path required")?
        .to_string();
    if prefix.is_empty() {
        bail!("pass the documentation directory within the Git repository");
    }
    let archive = git(root, &["archive", "--format=tar", &commit, &prefix])?;
    let tree = git(root, &["ls-tree", "-rz", &commit, "--", &prefix])?;
    let mut oids = BTreeMap::new();
    for entry in tree.split(|b| *b == 0).filter(|b| !b.is_empty()) {
        let s = std::str::from_utf8(entry)?;
        let (header, path) = s.split_once('\t').context("git tree entry")?;
        oids.insert(
            path.to_string(),
            header
                .split_whitespace()
                .nth(2)
                .context("git oid")?
                .to_string(),
        );
    }
    let mut graph = Graph::default();
    let mut files = vec![];
    let mut markdown = 0;
    let mut untyped = 0;
    let re = regex::Regex::new(r"^(?:BKLG|EPIC|Q)-\d{8}-[a-z0-9]{4}").unwrap();
    for entry in tar::Archive::new(Cursor::new(archive)).entries()? {
        let mut entry = entry?;
        if entry.header().entry_type().is_dir()
            || entry.header().entry_type().is_pax_global_extensions()
            || entry.header().entry_type().is_pax_local_extensions()
        {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            bail!(
                "unsupported non-regular tracked file {}; import refuses symlink escapes",
                entry.path()?.display()
            );
        }
        let full = entry
            .path()?
            .to_str()
            .context("UTF8 source path required")?
            .to_string();
        let path = full
            .strip_prefix(&format!("{prefix}/"))
            .context("archive outside source prefix")?
            .to_string();
        safe_path(&path)?;
        let mut bytes = vec![];
        std::io::Read::read_to_end(&mut entry, &mut bytes)?;
        let hash = digest(&bytes);
        files.push(SourceFile {
            path: path.clone(),
            sha256: hash,
            git_oid: oids[&full].clone(),
            mode: entry.header().mode()?,
            bytes: STANDARD.encode(&bytes),
        });
        if !path.ends_with(".md") {
            continue;
        }
        markdown += 1;
        let text =
            std::str::from_utf8(&bytes).with_context(|| format!("non-UTF8 Markdown {path}"))?;
        let (fm, body) = frontmatter(text);
        let dtype = fm["doc_type"].as_str().unwrap_or("");
        if dtype.is_empty() {
            untyped += 1;
        }
        let kind = if path.starts_with("meta/doctrine/") && !path.ends_with("README.md") {
            "doctrine"
        } else {
            match dtype {
                "raw" => "evidence",
                "knowledge" | "business" => "knowledge",
                "adr" => "decision",
                "question" => "question",
                "index" => "collection",
                _ => "note",
            }
        };
        let mut r = Record::new(kind, &scalar(&fm, "title", &path), &body, actor)?;
        r.id = format!("sc-{}", &digest(format!("import:{prefix}/{path}"))[..32]);
        let old_status = fm["status"].as_str().unwrap_or("");
        r.status = match kind {
            "knowledge" => match old_status {
                "canonical" | "current" => "current",
                "stale" => "stale",
                "deprecated" | "retired" | "archived" => "retired",
                _ => "draft",
            },
            "decision" => match old_status {
                "accepted" => "accepted",
                "superseded" => "superseded",
                "deprecated" | "retired" | "rejected" => "retired",
                _ => "proposed",
            },
            "doctrine" => {
                if old_status == "canonical" {
                    "active"
                } else {
                    "proposed"
                }
            }
            "question" => match old_status {
                "answered" => "answered",
                "withdrawn" => "withdrawn",
                _ => "open",
            },
            "evidence" => {
                if old_status == "archived" {
                    "archived"
                } else {
                    "captured"
                }
            }
            "collection" => {
                if old_status == "stale" {
                    "stale"
                } else {
                    "current"
                }
            }
            _ => "recorded",
        }
        .into();
        r.owner = scalar(&fm, "owner", "historical");
        r.created = scalar(&fm, "created", &r.created);
        r.last_reviewed = scalar(&fm, "last_reviewed", &r.created);
        r.confidence = scalar(&fm, "confidence", "medium");
        r.fields = fm.clone();
        r.path = Some(path.clone());
        r.frozen = matches!(dtype, "backlog" | "epic" | "milestone" | "question")
            || path.starts_with("backlog/")
            || path.starts_with("questions/")
            || dtype.is_empty();
        r.aliases.push(full);
        let name = Path::new(&path).file_stem().unwrap().to_string_lossy();
        if dtype == "adr" && name.len() >= 4 {
            r.aliases.push(format!("ADR-{}", &name[..4]));
        }
        if let Some(m) = re.find(&name) {
            r.aliases.push(m.as_str().into());
        }
        if dtype == "milestone" {
            r.aliases.push(name.into_owned());
        }
        r.original = Some(serde_json::to_value(&r)?);
        graph.records.insert(r.id.clone(), r);
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest = git(root, &["show", &format!("{commit}:repos/manifest.json")])
        .ok()
        .map(|b| serde_json::from_slice::<Value>(&b))
        .transpose()?
        .unwrap_or(json!({}));
    for repo in manifest["repositories"].as_array().into_iter().flatten() {
        let name = repo["name"].as_str().context("manifest repository name")?;
        let mut r = Record::new("entity", name, "", actor)?;
        r.id = format!("sc-{}", &digest(format!("repo:{name}"))[..32]);
        r.fields = json!({"type":"repo","manifest":repo});
        r.aliases = vec![name.into(), format!("repos/{name}")];
        r.original = Some(serde_json::to_value(&r)?);
        graph.records.insert(r.id.clone(), r);
    }
    let mut aliases = BTreeMap::new();
    for r in graph.records.values() {
        aliases.insert(r.id.clone(), r.id.clone());
        if let Some(p) = &r.path {
            aliases.insert(p.clone(), r.id.clone());
        }
        for a in &r.aliases {
            aliases.insert(a.clone(), r.id.clone());
        }
    }
    let resolve = |name: &str| -> Result<String> {
        aliases
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown alias {name}"))
    };
    let records: Vec<_> = graph.records.values().cloned().collect();
    for r in &records {
        for relation in ["supersedes", "depends_on"] {
            for reference in strings(&r.fields[relation]) {
                if let Ok(target) = resolve(&reference) {
                    if (relation == "supersedes" && r.kind == graph.records[&target].kind)
                        || (relation == "depends_on"
                            && r.kind == "decision"
                            && graph.records[&target].kind == "decision")
                    {
                        graph.add_edge(
                            &r.id,
                            relation,
                            &target,
                            &format!("imported {relation}"),
                            actor,
                        );
                    }
                }
            }
        }
        for name in strings(&r.fields["related_repos"]) {
            if let Ok(target) = resolve(&name) {
                graph.add_edge(&r.id, "about", &target, "imported related_repos", actor);
            }
        }
        if let Some(path) = &r.path {
            for reference in strings(&r.fields["source_documents"]) {
                let target = resolve_reference(path, &reference)
                    .and_then(|p| resolve(&p).ok())
                    .or_else(|| resolve(&reference).ok());
                if let Some(target) = target {
                    if graph.records[&target].kind == "evidence"
                        && matches!(r.kind.as_str(), "knowledge" | "decision")
                    {
                        graph.add_edge(
                            &target,
                            "supports",
                            &r.id,
                            "imported source_documents",
                            actor,
                        );
                    } else if r.kind == "knowledge"
                        && matches!(
                            graph.records[&target].kind.as_str(),
                            "knowledge" | "evidence"
                        )
                    {
                        graph.add_edge(
                            &r.id,
                            "derived_from",
                            &target,
                            "imported source_documents",
                            actor,
                        );
                    } else {
                        graph.add_edge(
                            &r.id,
                            "related",
                            &target,
                            "imported source_documents",
                            actor,
                        );
                    }
                }
            }
        }
    }
    let agents = git(root, &["show", &format!("{commit}:AGENTS.md")])
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let mut triggers = vec![];
    let link = regex::Regex::new(r"\]\((docs/meta/doctrine/[^)]+)\)")?;
    let glob = regex::Regex::new(r"`([^`]+)`")?;
    for line in agents.lines().filter(|l| l.starts_with('|')) {
        let Some(cap) = link.captures(line) else {
            continue;
        };
        let Ok(id) = resolve(&cap[1]) else { continue };
        let clause = line.split('|').nth(1).unwrap_or("").trim();
        let globs: Vec<_> = glob
            .captures_iter(clause)
            .map(|c| c[1].to_string())
            .collect();
        for p in &globs {
            globset::Glob::new(p)?;
        }
        let scope = clause
            .find("scope:")
            .map(|n| clause[n + 6..].trim().to_string());
        triggers.push(json!({"id":id,"globs":globs,"scope_clause":scope}));
    }
    let info = ImportInfo {
        repository: root.to_string_lossy().into_owned(),
        commit,
        prefix,
        files: files.len(),
        markdown,
        untyped,
        lint_debt: policy::lint(&graph),
    };
    Ok(Imported {
        graph,
        files,
        info,
        routing: json!({"triggers":triggers,"manifest":manifest,"agents_sha256":digest(agents.as_bytes())}),
    })
}
