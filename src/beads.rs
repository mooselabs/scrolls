//! Explicit Beads I/O. Graph reads never start a subprocess.
use crate::{
    cli::{BeadsCommand, Cli, Target},
    model::*,
    storage::Store,
    work_ref::*,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Binding {
    pub project_id: String,
    pub directory: PathBuf,
    pub bd: PathBuf,
    pub timeout: u64,
}
#[derive(Default, Serialize, Deserialize)]
pub struct Local {
    #[serde(default)]
    pub bindings: BTreeMap<String, Binding>,
    #[serde(default)]
    pub cache: BTreeMap<String, Value>,
    #[serde(default)]
    pub publications: BTreeMap<String, Value>,
    #[serde(default)]
    pub operations: BTreeMap<String, Value>,
}
pub fn read_json<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).with_context(|| format!("invalid {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e.into()),
    }
}
pub fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut f = tempfile::NamedTempFile::new_in(path.parent().context("missing parent")?)?;
    f.write_all(&serde_json::to_vec_pretty(value)?)?;
    f.as_file().sync_all()?;
    f.persist(path)?;
    fs::File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}
impl Local {
    pub fn load(root: &Path) -> Result<Self> {
        read_json(&root.join("beads.json"))
    }
    fn save(&self, root: &Path) -> Result<()> {
        save(&root.join("beads.json"), self)
    }
    pub fn resolve(&self, text: &str) -> Result<BeadRef> {
        if text.starts_with("beads://") {
            return BeadRef::parse(text);
        }
        let (name, issue) = text
            .split_once(':')
            .context("use NAME:FULL-ISSUE-ID or beads://PROJECT-UUID/FULL-ISSUE-ID")?;
        let binding = self
            .bindings
            .get(name)
            .context("unknown Beads binding; use beads bind")?;
        BeadRef::parse(&format!("beads://{}/{issue}", binding.project_id))
    }
    fn binding(&self, reference: &BeadRef) -> Result<&Binding> {
        self.bindings
            .values()
            .find(|b| b.project_id == reference.project_id)
            .context("unbound Beads project; use beads bind")
    }
    pub fn summary(&self, reference: &str) -> Value {
        self.cache
            .get(reference)
            .cloned()
            .unwrap_or_else(|| json!({"reference":reference,"resolution":"unresolved"}))
    }
}

