use crate::{
    cli::*,
    model::*,
    policy, query,
    review::{receipt_changes, wrap},
    storage::{self, Store, validate_merged_review},
    store::{Change, ChangeSet, KnowledgeStore, ProposalWrite},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::{IsTerminal, Read, Write},
    process::Command,
};
fn body(args: &Body, initial: &str, edit: bool) -> Result<(String, bool)> {
    if let Some(b) = &args.body {
        return Ok((b.clone(), true));
    }
    if let Some(path) = &args.body_file {
        if path.as_os_str() != "-" {
            return Ok((fs::read_to_string(path)?, true));
        }
    }
    if args.editor || (edit && std::io::stdin().is_terminal()) {
        let editor = std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .context("set VISUAL/EDITOR or pass --body-file")?;
        let words = shell_words::split(&editor)?;
        let Some((program, args)) = words.split_first() else {
            bail!("empty editor")
        };
        let mut file = tempfile::Builder::new().suffix(".md").tempfile()?;
        file.write_all(initial.as_bytes())?;
        if !Command::new(program)
            .args(args)
            .arg(file.path())
            .status()?
            .success()
        {
            bail!("editor failed; no write committed")
        }
        return Ok((fs::read_to_string(file.path())?, true));
    }
    if !std::io::stdin().is_terminal() {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        if !text.is_empty() || args.body_file.is_some() {
            return Ok((text, true));
        }
    }
    Ok((initial.into(), false))
}
fn actor(cli: &Cli) -> Result<&str> {
    let a = cli
        .actor
        .as_deref()
        .context("writes require --actor NAME (or SCROLLS_ACTOR); --role defaults to agent")?;
    if a.trim().is_empty() || a.contains(['\n', '\r', '<', '>']) {
        bail!("actor must be a non-empty single-line name without angle brackets")
    }
    Ok(a)
}
impl Cli {
    pub(crate) fn operation_context(
        &self,
        op: &str,
        hash: &str,
    ) -> Result<crate::review::OperationContext> {
        use crate::review::{OperationContext, RecoveryRequest, ReviewRequest};
        let review = match &self.command {
            Cmd::Review {
                action,
                review_token,
                reason,
                ..
            } => Some(ReviewRequest {
                action: action.clone(),
                token: review_token.clone(),
                reason: reason.clone(),
            }),
            _ => None,
        };
        let recovery = match &self.command {
            Cmd::Recover {
                operation,
                abandon,
                resume,
                reason,
            } => Some(RecoveryRequest {
                operation: operation.clone(),
                abandon: *abandon,
                resume: *resume,
                reason: reason.clone(),
            }),
            _ => None,
        };
        Ok(OperationContext {
            op_id: op.into(),
            args_hash: hash.into(),
            explicit_op_id: self.op_id.is_some(),
            actor: self.actor.clone(),
            role: self.role,
            dry_run: self.dry_run,
            command: serde_json::to_value(&self.command)?,
            review,
            recovery,
        })
    }
}

