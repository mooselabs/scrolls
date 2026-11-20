use super::{
    Format,
    export::{Export, ExportSnapshot, GRAPH, Identity, MANIFEST, MARKER, VERSION},
};
use crate::{
    model::{TenantId, digest},
    store::KnowledgeStore,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const MANIFEST_VERSION: u32 = 2;

fn counts(graph: &Value, files: usize) -> Value {
    let mut kinds = BTreeMap::new();
    for record in graph["records"]
        .as_object()
        .into_iter()
        .flat_map(|records| records.values())
    {
        if let Some(kind) = record["kind"].as_str() {
            *kinds.entry(kind).or_insert(0usize) += 1;
        }
    }
    json!({
        "records_by_kind": kinds,
        "edges": graph["edges"].as_object().map_or(0, |edges| edges.len()),
        "files": files,
    })
}

fn canonical_digest(mut value: Value) -> Result<String> {
    value.sort_all_objects();
    Ok(digest(serde_json::to_vec(&value)?))
}

pub(super) fn describe(
    manifest: &mut Value,
    identity: &Identity<'_>,
    graph: &Value,
    outside_store: &[String],
) -> Result<()> {
    let mut details = BTreeMap::new();
    for (id, record) in graph["records"]
        .as_object()
        .context("missing graph records")?
    {
        details.insert(
            id,
            json!({
                "kind": record["kind"],
                "revision": record["revision"],
                "aliases": record["aliases"],
                "digest": canonical_digest(record.clone())?,
            }),
        );
    }
    manifest["schema_version"] = json!(MANIFEST_VERSION);
    manifest["revision"] = json!(identity.revision);
    manifest["ref"] = json!(identity.reference);
    if let Some(store_id) = identity.store_id {
        manifest["store_id"] = json!(store_id);
    }
    manifest["schema_revision"] = graph["schema_revision"].clone();
    manifest["scrolls_version"] = json!(VERSION);
    manifest["record_details"] = json!(details);
    manifest["counts"] = counts(graph, manifest["files"].as_object().unwrap().len());
    manifest["content_digest"] = json!(canonical_digest(json!({
        "records": graph["records"],
        "edges": graph["edges"],
        "schema": graph["schema"],
    }))?);
    manifest["generated"] = json!(true);
    manifest["read_only"] = json!(true);
    manifest["outside_store"] = json!(outside_store);
    Ok(())
}

#[derive(Deserialize)]
struct ManifestTarget<'a> {
    #[serde(rename = "ref")]
    reference: &'a str,
    revision: &'a str,
    format: Format,
}

#[derive(Debug, Serialize)]
pub struct Verification {
    #[serde(rename = "ref")]
    pub reference: String,
    pub revision: String,
    pub head_revision: String,
    pub stale: bool,
    pub mismatches: Vec<String>,
}

/// Compare an export with the named, immutable store snapshot. A returned
/// report can contain mismatches. Errors indicate that verification could not
/// read the manifest, resolve its target, or build the expected export.
pub fn verify_export(
    store: &dyn KnowledgeStore,
    tenant: &TenantId,
    dir: &Path,
) -> Result<Verification> {
    tenant.check_served_by(store.tenant())?;
    ensure!(
        dir.symlink_metadata()?.is_dir(),
        "export must be a directory: {}",
        dir.display()
    );
    let manifest_path = dir.join(MANIFEST);
    ensure!(
        manifest_path
            .symlink_metadata()
            .with_context(|| format!("missing {MANIFEST}"))?
            .is_file(),
        "{MANIFEST} must be a regular file"
    );
    let manifest: Value = serde_json::from_slice(&fs::read(manifest_path)?)
        .with_context(|| format!("invalid {MANIFEST}"))?;
    ensure!(
        manifest["schema_version"] == MANIFEST_VERSION,
        "{MANIFEST} requires manifest version {MANIFEST_VERSION}"
    );
    let ManifestTarget {
        reference,
        revision,
        format,
    } = ManifestTarget::deserialize(&manifest)
        .with_context(|| format!("invalid {MANIFEST} target"))?;
    ensure!(
        !revision.is_empty(),
        "{MANIFEST} contains an empty revision"
    );
    ensure!(
        store
            .head(revision)
            .with_context(|| format!("{MANIFEST} revision {revision} is unavailable"))?
            == revision,
        "{MANIFEST} revision {revision} must be an immutable revision"
    );
    ensure!(
        store.branches()?.iter().any(|name| name == reference),
        "{MANIFEST} names an unknown ref {reference}"
    );
    let expected = ExportSnapshot::read(store, tenant, reference, revision)?.build(format)?;
    let mismatches = compare_tree(dir, &manifest, &expected)?;
    let head_revision = store.head(reference)?;
    Ok(Verification {
        stale: head_revision != revision,
        reference: reference.to_owned(),
        revision: revision.to_owned(),
        head_revision,
        mismatches,
    })
}

