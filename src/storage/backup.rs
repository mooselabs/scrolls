//! Database-only backup proof. No service/account configuration is restored here.
use crate::cli::{BackupCommand, Cli};
use anyhow::Result;
use serde_json::Value;

pub fn run(cli: &Cli, command: &BackupCommand) -> Result<Value> {
    #[cfg(target_os = "linux")]
    let result = linux::run(cli, command);
    #[cfg(not(target_os = "linux"))]
    let result = {
        let _ = (cli, command);
        Err(anyhow::anyhow!(
            "backup_host_required: backup administration requires the Linux SQL host"
        ))
    };
    result.map_err(|error| {
        let message = error.to_string();
        let io = error.chain().find_map(|cause| cause.downcast_ref::<std::io::Error>()).map(std::io::Error::kind);
        if message.starts_with("backup_") {
            if let Some(kind) = io { anyhow::anyhow!("{message} ({kind:?})") } else { anyhow::anyhow!(message) }
        } else if message.contains("publication_lock_timeout") {
            anyhow::anyhow!("backup_lock_timeout: another client holds the publication lock")
        } else if message.contains("dirty_state") || message.contains("unexplained dirty") {
            anyhow::anyhow!("backup_source_dirty: inspect source state without discarding it")
        } else if message.contains("unsupported Dolt version") {
            anyhow::anyhow!("backup_tool_version: Dolt 2.2.0 required")
        } else if let Some(kind) = io {
            anyhow::anyhow!("backup_io_failed: {kind:?}")
        } else {
            // Malformed stored rows and SQL errors can contain document values.
            anyhow::anyhow!("backup_source_failed: inspect the source connection, doctor and any incomplete private output")
        }
    })
}