fn proposal_name(s: &Store, input: &str) -> Result<String> {
    if s.branches()?.contains(&input.to_string()) && input.starts_with("proposal/") {
        Ok(input.into())
    } else {
        bail!("unknown proposal {input}; use proposals")
    }
}
pub fn run(cli: &Cli) -> Result<Value> {
    if let Cmd::Backup { command } = &cli.command {
        return storage::backup::run(cli, command);
    }
    if cli.connection.is_some() {
        storage::shared::check_command(cli)?;
    } else if matches!(cli.command, Cmd::Recover { .. }) {
        bail!("recover is a shared-store command; use --connection");
    }
    if let Cmd::Import { command } = &cli.command {
        if crate::markdown_import::handles(command)? {
            return crate::markdown_import::run(cli, command);
        }
        if !matches!(command, ImportCommand::Markdown { .. }) {
            return crate::source_import::run(cli, command);
        }
    }
    if matches!(
        cli.command,
        Cmd::Ask { .. }
            | Cmd::Answer { .. }
            | Cmd::Accept { .. }
            | Cmd::Supersede { .. }
            | Cmd::Inbox { .. }
            | Cmd::Doctrine { .. }
            | Cmd::Stale
    ) {
        bail!(
            "document-specific commands were removed in 0.2.0; use new/status/link with batch for coupled changes, or query --file; see docs/migration-0.2.md"
        );
    }
    let initial_schema = if let Cmd::Init { schema, .. } = &cli.command {
        let mut s = schema
            .as_ref()
            .map(|p| {
                fs::read_to_string(p)
                    .map_err(anyhow::Error::from)
                    .and_then(|v| crate::schema::Schema::parse(&v))
            })
            .transpose()?
            .unwrap_or_default();
        s.resolve_pins(&Graph::default())?;
        Some(s)
    } else {
        None
    };
    let init = matches!(cli.command, Cmd::Init { .. });
    if cli.command.mutation() {
        actor(cli)?;
    }
    if let Some(connection) = &cli.connection {
        if init {
            return storage::shared::initialize(cli, connection, initial_schema.as_ref().unwrap());
        }
    }
    if let Cmd::Init {
        expect_base,
        expect_schema,
        ..
    } = &cli.command
    {
        anyhow::ensure!(
            expect_base.is_none() && expect_schema.is_none(),
            "initialization expectations require --connection"
        );
    }
    if init && cli.dry_run {
        return Ok(
            json!({"schema_version":2,"ok":true,"data":{"dry_run":true,"store":cli.store,"dolt_version":storage::DOLT_VERSION}}),
        );
    }
    let store = if let Some(connection) = &cli.connection {
        let recovery = match &cli.command {
            Cmd::Recover { operation, .. } => Some(operation.as_str()),
            _ if cli.command.mutation() && !cli.dry_run => cli.op_id.as_deref(),
            _ => None,
        };
        if let Some(op) = recovery {
            Store::open_shared_recovery(connection, &cli.store, op)?
        } else {
            Store::open_shared(connection, &cli.store)?
        }
    } else {
        Store::open(&cli.store, &cli.dolt, init)?
    };
    let branch = cli.proposal.as_deref().unwrap_or("main");
    if cli.proposal.is_some() {
        proposal_name(&store, branch)?;
    }
    store.checkout(branch)?;
    let selected_format = store
        .one::<u32>("metadata", "schema")?
        .context("missing store format")?;
    if selected_format > if store.is_shared() { 5 } else { 4 } {
        bail!("unsupported store format {selected_format}; use a newer Scrolls writer");
    }
    if init {
        if branch != "main" {
            bail!("init operates on main")
        }
        let op = cli
            .op_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let hash = digest(serde_json::to_vec(
            &json!({"command":"init","schema":initial_schema,"actor":cli.actor,"role":cli.role}),
        )?);
        if let Some(result) = KnowledgeStore::receipt(&store, &op, &hash)? {
            return wrap(&store, result, branch);
        }
        let result = json!({"store":store.root,"store_id":store.one::<String>("metadata","store_id")?,"dolt_version":storage::DOLT_VERSION,"op_id":op});
        let requested = initial_schema.as_ref().unwrap();
        let active: crate::schema::Schema = store.one("metadata", "ontology")?.unwrap_or_default();
        if requested != &active
            && (!store.graph()?.records.is_empty()
                || !store.rows::<Value>("operations")?.is_empty())
        {
            bail!("existing store configuration differs; use schema apply FILE --expect REV");
        }
        let mut changes = ChangeSet::from(Change::put("metadata", "ontology", requested)?);
        changes.extend(receipt_changes(
            &op,
            &hash,
            actor(cli)?,
            &serde_json::to_value(&cli.command)?,
            &result,
        )?);
        store.commit(&changes, "initialize store receipt", actor(cli)?)?;
        return wrap(&store, result, branch);
    }
    if matches!(cli.command, Cmd::Doctor) {
        return wrap(
            &store,
            json!({"schema_version":store.one::<u32>("metadata","schema")?,"store_id":store.one::<String>("metadata","store_id")?,"dolt_version":storage::DOLT_VERSION,"shared_query_bytes":store.shared_query_limit(),"clean":true,"import_sources":store.graph()?.import_sources,"import_health":crate::source_import::health(&store.graph()?,&store.import_files()?)?,"markdown_health":crate::markdown_import::health(&store,&store.graph()?,None)?,"branches":store.branches()?,"import":store.one::<ImportInfo>("metadata","import")?,"policy":"cooperative client roles; not authenticated identity"}),
            branch,
        );
    }
    // Schema checks are branch-specific: historical proposal branches remain readable.
    let schema = store
        .one::<u32>("metadata", "schema")?
        .context("missing store schema")?;
    if matches!(cli.command, Cmd::Migrate) {
        if branch != "main" {
            bail!("migrate operates on main");
        }
        let op = cli
            .op_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let hash = digest(serde_json::to_vec(
            &json!({"command":"migrate","actor":cli.actor,"role":cli.role}),
        )?);
        if let Some(result) = KnowledgeStore::receipt(&store, &op, &hash)? {
            return wrap(&store, result, branch);
        }
        if schema == 4 {
            return wrap(
                &store,
                json!({"schema_version":4,"already_current":true}),
                branch,
            );
        }
        if store.proposals()?.iter().any(|p| p.status == "pending") {
            bail!("accept or reject pending proposals before migration");
        }
        let store_id = store
            .one::<String>("metadata", "store_id")?
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let result = json!({"schema_version":4,"store_id":store_id,"op_id":op});
        if cli.dry_run {
            return wrap(&store, json!({"dry_run":true,"result":result}), branch);
        }
        let mut changes = ChangeSet::from(Change::put("metadata", "schema", &4)?);
        if schema < 3 {
            changes.push(Change::put(
                "metadata",
                "ontology",
                &crate::schema::Schema::default(),
            )?);
        }
        changes.push(Change::put("metadata", "store_id", &store_id)?);
        // Translate legacy diagnostics from preserved import originals, never
        // from current records (which could contain new violations).
        if schema < 3 && store.one::<ImportInfo>("metadata", "import")?.is_some() {
            let current = store.graph()?;
            let mut original = Graph::default();
            for record in current.records.values() {
                if let Some(value) = &record.original {
                    let record: Record = serde_json::from_value(value.clone())?;
                    original.records.insert(record.id.clone(), record);
                }
            }
            original.edges = store
                .one::<Vec<Edge>>("metadata", "import_edges")?
                .unwrap_or_default()
                .into_iter()
                .map(|e| (e.key(), e))
                .collect();
            let mut debt = policy::lint(&original);
            debt.extend(policy::lint_routing(
                &store
                    .one::<Value>("metadata", "routing")?
                    .unwrap_or(json!({})),
            ));
            changes.push(Change::put("metadata", "validation_debt", &debt)?);
        }

        for rel in ["related_work", "implemented_by", "produced_by"] {
            changes.push(Change::put(
                "registry",
                &format!("rel:{rel}"),
                &json!({"relation":rel,"requires_why":false}),
            )?);
        }
        changes.extend(receipt_changes(
            &op,
            &hash,
            actor(cli)?,
            &serde_json::to_value(&cli.command)?,
            &result,
        )?);
        store.migrate_format4(&changes.to_sql(), actor(cli)?)?;
        return wrap(&store, result, branch);
    }
    if schema < 4 && cli.command.mutation() && !matches!(cli.command, Cmd::Review { .. }) {
        bail!(
            "store format {schema} is read-only; resolve pending proposals, then run scrolls migrate"
        );
    }
    let local = if store.is_shared() {
        crate::beads::Local::default()
    } else {
        crate::beads::Local::load(&store.root)?
    };
    let before = store.graph()?;
    if let Cmd::Beads { command } = &cli.command {
        if schema < 2 {
            bail!("Beads integration requires scrolls migrate");
        }
        if matches!(command, BeadsCommand::Publish(_)) {
            actor(cli)?;
        }
        let request = crate::beads::Request {
            dry_run: cli.dry_run,
            actor: cli.actor.as_deref(),
            op_id: cli.op_id.as_deref(),
            proposal: cli.proposal.as_deref(),
            command,
        };
        let data = crate::beads::run(&store, &store.root, &before, &request)?;
        let failed = data["failed"].as_u64().unwrap_or(0) > 0;
        let mut result = wrap(&store, data, branch)?;
        if failed {
            result["ok"] = json!(false);
            result["error"] =
                json!({"message":"some Beads operations failed; inspect data.results"});
        }
        return Ok(result);
    }
    let mut graph = before.clone();
    let info: Option<ImportInfo> = store.one("metadata", "import")?;
    let routing = store
        .one::<Value>("metadata", "routing")?
        .unwrap_or(json!({}));
    let mut body_value = None;
    let mut body_supplied = true;
    match &cli.command {
        Cmd::New(n) => body_value = Some(body(&n.body, "", false)?.0),
        Cmd::Edit { id, body: b, .. } => {
            let id = graph.resolve(id)?;
            let (value, supplied) = body(b, &graph.records[&id].body, true)?;
            body_value = Some(value);
            body_supplied = supplied;
        }
        _ => {}
    }
    let imported = if let Cmd::Import {
        command: ImportCommand::Markdown { dir, rev },
    } = &cli.command
    {
        Some(crate::import::import(dir, rev, actor(cli)?)?)
    } else {
        None
    };
    let file_content = match &cli.command {
        Cmd::Batch { file }
        | Cmd::Schema {
            command: SchemaCommand::Apply { file, .. },
        } => Some(fs::read_to_string(file)?),
        _ => None,
    };
    // Fingerprint effective bodies, not only stdin/file paths. Output flags and op-id are not arguments.
    let mut fingerprint = json!({"command":cli.command,"body":if store.is_shared() && !body_supplied {None} else {body_value.as_ref()},"actor":cli.actor,"role":cli.role,"proposal":cli.proposal,"source_commit":imported.as_ref().map(|i|&i.info.commit)});
    if let Some(content) = &file_content {
        fingerprint["file_content"] = json!(content);
    }
    if store.is_shared() {
        // Machine-local input locations are not operation identity. Their exact
        // bytes are already included above; a second client can supply its copy.
        fn normalize_paths(value: &mut Value) {
            if let Some(object) = value.as_object_mut() {
                for (key, value) in object {
                    if matches!(key.as_str(), "body_file" | "file") && !value.is_null() {
                        *value = json!("<input-content>");
                    } else {
                        normalize_paths(value);
                    }
                }
            }
        }
        normalize_paths(&mut fingerprint["command"]);
    }
    let hash = digest(serde_json::to_vec(&fingerprint)?);
    let op = cli
        .op_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let context = cli.operation_context(&op, &hash)?;
    if matches!(cli.command, Cmd::Recover { resume: true, .. }) {
        return store.recover(&context);
    }
    if cli.command.mutation() {
        if let Some(result) = KnowledgeStore::receipt(&store, &op, &hash)? {
            return if store.is_shared() {
                storage::shared_review::receipt_response(&store, result, branch)
            } else {
                wrap(&store, result, branch)
            };
        }
        if store.is_shared() && !cli.dry_run {
            if let Some(result) = store.resume(&context, &before, &op, &hash)? {
                return storage::shared_review::receipt_response(&store, result, branch);
            }
        }
    }
    if matches!(cli.command, Cmd::Recover { .. }) {
        return store.recover(&context);
    }
    if matches!(cli.command, Cmd::Review { .. }) {
        return review(cli, &store, &before, &op, &hash);
    }
    if let Cmd::Import { .. } = &cli.command {
        if branch != "main" {
            bail!("import only operates on main")
        }
        let imported = imported.unwrap();
        let bundled = crate::schema::Schema::default();
        if before.schema.kinds != bundled.kinds || before.schema.relations != bundled.relations {
            bail!(
                "Liskov import requires the bundled knowledge schema; custom source adapters are outside this release"
            );
        }

        if let Some(existing) = info {
            if existing.commit != imported.info.commit
                || existing.prefix != imported.info.prefix
                || existing.repository != imported.info.repository
            {
                bail!("changed-source reimport is unsupported in v1; use a fresh store")
            }
            let result =
                json!({"identical_replay":true,"import":existing,"op_id":op,"branch":branch});
            if cli.dry_run {
                return wrap(&store, json!({"dry_run":true,"result":result}), branch);
            }
            store.commit(
                &receipt_changes(
                    &op,
                    &hash,
                    actor(cli)?,
                    &serde_json::to_value(&cli.command)?,
                    &result,
                )?,
                "record identical import replay",
                actor(cli)?,
            )?;
            return wrap(&store, result, branch);
        }
        if !graph.records.is_empty() {
            bail!("initial import requires an empty store")
        }
        let result = json!({"import":imported.info,"op_id":op,"branch":"main"});
        if cli.dry_run {
            return wrap(&store, json!({"dry_run":true,"result":result}), branch);
        }
        let mut changes = ChangeSet::graph(&before, &imported.graph)?;
        for f in &imported.files {
            changes.push(Change::put("source_files", &f.path, f)?);
        }
        changes.push(Change::put("metadata", "import", &imported.info)?);
        changes.push(Change::put("metadata", "routing", &imported.routing)?);
        changes.push(Change::put(
            "metadata",
            "import_edges",
            &imported.graph.edges.values().collect::<Vec<_>>(),
        )?);
        changes.extend(receipt_changes(
            &op,
            &hash,
            actor(cli)?,
            &serde_json::to_value(&cli.command)?,
            &result,
        )?);
        store.commit(
            &changes,
            &format!("import {} by {}", imported.info.commit, actor(cli)?),
            actor(cli)?,
        )?;
        return wrap(&store, result, branch);
    }
    let who = cli.actor.as_deref().unwrap_or("reader");
    use crate::mutation::Mutation;
    let mut ops = match &cli.command {
        Cmd::Batch { .. } => crate::mutation::parse(file_content.as_deref().unwrap())?,
        Cmd::New(n) => {
            let mut fields = crate::mutation::fields(&n.field)?;
            // Envelope metadata is applied after creation, before validation below.
            let mut r =
                graph
                    .schema
                    .new_record(&n.kind, &n.title, body_value.as_deref().unwrap(), who)?;
            if let Some(v) = &n.confidence {
                r.confidence = v.clone();
            }
            if let Some(v) = n.review_every {
                r.review_every = v;
            }
            for (k, v) in std::mem::take(&mut fields) {
                r.fields[&k] = v;
            }
            r.owner = n.owner.clone().unwrap_or_else(|| who.into());
            graph.records.insert(r.id.clone(), r.clone());
            vec![]
        }
        Cmd::Edit {
            id,
            expect,
            title,
            owner,
            field,
            confidence,
            review_every,
            last_reviewed,
            ..
        } => vec![Mutation::Edit {
            id: id.clone(),
            expect: Some(*expect),
            title: title.clone(),
            body: body_value,
            owner: owner.clone(),
            fields: crate::mutation::fields(field)?,
            confidence: confidence.clone(),
            review_every: *review_every,
            last_reviewed: last_reviewed.clone(),
        }],
        Cmd::Status { id, state, expect } => vec![Mutation::Status {
            id: id.clone(),
            state: state.clone(),
            expect: Some(*expect),
        }],
        Cmd::Link {
            from,
            rel,
            to,
            why,
            expect,
            expect_to,
        } => vec![Mutation::Link {
            from: from.clone(),
            rel: rel.clone(),
            to: to.clone(),
            why: why.clone(),
            expect: Some(*expect),
            expect_to: *expect_to,
        }],
        Cmd::Unlink {
            from,
            rel,
            to,
            expect,
            expect_to,
        } => vec![Mutation::Unlink {
            from: from.clone(),
            rel: rel.clone(),
            to: to.clone(),
            expect: Some(*expect),
            expect_to: *expect_to,
        }],
        _ => vec![],
    };
    let mut affected = BTreeSet::new();
    let mut result = json!({});
    if !ops.is_empty() {
        (graph, affected, result) = crate::mutation::apply(&before, &ops, who, &local)?;
        for op in &mut ops {
            if let Mutation::Status { id, .. } = op {
                if let Some(h) = id.strip_prefix('$') {
                    *id = result["handles"][h]
                        .as_str()
                        .context("missing batch handle")?
                        .into();
                }
            }
        }
    } else if matches!(cli.command, Cmd::New(_)) {
        affected = graph
            .records
            .keys()
            .filter(|id| !before.records.contains_key(*id))
            .cloned()
            .collect();
        result = json!({"id":affected.first()});
    }
    let config_change = if let Cmd::Schema {
        command: SchemaCommand::Apply { expect, .. },
    } = &cli.command
    {
        ensure_schema_revision(&before.schema, expect)?;
        let mut configured = crate::schema::Schema::parse(file_content.as_deref().unwrap())?;
        configured.resolve_pins(&graph)?;
        graph.schema = configured;
        result = json!({"schema_revision":graph.schema.revision(),"schema":graph.schema});
        true
    } else {
        false
    };
    if cli.command.mutation() {
        let debt = info
            .as_ref()
            .map(|i| i.lint_debt.clone())
            .unwrap_or_default();
        if matches!(cli.command, Cmd::New(_)) {
            for id in &affected {
                policy::extract_links(&mut graph, id, who);
            }
        }
        policy::validate_change(&before, &graph, &debt)?;
        for pin in &graph.schema.pins {
            graph.resolve(pin)?;
        }
        let guarded =
            crate::mutation::guards(&before.schema, &before, &graph, &ops, who, cli.role.name())
                .is_err();
        let protected = config_change
            || affected
                .iter()
                .any(|id| graph.schema.protected(&graph.records[id], cli.role.name()))
            || (guarded && cli.role == Role::Agent);
        if !protected && cli.proposal.is_none() {
            crate::mutation::guards(&before.schema, &before, &graph, &ops, who, cli.role.name())?;
        }
        let mut target = branch.to_string();
        let mut proposal = None;
        if cli.proposal.is_some() {
            let p = store
                .proposals()?
                .into_iter()
                .find(|p| p.branch == branch)
                .context("missing proposal metadata")?;
            if p.status != "pending" {
                bail!("proposal is {}; cannot append writes", p.status)
            }
        } else if protected {
            target = format!("proposal/{}", uuid::Uuid::new_v4().simple());
            proposal = Some(Proposal {
                branch: target.clone(),
                base: store.revision()?,
                actor: who.into(),
                status: "pending".into(),
                reason: None,
            });
        }
        result["branch"] = json!(target);
        result["op_id"] = json!(op);
        result["records"] = json!(
            affected
                .iter()
                .map(|id| (&graph.records[id].id, graph.records[id].revision))
                .collect::<std::collections::BTreeMap<_, _>>()
        );
        if cli.dry_run {
            return wrap(&store, json!({"dry_run":true,"result":result}), branch);
        }
        let mut changes = ChangeSet::graph(&before, &graph)?;
        if config_change {
            changes.push(Change::put("metadata", "ontology", &graph.schema)?);
        }
        let requirements = crate::mutation::requirements(&before, &graph, &ops)?;
        if !requirements.is_empty() {
            changes.push(Change::put(
                "metadata",
                &format!("guard:{}", digest(&op)),
                &requirements,
            )?);
        }

        if let Some(p) = &proposal {
            changes.push(Change::put("metadata", &target, p)?);
        }
        changes.extend(receipt_changes(
            &op,
            &hash,
            who,
            &serde_json::to_value(&cli.command)?,
            &result,
        )?);
        let message = format!("{} by {who}", serde_json::to_string(&cli.command)?);
        if let Some(p) = &proposal {
            let result = store.propose(
                &context,
                ProposalWrite {
                    changes: &changes,
                    message: &message,
                    target: &target,
                    start: None,
                    proposal: p,
                    result: &result,
                },
            )?;
            return if store.is_shared() {
                storage::shared_review::receipt_response(&store, result, &target)
            } else {
                wrap(&store, result, &target)
            };
        }
        store.commit(&changes, &message, who)?;
        return wrap(&store, result, &target);
    }
    let revision = store.revision()?;
    let mut consumed = vec![];
    let store_id: Option<String> = store.one("metadata", "store_id")?;
    let reads = crate::read::View {
        graph: &graph,
        routing: &routing,
        local: &local,
        store_id: store_id.as_deref(),
        actor: who,
    };
    let data = match &cli.command {
        Cmd::List { filters } => {
            let rows = reads.execute(crate::read::Operation::Records {
                filters,
                terms: None,
                limit: usize::MAX,
            })?;
            if store.is_shared() {
                consumed = rows
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|r| r["id"].as_str().map(str::to_owned))
                    .collect();
            }
            rows
        }
        Cmd::Schema { command } => match command {
            SchemaCommand::Show => reads.execute(crate::read::Operation::Schema)?,
            SchemaCommand::Export => json!({"yaml":serde_yaml::to_string(&graph.schema)?}),
            SchemaCommand::Validate { file } => {
                let mut configured = crate::schema::Schema::parse(&fs::read_to_string(file)?)?;
                configured.resolve_pins(&graph)?;
                let mut after = graph.clone();
                after.schema = configured;
                policy::validate_change(&graph, &after, &BTreeSet::new())?;
                json!({"valid":true,"schema_revision":after.schema.revision()})
            }
            SchemaCommand::Apply { .. } => unreachable!(),
        },
        Cmd::Query { file, path, at } => {
            let expression: query::Expr = serde_yaml::from_str(&fs::read_to_string(file)?)?;
            reads.execute(crate::read::Operation::Query {
                expression: &expression,
                actor: who,
                paths: path,
                at: at.as_deref(),
            })?
        }
        Cmd::Show { id, provenance } => {
            let id = reads.resolve(id)?;
            consumed.push(id.clone());
            reads.execute(crate::read::Operation::Document {
                id: &id,
                provenance: *provenance,
            })?
        }
        Cmd::Search {
            terms,
            limit,
            filters,
        } => {
            let rows = reads.execute(crate::read::Operation::Records {
                filters,
                terms: Some(terms),
                limit: *limit,
            })?;
            consumed = rows
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|r| r["id"].as_str().map(str::to_owned))
                .collect();
            rows
        }
        Cmd::Walk {
            id,
            rel,
            depth,
            direction,
            full,
        } => reads.execute(crate::read::Operation::Walk {
            id,
            rel: rel.as_deref(),
            depth: *depth,
            direction: *direction,
            full: *full,
        })?,
        Cmd::Context { id, bead, budget } => {
            let packet = if let Some(reference) = bead {
                let reference = local.resolve(reference)?.uri();
                query::bead_packet(&graph, &reference, &local, *budget)?
            } else {
                reads.execute(crate::read::Operation::Context {
                    id: id.as_deref().context("missing context ID")?,
                    budget: *budget,
                })?
            };
            consumed = packet["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v["id"].as_str().map(str::to_string))
                .collect();
            packet
        }
        Cmd::Prime { budget } => {
            let packet = reads.execute(crate::read::Operation::Prime { budget: *budget })?;
            consumed = graph.schema.pins.clone();
            packet
        }
        Cmd::Proposals => json!(store.proposals()?),
        Cmd::History { id } => {
            let id = id.as_deref().map(|id| graph.resolve(id)).transpose()?;
            json!(crate::read::history(&store, &revision, id.as_deref())?)
        }
        Cmd::Lint => {
            let mut lint = policy::lint(&graph);
            lint.extend(policy::lint_routing(&routing));
            let original_debt = info
                .as_ref()
                .map(|i| i.lint_debt.clone())
                .unwrap_or_default();
            let debt = store
                .one::<BTreeSet<String>>("metadata", "validation_debt")?
                .unwrap_or(original_debt);
            json!({"errors":lint.difference(&debt).collect::<Vec<_>>(),"historical_debt":lint.intersection(&debt).collect::<Vec<_>>(),"warnings":[]})
        }
        Cmd::Consumed { for_session } => {
            let path = store.root.join("consumed.jsonl");
            let mut rows = vec![];
            if path.exists() {
                for line in fs::read_to_string(path)?.lines() {
                    let row: Value = serde_json::from_str(line)
                        .context("consumption audit damaged; preserve it before recovery")?;
                    if for_session.as_ref().is_none_or(|s| row["session"] == *s) {
                        rows.push(row)
                    }
                }
            }
            json!(rows)
        }
        Cmd::Render { out, format } => {
            crate::markdown_import::health(&store, &graph, None)?;
            crate::render::render(
                &graph,
                &store.rows("source_files")?,
                info.as_ref(),
                &store
                    .one::<Vec<Edge>>("metadata", "import_edges")?
                    .unwrap_or_default(),
                &revision,
                crate::render::Options {
                    out,
                    format: *format,
                    dry_run: cli.dry_run,
                    beads: &local,
                    markdown_import: store
                        .one::<crate::markdown_import::Plan>("metadata", "markdown_import")?
                        .as_ref(),
                    import_files: &if schema >= 4 {
                        store.import_files()?
                    } else {
                        vec![]
                    },
                },
            )?
        }
        _ => bail!("unsupported command"),
    };
    if store.is_shared()
        && matches!(
            cli.command,
            Cmd::Walk { .. } | Cmd::Query { .. } | Cmd::Render { .. } | Cmd::Lint
        )
    {
        // These operations evaluate the complete graph. Report that conservative
        // input set rather than claiming an empty consumption set.
        consumed = graph.records.keys().cloned().collect();
    }
    if !consumed.is_empty() && !store.is_shared() {
        store.audit(
            &cli.session,
            who,
            &revision,
            &serde_json::to_string(&cli.command)?,
            &consumed,
        )?;
    }
    let mut response = wrap(&store, data, branch)?;
    if store.is_shared() {
        response["consumed"] = json!({"store_id":store.one::<String>("metadata", "store_id")?, "revision":revision, "schema_revision":graph.schema.revision(), "records":consumed.iter().map(|id| (id, graph.records[id].revision)).collect::<std::collections::BTreeMap<_,_>>()});
    }
    Ok(response)
}
fn review(cli: &Cli, s: &Store, main_graph: &Graph, op: &str, hash: &str) -> Result<Value> {
    let Cmd::Review {
        action,
        branch: arg,
        reason,
        review_token,
    } = &cli.command
    else {
        unreachable!()
    };
    let action = action.as_str();
    let arg = arg.as_deref();
    let reason = reason.as_deref();
    if action != "accept" && review_token.is_some() {
        bail!("--review-token is only valid for review accept; omit it for inspection or rejection")
    }
    let deciding = action == "accept" || action == "reject";
    let name = proposal_name(
        s,
        if deciding {
            arg.context("review accept|reject requires a proposal branch")?
        } else {
            if arg.is_some() {
                bail!("review BRANCH takes one positional argument")
            }
            action
        },
    )?;
    let mut proposal = s
        .proposals()?
        .into_iter()
        .find(|p| p.branch == name)
        .context("proposal metadata missing")?;
    if !deciding {
        let data = crate::read::proposal_detail(s, &name)?;
        let mut response = wrap(s, data.clone(), &name)?;
        response["revision"] = data["proposal_revision"].clone();
        return Ok(response);
    }
    if cli.proposal.is_some() {
        bail!("review decisions run from main; omit --proposal")
    }
    if cli.role != Role::Human {
        bail!("proposal review decisions require --role human; agent actors cannot accept")
    }
    if proposal.status != "pending" {
        bail!("proposal is already {}", proposal.status)
    }
    let who = actor(cli)?;
    if action == "reject" {
        let reason = reason
            .filter(|r| !r.trim().is_empty())
            .context("review reject requires --reason")?;
        proposal.status = "rejected".into();
        proposal.reason = Some(reason.into());
        let result = json!({"proposal":proposal,"op_id":op});
        let result = s.reject(&cli.operation_context(op, hash)?, &proposal, &result)?;
        return if s.is_shared() && !cli.dry_run {
            storage::shared_review::receipt_response(s, result, &name)
        } else {
            wrap(s, result, &name)
        };
    }
    let base = s.head("main")?;
    let proposal_revision = s.head(&name)?;
    let token = review_token.as_deref().with_context(|| {
        format!(
            "review accept requires --review-token; run scrolls review {name} and pass its token"
        )
    })?;
    crate::review_token::ReviewToken::decode(token)
        .with_context(|| format!("run scrolls review {name} again and pass its review_token"))?
        .check(&name, &base, &proposal_revision)?;
    let result = s.accept(
        &cli.operation_context(op, hash)?,
        main_graph,
        &proposal,
        (&base, &proposal_revision),
        &mut || validate_merged_review(s, main_graph, who, cli.role),
    )?;
    wrap(s, result, "main")
}

fn ensure_schema_revision(schema: &crate::schema::Schema, expect: &str) -> Result<()> {
    if schema.revision() != expect {
        bail!("stale schema revision; run schema show and retry with a fresh --op-id");
    }
    Ok(())
}