// Name files from the directory, so a path from a tampered manifest is never
// used to open a file. Do not descend into symlinks.
fn inventory(dir: &Path, mismatches: &mut Vec<String>) -> Result<BTreeMap<String, PathBuf>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![(String::new(), dir.to_path_buf())];
    while let Some((prefix, path)) = pending.pop() {
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            let name = entry.file_name();
            let path = entry.path();
            let Some(name) = name.to_str() else {
                mismatches.push(format!(
                    "extra entry with a non-UTF-8 path: {}",
                    path.display()
                ));
                continue;
            };
            let relative = format!("{prefix}{name}");
            if entry.file_type()?.is_dir() {
                pending.push((format!("{relative}/"), path));
            } else {
                files.insert(relative, path);
            }
        }
    }
    Ok(files)
}

fn compare_tree(dir: &Path, manifest: &Value, expected: &Export) -> Result<Vec<String>> {
    let mut mismatches = Vec::new();
    let mut files = inventory(dir, &mut mismatches)?;
    files.remove(MANIFEST);
    let file_count = files.len() - usize::from(files.contains_key(MARKER));
    let mut observed_counts = json!({"files": file_count});
    let outside_store: BTreeSet<String> =
        serde_json::from_value(expected.manifest["outside_store"].clone())?;
    let recorded_external_hashes = outside_store
        .iter()
        .filter(|path| {
            manifest["files"][path.as_str()]
                .as_str()
                .is_some_and(|hash| {
                    hash.len() == 64
                        && hash
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
        })
        .map(String::as_str)
        .collect::<Vec<_>>();
    compare_fields(
        MANIFEST,
        manifest,
        &expected.manifest,
        &["files", "records", "record_details", "counts"],
        &mut mismatches,
    );
    for key in ["records", "record_details"] {
        compare_fields(
            &format!("{MANIFEST} {key}"),
            &manifest[key],
            &expected.manifest[key],
            &[],
            &mut mismatches,
        );
    }
    compare_fields(
        &format!("{MANIFEST} files"),
        &manifest["files"],
        &expected.manifest["files"],
        &recorded_external_hashes,
        &mut mismatches,
    );
    for path in expected.files.keys().map(String::as_str).chain([MARKER]) {
        let Some(target) = files.remove(path) else {
            mismatches.push(format!("missing file {path}"));
            continue;
        };
        if !target.symlink_metadata()?.is_file() {
            mismatches.push(format!("invalid file {path}: expected a regular file"));
            continue;
        }
        let bytes = match fs::read(&target) {
            Ok(bytes) => bytes,
            Err(error) => {
                mismatches.push(format!("cannot read {path}: {error}"));
                continue;
            }
        };
        let matches = if path == MARKER {
            bytes == expected.marker.as_bytes()
        } else if outside_store.contains(path) {
            manifest["files"][path]
                .as_str()
                .is_some_and(|hash| digest(&bytes) == hash)
        } else {
            bytes == expected.files[path]
        };
        if !matches {
            mismatches.push(format!("altered file {path}"));
        }
        if path == GRAPH {
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(graph) => {
                    compare_fields(
                        &format!("{GRAPH} records"),
                        &graph["records"],
                        &expected.graph["records"],
                        &[],
                        &mut mismatches,
                    );
                    compare_fields(
                        &format!("{GRAPH} edges"),
                        &graph["edges"],
                        &expected.graph["edges"],
                        &[],
                        &mut mismatches,
                    );
                    observed_counts = counts(&graph, file_count);
                }
                Err(error) => mismatches.push(format!("invalid {GRAPH}: {error}")),
            }
        }
    }
    mismatches.extend(files.keys().map(|path| format!("extra file {path}")));
    compare_fields(
        &format!("{MANIFEST} counts"),
        &manifest["counts"],
        &expected.manifest["counts"],
        &["records_by_kind"],
        &mut mismatches,
    );
    compare_fields(
        &format!("{MANIFEST} counts records_by_kind"),
        &manifest["counts"]["records_by_kind"],
        &expected.manifest["counts"]["records_by_kind"],
        &[],
        &mut mismatches,
    );
    for (key, observed) in observed_counts.as_object().unwrap() {
        if manifest["counts"].get(key) != Some(observed) {
            mismatches.push(format!(
                "{MANIFEST} counts {key} disagrees with the export tree"
            ));
        }
    }
    Ok(mismatches)
}

fn compare_fields(
    label: &str,
    actual: &Value,
    expected: &Value,
    skip: &[&str],
    mismatches: &mut Vec<String>,
) {
    let Some(actual) = actual.as_object() else {
        mismatches.push(format!("{label} must be an object"));
        return;
    };
    let expected = expected.as_object().unwrap();
    for key in actual
        .keys()
        .chain(expected.keys().filter(|&key| !actual.contains_key(key)))
    {
        if skip.contains(&key.as_str()) {
            continue;
        }
        let mismatch = match (actual.get(key), expected.get(key)) {
            (Some(actual), Some(expected)) if actual != expected => "altered",
            (Some(_), None) => "extra",
            (None, Some(_)) => "missing",
            _ => continue,
        };
        mismatches.push(format!("{label}: {mismatch} {key}"));
    }
}
