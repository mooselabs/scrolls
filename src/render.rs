use crate::{
    cli::Format,
    import::{safe_path, strings},
    model::*,
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, io::Write, path::Path};
fn original(r: &Record) -> Result<Option<Record>> {
    r.original
        .clone()
        .map(serde_json::from_value)
        .transpose()
        .map_err(Into::into)
}
fn changed(r: &Record) -> Result<bool> {
    let Some(mut old) = original(r)? else {
        return Ok(true);
    };
    let mut new = r.clone();
    new.original = None;
    new.revision = old.revision;
    new.actor = old.actor.clone();
    old.original = None;
    Ok(old != new)
}
fn relative(from: &str, to: &str) -> String {
    let mut a: Vec<_> = from.split('/').collect();
    a.pop();
    let b: Vec<_> = to.split('/').collect();
    let mut n = 0;
    while n < a.len() && n < b.len() && a[n] == b[n] {
        n += 1
    }
    format!("{}{}", "../".repeat(a.len() - n), b[n..].join("/"))
}
fn legacy_status(r: &Record, old: &Record) -> Result<String> {
    if r.status == old.status {
        return Ok(old.fields["status"].as_str().unwrap_or("").into());
    }
    Ok(match r.kind.as_str() {
        "knowledge" | "primer" => match r.status.as_str() {
            "current" => "canonical",
            "draft" => "draft",
            "stale" => "stale",
            "retired" => "deprecated",
            _ => bail!("unrepresentable knowledge status"),
        },
        "decision" => match r.status.as_str() {
            "proposed" => "proposed",
            "accepted" => "accepted",
            "superseded" => "superseded",
            _ => bail!("unrepresentable decision status"),
        },
        _ => bail!(
            "status edit on {} is not representable in liskov format",
            r.id
        ),
    }
    .into())
}
pub struct Options<'a> {
    pub out: &'a Path,
    pub format: Format,
    pub dry_run: bool,
    pub beads: &'a crate::beads::Local,
}
pub fn render(
    g: &Graph,
    files: &[SourceFile],
    info: Option<&ImportInfo>,
    baseline: &[Edge],
    revision: &str,
    options: Options<'_>,
) -> Result<Value> {
    let Options {
        out,
        format,
        dry_run,
        beads,
    } = options;
    if out.exists() || out.symlink_metadata().is_ok() {
        bail!("render destination must be fresh: {}", out.display());
    }
    let absolute = if out.is_absolute() {
        out.to_path_buf()
    } else {
        std::env::current_dir()?.join(out)
    };
    for ancestor in absolute.ancestors().skip(1) {
        if ancestor
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            bail!("render parent contains a symlink: {}", ancestor.display());
        }
    }
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent
        .canonicalize()
        .context("render destination parent must exist")?;
    let name = out
        .file_name()
        .context("render destination needs a directory name")?;
    let destination = parent.join(name);
    if let Some(i) = info {
        let source = Path::new(&i.repository).join(&i.prefix);
        if destination.starts_with(&source) {
            bail!("render must not write inside the source documentation tree");
        }
    }
    let mut output: BTreeMap<String, Vec<u8>> = files
        .iter()
        .map(|f| Ok((f.path.clone(), STANDARD.decode(&f.bytes)?)))
        .collect::<Result<_>>()?;
    let mut metadata = BTreeMap::new();
    let mut graph_only = vec![];
    for r in g.records.values() {
        if let Some(path) = &r.path {
            if changed(r)? {
                if r.frozen {
                    bail!("frozen record changed: {path}");
                }
                let old = original(r)?.context("imported record without original snapshot")?;
                if format == Format::Liskov
                    && (r.review_every != old.review_every
                        || r.fields
                            .as_object()
                            .unwrap()
                            .keys()
                            .any(|key| old.fields.get(key).is_none()))
                {
                    bail!(
                        "record {} has native-only metadata; use native format",
                        r.id
                    )
                }
                let mut fm = r.fields.clone();
                fm["title"] = json!(r.title);
                fm["owner"] = json!(r.owner);
                fm["confidence"] = json!(r.confidence);
                fm["last_reviewed"] = json!(r.last_reviewed);
                fm["status"] = json!(legacy_status(r, &old)?);
                metadata.insert(r.id.clone(), fm);
            }
        } else if r.original.is_some() {
            graph_only.push(r.id.clone());
            if format == Format::Liskov && changed(r)? {
                bail!(
                    "graph-only imported entity {} cannot be edited in liskov output",
                    r.id
                );
            }
        } else if format == Format::Liskov {
            bail!(
                "native addition {} ({}) is not representable in liskov format; use native",
                r.id,
                r.kind
            );
        }
    }
    let base: BTreeMap<_, _> = baseline.iter().map(|e| (e.key(), e)).collect();
    for (edge, add) in g
        .edges
        .iter()
        .filter(|(k, _)| !base.contains_key(*k))
        .map(|(_, e)| (e, true))
        .chain(
            base.iter()
                .filter(|(k, _)| !g.edges.contains_key(*k))
                .map(|(_, e)| (*e, false)),
        )
    {
        if format == Format::Native {
            continue;
        }
        let (record, target, key) = match edge.rel.as_str() {
            "supports" => (&edge.to, &edge.from, "source_documents"),
            "derived_from" => (&edge.from, &edge.to, "source_documents"),
            "about" => (&edge.from, &edge.to, "related_repos"),
            "supersedes" if g.records[&edge.from].kind == "decision" => {
                (&edge.from, &edge.to, "supersedes")
            }
            _ => bail!(
                "graph change {} is not representable in liskov output",
                edge.rel
            ),
        };
        let r = &g.records[record];
        if r.frozen {
            bail!("graph change affects frozen {}", r.id)
        }
        let path = r
            .path
            .as_ref()
            .context("graph-only entity has no legacy document")?;
        let t = &g.records[target];
        let value = if key == "related_repos" {
            t.title.clone()
        } else if key == "supersedes" {
            t.aliases
                .iter()
                .find(|a| a.starts_with("ADR-"))
                .context("legacy supersedes requires an ADR alias")?
                .clone()
        } else {
            relative(
                path,
                t.path
                    .as_ref()
                    .context("provenance target has no legacy path")?,
            )
        };
        let fm = metadata
            .entry(record.clone())
            .or_insert_with(|| r.fields.clone());
        let mut values = strings(&fm[key]);
        if add {
            if !values.contains(&value) {
                values.push(value)
            }
        } else {
            values.retain(|v| v != &value)
        }
        fm[key] = json!(values);
    }
    for (id, fm) in metadata {
        let r = &g.records[&id];
        let path = r.path.as_ref().unwrap();
        let text = format!("---\n{}---\n{}", serde_yaml::to_string(&fm)?, r.body);
        output.insert(path.clone(), text.into_bytes());
    }
    // Native records are a complete normalized graph, including entities and edges.
    if format == Format::Native {
        for r in g.records.values() {
            if r.path.is_none() {
                output.insert(format!("scrolls/{}.md",r.id),format!("---\n{}---\n{}",serde_yaml::to_string(&json!({"id":r.id,"kind":r.kind,"title":r.title,"status":r.status,"owner":r.owner}))?,r.body).into_bytes());
            }
        }
        // Derived navigation is emitted only for qualified work references.
        // Untouched imported documents remain byte-for-byte identical.
        let mut work = std::collections::BTreeSet::new();
        for r in g.records.values() {
            let edges: Vec<_> = g
                .edges
                .values()
                .filter(|e| e.from == r.id && e.to.starts_with("beads://"))
                .collect();
            if edges.is_empty() {
                continue;
            }
            let path = r
                .path
                .clone()
                .unwrap_or_else(|| format!("scrolls/{}.md", r.id));
            if let Some(bytes) = output.get_mut(&path) {
                bytes.extend_from_slice(b"\n\n## Related work\n\n");
                for edge in edges {
                    let reference = crate::work_ref::BeadRef::parse(&edge.to)?;
                    let target = format!(
                        "scrolls/beads/{}/{}.md",
                        reference.project_id, reference.issue_id
                    );
                    let line = format!(
                        "- `{}`: [{}]({})\n",
                        edge.rel,
                        reference.issue_id,
                        relative(&path, &target)
                    );
                    bytes.extend_from_slice(line.as_bytes());
                    work.insert(edge.to.clone());
                }
            }
        }
        for uri in work {
            let reference = crate::work_ref::BeadRef::parse(&uri)?;
            let path = format!(
                "scrolls/beads/{}/{}.md",
                reference.project_id, reference.issue_id
            );
            let mut body = format!(
                "# {}\n\nReference: `{uri}`\n\nCached observation:\n\n```json\n{}\n```\n\n## Documents\n\n",
                reference.issue_id,
                serde_json::to_string_pretty(&beads.summary(&uri))?
            );
            for doc in crate::beads::refs(g, &uri) {
                let record = &g.records[doc["id"].as_str().unwrap()];
                let target = record
                    .path
                    .clone()
                    .unwrap_or_else(|| format!("scrolls/{}.md", record.id));
                body.push_str(&format!(
                    "- [{}]({}) — {} / {}; {}\n",
                    record.id,
                    relative(&path, &target),
                    record.kind,
                    record.status,
                    doc["relations"]
                ));
            }
            if output.insert(path.clone(), body.into_bytes()).is_some() {
                bail!("reserved Beads render path {path}");
            }
        }
        output.insert(
            "scrolls/graph.json".into(),
            serde_json::to_vec_pretty(&json!({"records":g.records,"edges":g.edges}))?,
        );
    }
    // Do not normalize a pristine export; verify its indexes separately. On edits
    // refresh only directory indexes, never frozen work views.
    if files
        .iter()
        .any(|f| output.get(&f.path).is_some_and(|b| digest(b) != f.sha256))
    {
        refresh_indexes(&mut output)?;
    }
    let hashes: BTreeMap<_, _> = output.iter().map(|(p, b)| (p.clone(), digest(b))).collect();
    let manifest = json!({"schema_version":1,"revision":revision,"format":format,"source":info,"files":hashes,"graph_only_imported_entities":graph_only});
    for path in output.keys() {
        safe_path(path)?;
        if path == "RENDERED" || path == "SCROLLS-MANIFEST.json" {
            bail!("reserved render path {path}");
        }
    }
    if dry_run {
        return Ok(json!({"dry_run":true,"out":destination,"manifest":manifest}));
    }
    let stage = tempfile::Builder::new()
        .prefix(".scrolls-render-")
        .tempdir_in(&parent)?;
    for (path, bytes) in &output {
        let target = stage.path().join(path);
        fs::create_dir_all(target.parent().unwrap())?;
        let mut f = fs::File::create(&target)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::write(
        stage.path().join("RENDERED"),
        format!("Scrolls v1\nDolt revision: {revision}\n"),
    )?;
    fs::write(
        stage.path().join("SCROLLS-MANIFEST.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    // Reserve the final name atomically; this refuses a racing symlink/directory.
    fs::create_dir(&destination).context("render destination appeared during export")?;
    if let Err(e) = fs::rename(stage.path(), &destination) {
        let _ = fs::remove_dir(&destination);
        return Err(e.into());
    }
    Ok(json!({"out":destination,"files":output.len(),"graph_only_imported_entities":graph_only}))
}
fn escape(v: &Value) -> String {
    v.as_str()
        .unwrap_or("—")
        .replace('|', "\\|")
        .replace('\n', " ")
}
pub fn refresh_indexes(files: &mut BTreeMap<String, Vec<u8>>) -> Result<()> {
    let records: BTreeMap<_, _> = files
        .iter()
        .filter(|(p, _)| {
            p.ends_with(".md")
                && !p.contains("/imported/")
                && !p.contains("templates/")
                && !p.ends_with(".template.md")
        })
        .filter_map(|(p, b)| {
            let fm = legacy_frontmatter(std::str::from_utf8(b).ok()?);
            fm["doc_type"].as_str()?;
            Some((p.clone(), fm))
        })
        .collect();
    let mut aliases = BTreeMap::new();
    let mut amendments: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let re = regex::Regex::new(r"^(?:BKLG|EPIC|Q)-\d{8}-[a-z0-9]{4}")?;
    for (path, fm) in &records {
        let name = Path::new(path).file_stem().unwrap().to_string_lossy();
        if let Some(m) = re.find(&name) {
            aliases.insert(m.as_str().to_string(), fm.clone());
        }
        if fm["doc_type"] == "adr" && name.len() >= 4 {
            let id = format!("ADR-{}", &name[..4]);
            for target in strings(&fm["amends"]) {
                amendments.entry(target).or_default().push(id.clone());
            }
        }
    }
    const BEGIN: &str = "<!-- BEGIN GENERATED INDEX -->";
    const END: &str = "<!-- END GENERATED INDEX -->";
    for (readme, sub, dates) in [
        ("raw/README.md", "raw", true),
        ("work-notes/README.md", "work-notes/sessions", true),
        ("questions/README.md", "questions", false),
        ("knowledge/README.md", "knowledge", false),
        ("decisions/README.md", "decisions/adr", false),
        ("business/README.md", "business", false),
    ] {
        let Some(bytes) = files.get(readme) else {
            continue;
        };
        let text = String::from_utf8(bytes.clone())?;
        let (Some(begin), Some(end)) = (text.find(BEGIN), text.find(END)) else {
            continue;
        };
        let mut entries: Vec<_> = records
            .iter()
            .filter(|(p, _)| p.starts_with(&format!("{sub}/")) && !p.ends_with("/README.md"))
            .collect();
        entries.sort_by_key(|(p, _)| relative(readme, p));
        if dates {
            entries.reverse()
        }
        let mut lines = vec![BEGIN.into(), "".into(), format!("{} files.", entries.len())];
        let mut group = String::new();
        for (path, fm) in entries {
            let rel = relative(readme, path);
            let name = Path::new(path).file_name().unwrap().to_string_lossy();
            if dates {
                let new = name.chars().take(10).collect::<String>();
                if new != group {
                    group = new;
                    lines.extend(["".into(), format!("### {group}"), "".into()]);
                }
            } else if group.is_empty() {
                group = "all".into();
                lines.push("".into());
            }
            let status = fm["status"].as_str().unwrap_or("");
            let mut facts = vec![];
            if fm["doc_type"] == "adr" {
                if let Some(delivery) = fm["delivery"].as_str() {
                    let targets = strings(&fm["delivered_by"]);
                    let done = targets
                        .iter()
                        .filter(|t| aliases.get(*t).is_some_and(|f| f["status"] == "done"))
                        .count();
                    facts.push(format!(
                        "delivery: {delivery} ({done}/{} done)",
                        targets.len()
                    ));
                }
                let supersedes = strings(&fm["supersedes"]);
                if !supersedes.is_empty() {
                    facts.push(format!("supersedes {}", supersedes.join(", ")));
                }
                let id = format!("ADR-{}", &name[..4]);
                if let Some(amends) = amendments.get(&id) {
                    let mut a = amends.clone();
                    a.sort();
                    a.dedup();
                    facts.push(format!("amended by {}", a.join(", ")));
                }
            } else if fm["doc_type"] == "question" && status == "open" {
                let id = re.find(&name).map(|m| m.as_str()).unwrap_or("");
                let n = records
                    .values()
                    .filter(|v| {
                        matches!(v["doc_type"].as_str(), Some("backlog" | "epic"))
                            && matches!(v["status"].as_str(), Some("open" | "working"))
                            && strings(&v["depends_on"]).iter().any(|s| s == id)
                    })
                    .count();
                facts.push(format!("blocks: {n}"));
            }
            let suffix = facts.iter().map(|f| format!(" · {f}")).collect::<String>();
            lines.push(format!(
                "- [{}]({rel}) — `{status}`{suffix}",
                escape(&fm["title"])
            ));
        }
        lines.extend(["".into(), END.into()]);
        files.insert(
            readme.into(),
            format!(
                "{}{}{}",
                &text[..begin],
                lines.join("\n"),
                &text[end + END.len()..]
            )
            .into_bytes(),
        );
    }
    Ok(())
}

// The Liskov index renderer deliberately uses a minimal YAML-ish parser, not
// YAML scalar decoding: quoted backslashes and continuation lines matter.
// Keep this adapter independent of the normalized import parser.
pub fn legacy_frontmatter(text: &str) -> Value {
    fn scalar(s: &str) -> String {
        let s = s.trim();
        if s.len() >= 2
            && (s.starts_with('\'') && s.ends_with('\'') || s.starts_with('"') && s.ends_with('"'))
        {
            s[1..s.len() - 1].into()
        } else {
            s.into()
        }
    }
    let mut data = serde_json::Map::new();
    let mut list: Option<String> = None;
    if !text.starts_with("---") {
        return Value::Object(data);
    }
    for line in text.lines().skip(1) {
        if line.trim() == "---" {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        if let Some(item) = line.trim_start().strip_prefix("- ") {
            if let Some(key) = &list {
                data.get_mut(key)
                    .unwrap()
                    .as_array_mut()
                    .unwrap()
                    .push(json!(scalar(item)));
                continue;
            }
        }
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let value = value.trim();
        if value.starts_with('[') && value.ends_with(']') {
            data.insert(
                key.into(),
                json!(
                    value[1..value.len() - 1]
                        .split(',')
                        .filter(|s| !s.trim().is_empty())
                        .map(scalar)
                        .collect::<Vec<_>>()
                ),
            );
            list = None;
        } else if value.is_empty() {
            data.insert(key.into(), json!([]));
            list = Some(key.into());
        } else {
            data.insert(key.into(), json!(scalar(value)));
            list = None;
        }
    }
    Value::Object(data)
}
