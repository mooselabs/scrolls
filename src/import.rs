use crate::{model::*, policy};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    path::{Component, Path},
    process::{Command, Stdio},
};

pub struct Imported {
    pub graph: Graph,
    pub files: Vec<SourceFile>,
    pub info: ImportInfo,
    pub routing: Value,
}
pub(crate) fn git_command(repo: &Path) -> Command {
    let mut c = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(|k| k.starts_with("GIT_")) {
            c.env_remove(key);
        }
    }
    c.arg("--no-replace-objects").arg("-C").arg(repo);
    c
}
pub(crate) fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let o = git_command(repo).args(args).stdin(Stdio::null()).output()?;
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
    import_selected(dir, rev, actor, false)
}
pub(crate) fn import_selected(
    dir: &Path,
    rev: &str,
    actor: &str,
    markdown_only: bool,
) -> Result<Imported> {
    let dir = dir.canonicalize()?;
    let root = String::from_utf8(git(&dir, &["rev-parse", "--show-toplevel"])?)?
        .trim()
        .to_string();
    let root = Path::new(&root);
    let commit = String::from_utf8(git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{rev}^{{commit}}"),
        ],
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
    let source_files = read_files(root, &commit, &prefix, markdown_only)?;
    let mut graph = Graph::default();
    let mut files = vec![];
    let mut markdown = 0;
    let mut untyped = 0;
    let re = regex::Regex::new(r"^(?:BKLG|EPIC|Q)-\d{8}-[a-z0-9]{4}").unwrap();
    for source in source_files {
        let path = source.path.clone();
        let full = format!("{prefix}/{path}");
        let bytes = STANDARD.decode(&source.bytes)?;
        files.push(source);
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
        if dtype == "adr"
            && name
                .get(..4)
                .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
        {
            r.aliases.push(format!("ADR-{}", &name[..4]));
            r.metadata = Some(RecordMetadata {
                number: name[..4].into(),
            });
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
        let scope = clause
            .find("scope:")
            .map(|n| clause[n + 6..].trim().to_string());
        let mut trigger = json!({"id":id,"globs":globs});
        if let Some(scope) = scope {
            trigger["scope_clause"] = json!(scope);
        }
        crate::triggers::imported(&trigger)?;
        triggers.push(trigger);
    }
    for record in graph.records.values().filter(|r| r.kind == "doctrine") {
        crate::triggers::parse(&record.id, &record.fields, "glob")?;
    }
    for edge in graph
        .edges
        .values()
        .filter(|e| e.rel == "applies_to" && e.to.starts_with("glob:"))
    {
        crate::triggers::glob(&edge.from, "applies_to", &edge.to[5..])?;
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

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitEntry {
    pub path: String,
    pub mode: u32,
    pub oid: String,
    pub size: u64,
}
pub(crate) fn inventory(repo: &Path, commit: &str) -> Result<Vec<GitEntry>> {
    let tree = git(repo, &["ls-tree", "-rlz", "--full-tree", commit])?;
    tree.split(|b| *b == 0)
        .filter(|b| !b.is_empty())
        .map(|row| {
            let row = std::str::from_utf8(row).context("non-UTF8 Git path")?;
            let (head, path) = row.split_once('\t').context("invalid Git inventory")?;
            safe_path(path)?;
            let parts: Vec<_> = head.split_whitespace().collect();
            anyhow::ensure!(parts.len() == 4, "invalid Git inventory");
            Ok(GitEntry {
                path: path.into(),
                mode: u32::from_str_radix(parts[0], 8)?,
                oid: parts[2].into(),
                size: if parts[3] == "-" {
                    0
                } else {
                    parts[3].parse()?
                },
            })
        })
        .collect()
}
fn read_files(
    repo: &Path,
    commit: &str,
    prefix: &str,
    markdown_only: bool,
) -> Result<Vec<SourceFile>> {
    let prefix = format!("{prefix}/");
    let entries: Vec<_> = inventory(repo, commit)?
        .into_iter()
        .filter(|e| e.path.starts_with(&prefix) && (!markdown_only || e.path.ends_with(".md")))
        .collect();
    anyhow::ensure!(
        entries.len() <= crate::source_import::FILE_COUNT,
        "input_limit: too many source files"
    );
    let total: u64 = entries.iter().map(|e| e.size).sum();
    anyhow::ensure!(
        total <= crate::source_import::TOTAL_LIMIT,
        "input_limit: source bytes exceed 1 GiB"
    );
    for e in &entries {
        anyhow::ensure!(
            [0o100644, 0o100755].contains(&e.mode),
            "unsupported non-regular tracked file {}",
            e.path
        );
        anyhow::ensure!(
            e.size <= crate::source_import::FILE_LIMIT,
            "input_limit: source file exceeds 64 MiB"
        );
    }
    let mut child = git_command(repo)
        .arg("cat-file")
        .arg("--batch")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let result = (|| -> Result<Vec<SourceFile>> {
        let mut input = child.stdin.take().context("Git input")?;
        let mut output = BufReader::new(child.stdout.take().context("Git output")?);
        let mut files = vec![];
        for e in entries {
            writeln!(input, "{}", e.oid)?;
            input.flush()?;
            let mut header = String::new();
            output.read_line(&mut header)?;
            let parts: Vec<_> = header.split_whitespace().collect();
            anyhow::ensure!(
                parts.len() == 3
                    && parts[0] == e.oid
                    && parts[1] == "blob"
                    && parts[2].parse::<u64>()? == e.size,
                "source_read: Git blob framing mismatch"
            );
            let mut bytes = vec![0; e.size as usize];
            output.read_exact(&mut bytes)?;
            let mut end = [0];
            output.read_exact(&mut end)?;
            anyhow::ensure!(end == *b"\n", "source_read: Git blob terminator");
            files.push(SourceFile {
                path: e
                    .path
                    .strip_prefix(&prefix)
                    .context("source prefix")?
                    .into(),
                sha256: digest(&bytes),
                git_oid: e.oid,
                mode: e.mode,
                bytes: STANDARD.encode(bytes),
            });
        }
        drop(input);
        Ok(files)
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let files = result?;
    anyhow::ensure!(status.success(), "source_read: Git object reader failed");
    Ok(files)
}