fn executable(path: &Path) -> Result<PathBuf> {
    if path.components().count() > 1 {
        return Ok(fs::canonicalize(path)?);
    }
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let candidate = dir.join(path);
        if candidate.is_file() {
            return Ok(fs::canonicalize(candidate)?);
        }
    }
    bail!("Beads executable {} not found", path.display())
}
impl Binding {
    fn call(&self, args: &[String], read_only: bool, actor: Option<&str>) -> Result<Value> {
        let mut out = tempfile::tempfile()?;
        let mut err = tempfile::tempfile()?;
        let mut cmd = Command::new(&self.bd);
        cmd.current_dir(&self.directory)
            .args(["--json", "--sandbox", "--no-color"]);
        // Ambient workspace selectors must not redirect an explicit binding.
        for key in [
            "BEADS_DIR",
            "BEADS_DB",
            "BEADS_DOLT_SERVER_DATABASE",
            "BEADS_DATABASE",
            "BEADS_DOLT_SERVER_HOST",
            "BEADS_DOLT_SERVER_PORT",
            "BEADS_DOLT_PORT",
        ] {
            cmd.env_remove(key);
        }
        cmd.env("BEADS_ROUTING_MODE", "explicit")
            .env("BEADS_ROUTING_DEFAULT", ".")
            .env("BD_LAST_TOUCHED_FALLBACK", "0");
        if read_only {
            cmd.arg("--readonly");
        }
        if let Some(actor) = actor {
            cmd.args(["--actor", actor]);
        }
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(err.try_clone()?);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("cannot start {}", self.bd.display()))?;
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if start.elapsed() >= Duration::from_secs(self.timeout) {
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "Beads timed out after {}s; outcome may be unknown, retry to reconcile",
                    self.timeout
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        out.seek(SeekFrom::Start(0))?;
        err.seek(SeekFrom::Start(0))?;
        let mut stdout = String::new();
        let mut stderr = String::new();
        out.take(16 * 1024 * 1024).read_to_string(&mut stdout)?;
        err.take(8192).read_to_string(&mut stderr)?;
        ensure!(
            status.success(),
            "Beads failed: {} {}",
            stderr.trim(),
            stdout.chars().take(2048).collect::<String>()
        );
        serde_json::from_str(&stdout).context("invalid Beads JSON response")
    }
    fn identity(&self) -> Result<String> {
        let version = self.call(&["version".into()], true, None)?;
        ensure!(
            version["version"] == "1.3.0",
            "Beads 1.3.0 is qualified; select that binary with beads bind --bd"
        );
        let location = self.call(&["where".into()], true, None)?;
        let path = PathBuf::from(
            location["path"]
                .as_str()
                .context("bd where did not return path")?,
        );
        let metadata: Value = read_json(&path.join("metadata.json"))?;
        let id = metadata["project_id"].as_str().context(
            "Beads project has no persistent project_id; initialize it with Beads before binding",
        )?;
        Ok(uuid::Uuid::parse_str(id)?.to_string())
    }
    fn verify(&self) -> Result<()> {
        ensure!(
            self.identity()? == self.project_id,
            "Beads project identity mismatch; binding was not changed"
        );
        Ok(())
    }
    fn list(&self, flags: &[String]) -> Result<Vec<Value>> {
        self.verify()?;
        let mut args: Vec<String> = [
            "list",
            "--all",
            "--include-infra",
            "--include-gates",
            "--limit",
            "0",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        args.extend_from_slice(flags);
        let result = self.call(&args, true, None)?;
        Ok(result
            .as_array()
            .context("expected Beads issue array")?
            .clone())
    }
    fn issue(&self, reference: &BeadRef) -> Result<Value> {
        let rows = self.list(&["--id".into(), reference.issue_id.clone()])?;
        let matches: Vec<_> = rows
            .into_iter()
            .filter(|v| v["id"] == reference.issue_id)
            .collect();
        ensure!(
            matches.len() == 1,
            "exact Beads issue {} not found in bound project",
            reference.issue_id
        );
        Ok(matches.into_iter().next().unwrap())
    }
}

pub fn refs(g: &Graph, reference: &str) -> Vec<Value> {
    let mut grouped: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for e in g
        .edges
        .values()
        .filter(|e| e.to == reference && relation(&e.rel))
    {
        grouped.entry(&e.from).or_default().push(&e.rel);
    }
    grouped.into_iter().filter_map(|(id, mut relations)| {
        let r = g.records.get(id)?;
        relations.sort(); relations.dedup();
        Some(json!({"id":r.id,"title":r.title,"kind":r.kind,"status":r.status,"revision":r.revision,"relations":relations}))
    }).collect()
}
pub fn related(g: &Graph, id: &str, local: &Local) -> Vec<Value> {
    g.edges
        .values()
        .filter(|e| e.from == id && e.to.starts_with("beads://"))
        .map(|e| json!({"relation":e.rel,"reference":e.to,"bead":local.summary(&e.to)}))
        .collect()
}
fn targets(g: &Graph, local: &Local, target: &Target) -> Result<BTreeSet<BeadRef>> {
    if let Some(r) = &target.reference {
        return Ok(BTreeSet::from([local.resolve(r)?]));
    }
    g.edges
        .values()
        .filter(|e| e.to.starts_with("beads://"))
        .map(|e| BeadRef::parse(&e.to))
        .collect()
}
pub fn run(cli: &Cli, store: &Store, graph: &Graph, command: &BeadsCommand) -> Result<Value> {
    let mut local = Local::load(&store.root)?;
    let revision = store.revision()?;
    match command {
        BeadsCommand::Bind {
            name,
            directory,
            bd,
            timeout,
        } => {
            ensure!(
                !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c)),
                "binding name must contain letters, digits, - or _"
            );
            ensure!(
                (1..=300).contains(timeout),
                "timeout must be 1..300 seconds"
            );
            let mut binding = Binding {
                project_id: String::new(),
                directory: fs::canonicalize(directory)?,
                bd: executable(bd)?,
                timeout: *timeout,
            };
            binding.project_id = binding.identity()?;
            if let Some(old) = local.bindings.get(name) {
                ensure!(
                    old.project_id == binding.project_id,
                    "cannot rebind a name to another project"
                );
            }
            let result = json!({"name":name,"binding":binding,"dry_run":cli.dry_run});
            local.bindings.insert(name.clone(), binding);
            if !cli.dry_run {
                local.save(&store.root)?;
            }
            Ok(result)
        }
        BeadsCommand::List => Ok(
            json!({"bindings":local.bindings,"cache":local.cache,"publications":local.publications}),
        ),
        BeadsCommand::Refs { reference } => {
            let reference = local.resolve(reference)?.uri();
            Ok(
                json!({"reference":reference,"bead":local.summary(&reference),"documents":refs(graph,&reference)}),
            )
        }
        BeadsCommand::Refresh(target) => {
            let mut results = vec![];
            for reference in targets(graph, &local, target)? {
                let uri = reference.uri();
                let fetched = local.binding(&reference).and_then(|b| b.issue(&reference));
                let mut summary = local.summary(&uri);
                summary["last_attempt_at"] = json!(now());
                match fetched {
                    Ok(issue) => {
                        summary = json!({"reference":uri,"resolution":"cached","title":issue["title"],"status":issue["status"],"issue_updated_at":issue["updated_at"],"observed_at":now(),"last_attempt_at":now()});
                    }
                    Err(e) => {
                        summary["resolution"] = json!("lookup_failed");
                        summary["error"] = json!(format!("{e:#}"));
                    }
                }
                results.push(summary.clone());
                local.cache.insert(uri, summary);
            }
            if !cli.dry_run {
                local.save(&store.root)?;
            }
            Ok(
                json!({"failed":results.iter().filter(|r|r["error"].is_string()).count(),"results":results,"dry_run":cli.dry_run}),
            )
        }
        BeadsCommand::Publish(target) => {
            ensure!(cli.proposal.is_none(), "publish uses main; omit --proposal");
            let actor = cli
                .actor
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .context("publish requires --actor NAME")?;
            let store_id: String = store
                .one("metadata", "store_id")?
                .context("run scrolls migrate first")?;
            let key = format!("scrolls/{}", uuid::Uuid::parse_str(&store_id)?.simple());
            let mut targets = targets(graph, &local, target)?;
            let mut results = vec![];
            if target.all {
                let mut seen = BTreeSet::new();
                for binding in local.bindings.values() {
                    if !seen.insert(&binding.project_id) {
                        continue;
                    }
                    match binding.list(&["--has-metadata-key".into(), key.clone()]) {
                        Ok(rows) => {
                            for row in rows {
                                let id = row["id"].as_str().context("Beads issue missing id")?;
                                targets.insert(BeadRef::parse(&format!(
                                    "beads://{}/{id}",
                                    binding.project_id
                                ))?);
                            }
                        }
                        Err(e) => results.push(
                            json!({"project_id":binding.project_id,"error":format!("{e:#}")}),
                        ),
                    }
                }
                // Retain previously published targets even after local links are removed.
                for (uri, receipt) in &local.publications {
                    // Failed lookups and completed removals are history, not outstanding
                    // remote projections. Discovery covers uncertain writes after crashes.
                    let result = &receipt["result"];
                    if result["verified"] == true && result["projection"].is_object() {
                        targets.insert(BeadRef::parse(uri)?);
                    }
                }
            }
            let op = cli
                .op_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let hash = digest(serde_json::to_vec(
                &json!({"command":command,"actor":actor,"revision":revision}),
            )?);
            // Detect collisions with both graph-operation and publication receipts.
            let _ = store.receipt(&op, &hash)?;
            if let Some(old) = local.operations.get(&op) {
                ensure!(
                    old["hash"] == hash,
                    "--op-id already used with different arguments or Scrolls revision"
                );
            }
            if !cli.dry_run {
                local.operations.insert(
                    op.clone(),
                    json!({"hash":hash,"revision":revision,"started_at":now()}),
                );
                local.save(&store.root)?;
            }
            for reference in targets {
                let uri = reference.uri();
                let outcome = (|| -> Result<Value> {
                    let binding = local.binding(&reference)?;
                    let issue = binding.issue(&reference)?;
                    let mut documents = refs(graph, &uri);
                    for doc in &mut documents {
                        doc["uri"] = json!(document_uri(&store_id, doc["id"].as_str().unwrap()));
                    }
                    let desired = if documents.is_empty() {
                        Value::Null
                    } else {
                        json!({"schema_version":1,"store_id":store_id,"source_commit":revision,"documents":documents})
                    };
                    let current = &issue["metadata"][&key];
                    // Unrelated Scrolls commits do not republish identical document revisions.
                    let equal = if desired.is_null() {
                        current.is_null()
                    } else {
                        current["schema_version"] == 1
                            && current["store_id"] == store_id
                            && current["documents"] == desired["documents"]
                    };
                    let action = if equal {
                        "unchanged"
                    } else if desired.is_null() {
                        "remove"
                    } else {
                        "update"
                    };
                    if !cli.dry_run && !equal {
                        let mut args = vec!["update".into(), reference.issue_id.clone()];
                        if desired.is_null() {
                            args.extend(["--unset-metadata".into(), key.clone()]);
                        } else {
                            args.extend([
                                "--metadata".into(),
                                serde_json::to_string(&json!({&key:desired}))?,
                            ]);
                        }
                        binding.call(&args, false, Some(actor))?;
                        let verified = binding.issue(&reference)?;
                        ensure!(
                            verified["metadata"][&key] == desired,
                            "publication readback mismatch; retry to reconcile"
                        );
                    }
                    Ok(
                        json!({"reference":uri,"action":action,"projection":if equal { current.clone() } else { desired },"verified":!cli.dry_run,"source_commit":revision}),
                    )
                })();
                let row = match outcome {
                    Ok(row) => row,
                    Err(e) => json!({"reference":uri,"error":format!("{e:#}")}),
                };
                if !cli.dry_run {
                    local
                        .publications
                        .insert(uri, json!({"op_id":op,"attempted_at":now(),"result":row}));
                    local.save(&store.root)?;
                }
                results.push(row);
            }
            let result = json!({"op_id":op,"source_commit":revision,"results":results,"failed":results.iter().filter(|r| r["error"].is_string()).count(),"dry_run":cli.dry_run});
            if !cli.dry_run {
                local.operations.get_mut(&op).unwrap()["result"] = result.clone();
                local.save(&store.root)?;
            }
            Ok(result)
        }
    }
}