pub fn diagnostic(error: &anyhow::Error) -> Value {
    let message = error.to_string();
    let code = message
        .split(':')
        .next()
        .filter(|s| s.starts_with("backup_"))
        .unwrap_or("backup_failed");
    serde_json::json!({"code":code,"severity":"error","reason":message,"remedy":"Preserve source state and incomplete outputs. Correct the reported phase, then use a fresh destination."})
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::{
        model::{ImportInfo, digest, now},
        policy,
        storage::{
            DOLT_VERSION, Store, literal, session::SqlSession, shared_review::snapshot_fingerprint,
        },
    };
    use anyhow::{Context, anyhow, bail, ensure};
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::{
        collections::BTreeMap,
        fs::{self, File, OpenOptions},
        io::{Read, Seek, SeekFrom, Write},
        os::{
            fd::AsRawFd,
            unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        },
        path::{Component, Path, PathBuf},
        process::{Command, Stdio},
    };

    const EXCLUDED: &[&str] = &[
        "SQL accounts and grants",
        "branch permissions",
        "server and global configuration",
        "per-database non-versioned configuration",
        "optimizer statistics cache",
        "service activation and client bindings",
    ];
    const MAX_MANIFEST: u64 = 16 * 1024 * 1024;
    const MAX_FILES: usize = 100_000;

    /// Every subprocess/server URL uses this held inode, never a checked-then-reused pathname.
    struct Directory {
        file: File,
        path: PathBuf,
    }
    impl Directory {
        fn open(path: &Path) -> Result<Self> {
            let path = if path.is_absolute() {
                path.to_owned()
            } else {
                std::env::current_dir()?.join(path)
            };
            ensure!(path.to_str().is_some(), "backup_path: UTF-8 path required");
            let mut dir = Self {
                file: OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                    .open("/")?,
                path: PathBuf::from("/"),
            };
            for part in path.components() {
                match part {
                    Component::RootDir | Component::CurDir => {}
                    Component::Normal(name) => {
                        dir = dir.open_child(name)?;
                    }
                    _ => bail!("backup_path: parent traversal is not supported"),
                }
            }
            Ok(dir)
        }
        fn duplicate(&self) -> Result<Self> {
            Ok(Self {
                file: self.file.try_clone()?,
                path: self.path.clone(),
            })
        }
        fn anchor(&self) -> PathBuf {
            PathBuf::from(format!(
                "/proc/{}/fd/{}",
                std::process::id(),
                self.file.as_raw_fd()
            ))
        }
        fn url(&self) -> String {
            format!("file://{}", self.anchor().display())
        }
        fn open_child(&self, name: &std::ffi::OsStr) -> Result<Self> {
            Ok(Self {
                file: OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                    .open(self.anchor().join(name))?,
                path: self.path.join(name),
            })
        }
        fn create_child(&self, name: &str) -> Result<Self> {
            // Open an unpredictable private staging directory before publishing
            // its name. The destination is never reused or overwritten.
            let temporary = format!(".scrolls-backup-{}", uuid::Uuid::new_v4().simple());
            fs::DirBuilder::new()
                .mode(0o700)
                .create(self.anchor().join(&temporary))?;
            let mut child = self.open_child(std::ffi::OsStr::new(&temporary))?;
            let old = std::ffi::CString::new(temporary)?;
            let new = std::ffi::CString::new(name)?;
            // Both names are single components relative to the held parent.
            let renamed = unsafe {
                libc::renameat2(
                    self.file.as_raw_fd(),
                    old.as_ptr(),
                    self.file.as_raw_fd(),
                    new.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            };
            if renamed != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            child.path = self.path.join(name);
            self.file.sync_all()?;
            child.check()?;
            Ok(child)
        }

        fn fresh(path: &Path) -> Result<Self> {
            let parent = Self::open(
                path.parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            )?;
            parent.check()?;
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .context("backup_path: output needs a directory name")?;
            let dir = parent.create_child(name).context(
                "backup_destination: output must be new; incomplete outputs are never reused",
            )?;
            parent.check()?;
            dir.check()?;
            Ok(dir)
        }
        fn check(&self) -> Result<()> {
            let actual = self.file.metadata()?;
            let named = fs::symlink_metadata(&self.path)
                .context("backup_destination_changed: original path is unavailable")?;
            ensure!(
                named.is_dir() && (actual.dev(), actual.ino()) == (named.dev(), named.ino()),
                "backup_destination_changed: preserve the held directory; no verified result was published"
            );
            Ok(())
        }
        fn write(&self, name: &str, bytes: &[u8]) -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(self.anchor().join(name))?;
            file.write_all(bytes)?;
            file.sync_all()?;
            self.file.sync_all()?;
            Ok(())
        }
        fn read(&self, name: &str, limit: u64) -> Result<Vec<u8>> {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(self.anchor().join(name))?;
            let before = file.metadata()?;
            ensure!(
                before.is_file() && before.len() <= limit,
                "backup_input_limit: invalid or oversized file"
            );
            let mut bytes = vec![];
            (&file).take(limit + 1).read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() as u64 <= limit && unchanged(&before, &file.metadata()?),
                "backup_changed: file changed while reading"
            );
            Ok(bytes)
        }
        fn publish(&self, manifest: &Manifest) -> Result<()> {
            self.check()?;
            let temporary = format!(".verified-{}", uuid::Uuid::new_v4().simple());
            let bytes = serde_json::to_vec_pretty(manifest)?;
            ensure!(
                bytes.len() as u64 <= MAX_MANIFEST,
                "backup_input_limit: proof exceeds 16 MiB"
            );
            self.write(&temporary, &bytes)?;
            self.check()?;
            fs::hard_link(
                self.anchor().join(&temporary),
                self.anchor().join("verified.json"),
            )?;
            if let Err(error) = self.check() {
                let a = fs::symlink_metadata(self.anchor().join(&temporary))?;
                let b = fs::symlink_metadata(self.anchor().join("verified.json"))?;
                if (a.dev(), a.ino()) == (b.dev(), b.ino()) {
                    fs::remove_file(self.anchor().join("verified.json"))?;
                    self.file.sync_all()?;
                }
                return Err(error);
            }
            fs::remove_file(self.anchor().join(temporary))?;
            self.file.sync_all()?;
            Ok(())
        }
    }
    fn unchanged(a: &fs::Metadata, b: &fs::Metadata) -> bool {
        (
            a.dev(),
            a.ino(),
            a.len(),
            a.mtime(),
            a.mtime_nsec(),
            a.ctime(),
            a.ctime_nsec(),
        ) == (
            b.dev(),
            b.ino(),
            b.len(),
            b.mtime(),
            b.mtime_nsec(),
            b.ctime(),
            b.ctime_nsec(),
        )
    }

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct Branch {
        head: String,
        root: String,
        fingerprint: String,
    }
    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct Tag {
        commit: String,
        metadata_digest: String,
    }
    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct Inventory {
        store_id: String,
        format: u32,
        writer: u32,
        schema_revision: String,
        branches: BTreeMap<String, Branch>,
        tags: BTreeMap<String, Tag>,
        remote_refs: BTreeMap<String, String>,
    }
    fn refs(s: &Store, table: &str, name: &str, hash: &str) -> Result<BTreeMap<String, String>> {
        let mut out = BTreeMap::new();
        for row in s.sql(&format!(
            "SELECT {name} AS name,{hash} AS hash FROM {table} ORDER BY {name}"
        ))? {
            let name = row["name"].as_str().context("backup_ref_identity")?;
            let hash = row["hash"].as_str().context("backup_ref_hash")?;
            ensure!(
                hash.len() == 32 && hash.bytes().all(|b| b.is_ascii_alphanumeric()),
                "backup_ref_hash"
            );
            ensure!(
                out.insert(name.into(), hash.into()).is_none(),
                "backup_duplicate_ref"
            );
        }
        Ok(out)
    }
    fn select(s: &Store, branch: &str) -> Result<()> {
        ensure!(!branch.starts_with('-'), "backup_unsupported_branch_name");
        if s.is_shared() {
            s.select_shared(branch)?;
        } else {
            ensure!(
                branch
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b)),
                "backup_unsupported_branch_name"
            );
            s.sql(&format!("USE `database/{branch}`"))?;
        }
        SqlSession::clean(s)
    }
    fn inventory(s: &Store) -> Result<Inventory> {
        select(s, "main")?;
        ensure!(
            s.sql("SELECT * FROM dolt_stashes")?.is_empty(),
            "backup_unsupported_refs: nonempty stashes require separate qualification; preserve them"
        );
        let store_id: String = s
            .one("metadata", "store_id")?
            .context("backup_store_identity")?;
        uuid::Uuid::parse_str(&store_id)?;
        let format = s
            .one("metadata", "schema")?
            .context("backup_store_format")?;
        let writer = s
            .one("metadata", "shared_writer")?
            .context("backup_writer")?;
        ensure!(
            format == 5 && writer == 4 && s.one::<bool>("metadata", "shared_ready")? == Some(true),
            "backup_unsupported_format: shared format 5 writer 4 required"
        );
        let schema_revision = s.graph()?.schema.revision();
        let local = refs(s, "dolt_branches", "name", "hash")?;
        ensure!(
            local.contains_key("main") && local.contains_key(super::super::shared_review::JOURNAL),
            "backup_missing_required_branch"
        );
        let mut tags = BTreeMap::new();
        // tag_hash is the target commit, not the annotated tag object's hash.
        for row in s.sql("SELECT tag_name,tag_hash,SHA2(CAST(JSON_ARRAY(tagger,email,CAST(`date` AS CHAR),message) AS CHAR),256) AS metadata_digest FROM dolt_tags ORDER BY tag_name")? {
            let name = row["tag_name"].as_str().context("backup_tag_identity")?;
            let tag = Tag { commit: row["tag_hash"].as_str().context("backup_tag_commit")?.into(), metadata_digest: row["metadata_digest"].as_str().context("backup_tag_metadata")?.into() };
            ensure!(tags.insert(name.to_owned(), tag).is_none(), "backup_duplicate_tag");
        }
        let remote_refs = refs(s, "dolt_remote_branches", "name", "hash")?;
        let mut branches = BTreeMap::new();
        for (name, head) in local {
            ensure!(
                !name.starts_with("dolt_rebase_"),
                "backup_source_dirty: native rebase branch requires explicit resolution"
            );
            select(s, &name)?;
            let merging = s.sql("SELECT is_merging FROM dolt_merge_status")?;
            ensure!(
                merging.len() == 1
                    && (merging[0]["is_merging"] == json!(false)
                        || merging[0]["is_merging"] == json!(0)),
                "backup_source_dirty: active merge state is not qualified"
            );
            ensure!(
                s.one::<String>("metadata", "store_id")?.as_deref() == Some(&store_id)
                    && s.one::<u32>("metadata", "schema")? == Some(format)
                    && s.one::<u32>("metadata", "shared_writer")? == Some(writer),
                "backup_unsupported_branch_format"
            );
            let graph = s.graph_at(&head)?;
            graph
                .schema
                .validate()
                .map_err(|_| anyhow!("backup_graph_invalid: inspect source schema"))?;
            let debt = s
                .one_at::<ImportInfo>("metadata", "import", &head)?
                .map(|i| i.lint_debt)
                .unwrap_or_default();
            ensure!(
                policy::lint(&graph).is_subset(&debt),
                "backup_graph_invalid: inspect source lint"
            );
            crate::markdown_import::health(s, &graph, Some(&head))
                .map_err(|_| anyhow!("backup_source_health: inspect Markdown originals"))?;
            crate::source_import::health(&graph, &s.import_files_at(&head)?)
                .map_err(|_| anyhow!("backup_source_health: inspect source provenance"))?;
            ensure!(
                s.revision()? == head,
                "backup_source_changed: branch advanced during inventory"
            );
            let roots = s.sql("SELECT DOLT_HASHOF_DB('HEAD') AS root")?;
            let root = roots
                .first()
                .and_then(|row| row["root"].as_str())
                .context("backup_root_hash")?
                .to_owned();
            branches.insert(
                name,
                Branch {
                    fingerprint: snapshot_fingerprint(s, &head)?,
                    head,
                    root,
                },
            );
        }
        select(s, "main")?;
        Ok(Inventory {
            store_id,
            format,
            writer,
            schema_revision,
            branches,
            tags,
            remote_refs,
        })
    }

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct FileStamp {
        bytes: u64,
        sha256: String,
        mode: u32,
    }
    fn files(dir: &Directory, sync: bool) -> Result<BTreeMap<String, FileStamp>> {
        fn walk(
            dir: &Directory,
            prefix: &str,
            depth: usize,
            out: &mut BTreeMap<String, FileStamp>,
            sync: bool,
            entries: &mut usize,
        ) -> Result<()> {
            ensure!(depth <= 32, "backup_input_limit: directory depth exceeded");
            for entry in fs::read_dir(dir.anchor())? {
                let entry = entry?;
                *entries += 1;
                ensure!(
                    *entries <= MAX_FILES,
                    "backup_input_limit: too many entries"
                );
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow!("backup_path: UTF-8 entry required"))?;
                let path = format!("{prefix}{name}");
                if entry.file_type()?.is_dir() {
                    let child = dir.open_child(std::ffi::OsStr::new(&name))?;
                    walk(&child, &(path + "/"), depth + 1, out, sync, entries)?;
                } else {
                    ensure!(
                        entry.file_type()?.is_file(),
                        "backup_unsafe_file: symlinks and special files are refused"
                    );
                    let mut file = OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                        .open(dir.anchor().join(&name))?;
                    let before = file.metadata()?;
                    ensure!(before.is_file(), "backup_changed: file type changed");
                    let mut hasher = Sha256::new();
                    let mut bytes = 0;
                    let mut buffer = [0u8; 65536];
                    loop {
                        let n = file.read(&mut buffer)?;
                        if n == 0 {
                            break;
                        }
                        hasher.update(&buffer[..n]);
                        bytes += n as u64;
                        ensure!(
                            bytes <= before.len(),
                            "backup_changed: file grew while hashing"
                        );
                    }
                    ensure!(
                        unchanged(&before, &file.metadata()?) && before.len() == bytes,
                        "backup_changed: file changed while hashing"
                    );
                    if sync {
                        file.sync_all()?;
                    }
                    ensure!(
                        out.insert(
                            path,
                            FileStamp {
                                bytes,
                                sha256: hex::encode(hasher.finalize()),
                                mode: before.mode() & 0o777
                            }
                        )
                        .is_none(),
                        "backup_duplicate_file"
                    );
                }
            }
            if sync {
                dir.file.sync_all()?;
            }
            Ok(())
        }
        let mut out = BTreeMap::new();
        walk(dir, "", 0, &mut out, sync, &mut 0)?;
        Ok(out)
    }
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Manifest {
        format: u32,
        kind: String,
        source_backup_digest: Option<String>,
        dolt_version: String,
        snapshot: Inventory,
        dataset_root: String,
        dataset_refs: BTreeMap<String, String>,
        dataset_map_nodes: usize,
        files: BTreeMap<String, FileStamp>,
        excluded: Vec<String>,
        verified_at: String,
        digest: String,
    }
    impl Manifest {
        fn seal(&mut self) -> Result<()> {
            self.digest.clear();
            self.digest = digest(serde_json::to_vec(self)?);
            Ok(())
        }
        fn verify(&mut self) -> Result<()> {
            ensure!(
                self.format == 1
                    && self.kind == "database-backup"
                    && self.source_backup_digest.is_none()
                    && self.dolt_version == DOLT_VERSION
                    && self.excluded == EXCLUDED
                    && self.files.len() <= MAX_FILES,
                "backup_manifest_invalid: unsupported contract"
            );
            let expected = self.digest.clone();
            self.seal()?;
            ensure!(
                self.digest == expected,
                "backup_manifest_invalid: digest differs"
            );
            Ok(())
        }
    }

    fn dataset_root(dir: &Directory) -> Result<String> {
        let bytes = dir.read("manifest", MAX_MANIFEST)?;
        let text = std::str::from_utf8(&bytes)
            .context("backup_manifest_invalid: native manifest encoding")?;
        let parts: Vec<_> = text.trim().split(':').collect();
        ensure!(
            parts.len() >= 5 && parts[0] == "5" && parts[1] == "__DOLT__",
            "backup_unsupported_format: native manifest requires version 5 / __DOLT__"
        );
        let hash = parts[3];
        ensure!(
            valid_hash(hash),
            "backup_manifest_invalid: native root hash"
        );
        Ok(hash.into())
    }
    fn valid_hash(hash: &str) -> bool {
        hash.len() == 32
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'v').contains(&b))
    }
    struct NativeRefs {
        nodes: usize,
        refs: BTreeMap<String, String>,
        working_sets: BTreeMap<String, String>,
    }
    struct VerifiedCopy {
        nodes: usize,
        inventory: Inventory,
        files: BTreeMap<String, FileStamp>,
        root: String,
        refs: BTreeMap<String, String>,
        working_sets: BTreeMap<String, String>,
    }
    struct Tool {
        root: Directory,
        binary: PathBuf,
    }
    impl Tool {
        fn new(root: Directory, binary: &Path) -> Result<Self> {
            let binary = fs::canonicalize(binary)
                .context("backup_tool_missing: provide the pinned --dolt executable")?;
            let home = root.create_child("dolt-home")?;
            let config = home.create_child(".dolt")?;
            config.write("config_global.json", br#"{"versioncheck.disabled":"true","metrics.disabled":"true","user.name":"Scrolls backup verifier","user.email":"backup@localhost"}"#)?;
            let tool = Self { root, binary };
            let version = tool.command(&["version"], &tool.root.anchor())?;
            ensure!(
                version.lines().next() == Some(format!("dolt version {DOLT_VERSION}").as_str()),
                "backup_tool_version: Dolt 2.2.0 required"
            );
            Ok(tool)
        }
        fn command(&self, args: &[&str], cwd: &Path) -> Result<String> {
            let mut command = Command::new(&self.binary);
            for (key, _) in std::env::vars_os() {
                if key
                    .to_str()
                    .is_some_and(|k| k.starts_with("DOLT_") || k.starts_with("GIT_"))
                {
                    command.env_remove(key);
                }
            }
            let phase = args.first().copied().unwrap_or("command");
            // Native show contains commit descriptions, which may contain
            // authored bodies. Capture into anonymous private files, never logs.
            let mut logs = vec![
                tempfile::tempfile_in(self.root.anchor())?,
                tempfile::tempfile_in(self.root.anchor())?,
            ];
            let mut child = command
                .args(args)
                .current_dir(cwd)
                .env("DOLT_ROOT_PATH", self.root.anchor().join("dolt-home"))
                .env("DOLT_DISABLE_EVENT_FLUSH", "1")
                .stdin(Stdio::null())
                .stdout(logs[0].try_clone()?)
                .stderr(logs[1].try_clone()?)
                .spawn()?;
            let waited = (|| -> Result<std::process::ExitStatus> {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
                loop {
                    for log in &logs {
                        ensure!(
                            log.metadata()?.len() <= MAX_MANIFEST,
                            "backup_tool_output_limit"
                        );
                    }
                    if let Some(status) = child.try_wait()? {
                        return Ok(status);
                    }
                    ensure!(
                        std::time::Instant::now() < deadline,
                        "backup_tool_timeout: native command exceeded 300 seconds"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            })();
            if waited.is_err() {
                let _ = child.kill();
                let _ = child.wait();
            }
            ensure!(
                waited?.success(),
                "backup_tool_failed: pinned Dolt {phase} failed; no verified result"
            );
            logs[0].seek(SeekFrom::Start(0))?;
            let mut bytes = Vec::new();
            (&logs[0]).take(MAX_MANIFEST + 1).read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() as u64 <= MAX_MANIFEST,
                "backup_tool_output_limit"
            );
            String::from_utf8(bytes).context("backup_tool_failed: invalid response")
        }

        fn dataset_refs(&self, database: &Directory, root: &str) -> Result<NativeRefs> {
            let mut pending = vec![(root.to_owned(), 0usize)];
            let mut seen = std::collections::BTreeSet::new();
            let mut objects = BTreeMap::new();
            let mut refs = BTreeMap::new();
            let mut working_sets = BTreeMap::new();
            while let Some((hash, depth)) = pending.pop() {
                ensure!(
                    depth <= 32 && seen.insert(hash.clone()) && seen.len() <= 1024,
                    "backup_unsupported_refs: excessive or repeated address-map node"
                );
                let text = if let Some(text) = objects.remove(&hash) {
                    text
                } else {
                    self.command(&["show", "--no-pretty", &hash], &database.anchor())?
                };
                let text = text.trim();
                let body = if depth == 0 {
                    text.strip_prefix("SerialMessage StoreRoot{AddressMap {\n")
                        .and_then(|s| s.strip_suffix("}}"))
                } else {
                    text.strip_prefix("SerialMessage AddressMap {\n")
                        .and_then(|s| s.strip_suffix('}'))
                }
                .context(
                    "backup_unsupported_refs: unrecognized native address-map representation",
                )?;
                for line in body.lines().filter(|line| !line.trim().is_empty()) {
                    let (name, child) = line
                        .trim()
                        .split_once(": #")
                        .context("backup_unsupported_refs: unrecognized native ref entry")?;
                    ensure!(
                        valid_hash(child) && !name.is_empty(),
                        "backup_unsupported_refs: invalid native ref entry"
                    );
                    let content = if let Some(text) = objects.get(child) {
                        text.clone()
                    } else {
                        let text =
                            self.command(&["show", "--no-pretty", child], &database.anchor())?;
                        objects.insert(child.to_owned(), text.clone());
                        text
                    };
                    if content.starts_with("SerialMessage AddressMap {") {
                        // Internal-node keys are boundaries, never dataset names.
                        pending.push((child.into(), depth + 1));
                    } else {
                        ensure!(
                            content.starts_with("SerialMessage {"),
                            "backup_unsupported_refs: unqualified ref object"
                        );
                        if name.starts_with("workingSets/") {
                            ensure!(
                                content
                                    .lines()
                                    .filter(|line| line.trim_start().starts_with("Time: "))
                                    .count()
                                    == 1,
                                "backup_unsupported_refs: unrecognized working-set metadata"
                            );
                            working_sets.insert(
                                name.to_owned(),
                                content
                                    .lines()
                                    .filter(|line| !line.trim_start().starts_with("Time: "))
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            );
                        }
                        ensure!(
                            refs.len() < MAX_FILES
                                && refs.insert(name.into(), child.into()).is_none(),
                            "backup_unsupported_refs: duplicate or excessive refs"
                        );
                    }
                }
            }
            Ok(NativeRefs {
                refs,
                working_sets,
                nodes: seen.len(),
            })
        }
        fn restore(&self, snapshot: &Directory) -> Result<VerifiedCopy> {
            let root = dataset_root(snapshot)?;
            self.command(
                &["backup", "restore", &snapshot.url(), "database"],
                &self.root.anchor(),
            )?;
            let database = self.root.open_child(std::ffi::OsStr::new("database"))?;
            let dolt = database.open_child(std::ffi::OsStr::new(".dolt"))?;
            let noms = dolt.open_child(std::ffi::OsStr::new("noms"))?;
            ensure!(
                dataset_root(&noms)? == root,
                "backup_restore_mismatch: complete dataset root differs"
            );
            let before_files = files(&database, true)?;
            self.command(&["fsck", "--quiet"], &database.anchor())?;
            let native = self.dataset_refs(&database, &root)?;
            let refs = native.refs;
            let session = super::super::inspection::Session::start(
                &self.binary,
                &database.anchor(),
                &self.root.anchor().join("dolt-home"),
                &self.root.anchor().join("native-sql.stderr"),
            )?;
            let store = Store {
                root: self.root.anchor(),
                db: database.anchor(),
                binary: self.binary.clone(),
                _lock: None,
                shared: None,
                inspection: Some(Box::new(std::cell::RefCell::new(session))),
            };
            let found = inventory(&store).map_err(|_| anyhow!("backup_restore_invalid: restored copy failed schema, graph, provenance or native inventory checks"))?;
            store
                .inspection
                .as_ref()
                .context("backup_inspection_missing")?
                .borrow_mut()
                .finish()?;
            drop(store);
            ensure!(
                found
                    .branches
                    .keys()
                    .all(|name| refs.contains_key(&format!("refs/heads/{name}")))
                    && found
                        .tags
                        .keys()
                        .all(|name| refs.contains_key(&format!("refs/tags/{name}")))
                    && found
                        .remote_refs
                        .keys()
                        .all(|name| refs.contains_key(&format!("refs/{name}"))),
                "backup_unsupported_refs: native map omitted a SQL-visible ref"
            );
            for (name, hash) in &refs {
                let expected = if let Some(branch) = name.strip_prefix("refs/heads/") {
                    found.branches.get(branch).map(|b| b.head.as_str())
                } else if let Some(remote) = name.strip_prefix("refs/remotes/") {
                    found
                        .remote_refs
                        .get(&format!("remotes/{remote}"))
                        .map(String::as_str)
                } else if name == "refs/internal/create"
                    || name
                        .strip_prefix("refs/tags/")
                        .is_some_and(|tag| found.tags.contains_key(tag))
                    || name
                        .strip_prefix("workingSets/heads/")
                        .is_some_and(|branch| found.branches.contains_key(branch))
                {
                    Some(hash.as_str())
                } else {
                    None
                };
                if expected != Some(hash.as_str()) {
                    self.root.write(
                        "ref-error.json",
                        &serde_json::to_vec_pretty(
                            &json!({"name":name,"hash":hash,"known":found}),
                        )?,
                    )?;
                    bail!(
                        "backup_unsupported_refs: inspect private ref-error.json; native dataset is outside qualified refs"
                    );
                }
            }
            ensure!(
                dataset_root(&noms)? == root,
                "backup_restore_mismatch: inspection changed complete dataset root"
            );
            let contents = files(&database, true)?;
            // Native read processes maintain their own optimizer-statistics
            // cache. It is absent from database-only native backups and is not
            // part of the restored authority. Everything else must be stable.
            let authoritative = |entries: &BTreeMap<String, FileStamp>| {
                entries
                    .iter()
                    .filter(|(name, _)| !name.starts_with(".dolt/stats/"))
                    .map(|(name, stamp)| (name.clone(), stamp.clone()))
                    .collect::<BTreeMap<_, _>>()
            };
            let stable = authoritative(&before_files) == authoritative(&contents);
            if !stable {
                self.root.write(
                    "file-change.json",
                    &serde_json::to_vec_pretty(&json!({"before":before_files,"after":contents}))?,
                )?;
            }
            ensure!(
                stable,
                "backup_changed: restored database files changed during verification"
            );
            self.root.check()?;
            Ok(VerifiedCopy {
                nodes: native.nodes,
                inventory: found,
                files: contents,
                root,
                refs,
                working_sets: native.working_sets,
            })
        }
    }
    fn response(path: &Directory, manifest: &Manifest, restore: bool) -> Result<Value> {
        let state = digest(serde_json::to_vec(&manifest.snapshot)?);
        Ok(
            json!({"schema_version":2,"ok":true,"data":{"verified":true,"database_only":true,"restored":restore,"path":path.path,"store_id":manifest.snapshot.store_id,"source_revision":manifest.snapshot.branches["main"].head,"snapshot_digest":state,"proof_digest":manifest.digest,"source_backup_digest":manifest.source_backup_digest,"excluded":manifest.excluded,"database_directory":if restore {Some(path.path.join("database"))} else {None}}}),
        )
    }
    pub(super) fn run(cli: &Cli, command: &BackupCommand) -> Result<Value> {
        ensure!(
            cli.proposal.is_none() && cli.op_id.is_none(),
            "backup_arguments: omit --proposal and --op-id; backup does not publish database changes"
        );
        ensure!(
            !cli.dry_run,
            "backup_arguments: verification requires an actual disposable restore; dry-run is not supported"
        );
        match command {
            BackupCommand::Create { out } => {
                let connection = cli.connection.as_ref().context(
                    "backup_host_required: create requires --connection on the Linux SQL host",
                )?;
                let store = Store::open_shared(connection, &cli.store)?;
                let before = inventory(&store)?;
                let output = Directory::fresh(out)?;
                output.write(
                    "attempt.json",
                    br#"{"format":1,"operation":"create","verified":false}"#,
                )?;
                let snapshot = output.create_child("snapshot")?;
                let nonce = uuid::Uuid::new_v4().to_string();
                output.write("host-nonce", nonce.as_bytes())?;
                let proof = store.sql(&format!(
                    "SELECT HEX(LOAD_FILE({})) AS proof",
                    literal(
                        output
                            .anchor()
                            .join("host-nonce")
                            .to_str()
                            .context("backup_host_path")?
                    )
                ))?;
                ensure!(
                    proof.first().and_then(|r| r["proof"].as_str())
                        == Some(hex::encode_upper(nonce.as_bytes()).as_str()),
                    "backup_host_mismatch: server cannot read this held local directory; no backup write was requested"
                );
                output.check()?;
                snapshot.check()?;
                ensure!(
                    fs::read_dir(snapshot.anchor())?.next().is_none(),
                    "backup_destination_changed: snapshot directory is no longer empty"
                );
                store.sql(&format!(
                    "CALL DOLT_BACKUP('sync-url',{})",
                    literal(&snapshot.url())
                ))?;
                output.check()?;
                snapshot.check()?;
                let snapshot_files = files(&snapshot, true)?;
                let tool = Tool::new(output.create_child("verification")?, &cli.dolt)?;
                let restored = tool.restore(&snapshot)?;
                ensure!(
                    restored.inventory == before,
                    "backup_restore_mismatch: restored snapshot differs"
                );
                // Recopy the complete native dataset root: SQL branch tables
                // cannot reveal concurrent changes to hidden ref categories.
                let recheck = output.create_child("source-recheck")?;
                output.check()?;
                store.sql(&format!(
                    "CALL DOLT_BACKUP('sync-url',{})",
                    literal(&recheck.url())
                ))?;
                let checked = Tool::new(output.create_child("recheck-verification")?, &cli.dolt)?
                    .restore(&recheck)?;
                // DOLT_BACKUP commits the selected working set before copying,
                // refreshing its timestamp even when its roots are unchanged.
                // Preserve exact root identity backup→restore; between source
                // copies allow only that timestamp change in known working sets.
                let stable_refs = |refs: &BTreeMap<String, String>| {
                    refs.iter()
                        .filter(|(name, _)| !name.starts_with("workingSets/"))
                        .map(|(name, hash)| (name.clone(), hash.clone()))
                        .collect::<BTreeMap<_, _>>()
                };
                ensure!(
                    checked.inventory == before
                        && checked.working_sets == restored.working_sets
                        && stable_refs(&checked.refs) == stable_refs(&restored.refs),
                    "backup_source_changed: source datasets changed during verification"
                );
                ensure!(
                    inventory(&store)? == before,
                    "backup_source_changed: source changed during verification"
                );
                ensure!(
                    files(&snapshot, false)? == snapshot_files,
                    "backup_changed: artifact changed during verification"
                );
                let mut manifest = Manifest {
                    format: 1,
                    kind: "database-backup".into(),
                    source_backup_digest: None,
                    dolt_version: DOLT_VERSION.into(),
                    snapshot: before,
                    dataset_root: restored.root,
                    dataset_refs: restored.refs,
                    dataset_map_nodes: restored.nodes,
                    files: snapshot_files,
                    excluded: EXCLUDED.iter().map(|s| s.to_string()).collect(),
                    verified_at: now(),
                    digest: String::new(),
                };
                manifest.seal()?;
                output.publish(&manifest)?;
                response(&output, &manifest, false)
            }
            BackupCommand::Restore { backup, out } => {
                ensure!(
                    cli.connection.is_none(),
                    "backup_arguments: restore takes a local artifact, never a live connection"
                );
                let input = Directory::open(backup)?;
                let mut manifest: Manifest = serde_json::from_slice(
                    &input.read("verified.json", MAX_MANIFEST)?,
                )
                .map_err(|_| {
                    anyhow!("backup_manifest_invalid: missing or invalid verified contract")
                })?;
                manifest.verify()?;
                let snapshot = input.open_child(std::ffi::OsStr::new("snapshot"))?;
                ensure!(
                    files(&snapshot, false)? == manifest.files,
                    "backup_changed: artifact bytes, modes or file inventory differ"
                );
                let output = Directory::fresh(out)?;
                output.write(
                    "attempt.json",
                    br#"{"format":1,"operation":"restore","verified":false}"#,
                )?;
                let tool = Tool::new(output.duplicate()?, &cli.dolt)?;
                let restored = tool.restore(&snapshot)?;
                ensure!(
                    restored.inventory == manifest.snapshot
                        && restored.root == manifest.dataset_root
                        && restored.refs == manifest.dataset_refs
                        && restored.nodes == manifest.dataset_map_nodes,
                    "backup_restore_mismatch: restored snapshot differs"
                );
                input.check()?;
                snapshot.check()?;
                ensure!(
                    files(&snapshot, false)? == manifest.files,
                    "backup_changed: artifact changed during restore"
                );
                let proof = manifest.digest.clone();
                manifest.kind = "database-restore".into();
                manifest.source_backup_digest = Some(proof);
                manifest.files = restored.files;
                manifest.verified_at = now();
                manifest.seal()?;
                output.publish(&manifest)?;
                response(&output, &manifest, true)
            }
        }
    }
}
