use crate::{
    cli::*,
    model::*,
    policy, query,
    storage::{self, Store, put},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::{IsTerminal, Read, Write},
    process::Command,
};
fn body(args: &Body, initial: &str, edit: bool) -> Result<String> {
    if let Some(b) = &args.body {
        return Ok(b.clone());
    }
    if let Some(path) = &args.body_file {
        if path.as_os_str() != "-" {
            return Ok(fs::read_to_string(path)?);
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
        return Ok(fs::read_to_string(file.path())?);
    }
    if !std::io::stdin().is_terminal() {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        if !text.is_empty() || args.body_file.is_some() {
            return Ok(text);
        }
    }
    Ok(initial.into())
}
fn set_fields(r: &mut Record, fields: &[String]) -> Result<()> {
    for field in fields {
        let (k, v) = field
            .split_once('=')
            .context("--field must be KEY=VALUE (JSON values allowed)")?;
        if k.is_empty() {
            bail!("empty field name")
        };
        if [
            "id",
            "kind",
            "title",
            "status",
            "body",
            "owner",
            "confidence",
            "created",
            "last_reviewed",
            "revision",
            "review_every",
            "sync",
            "visibility",
            "doc_type",
            "source_documents",
            "related_repos",
            "metadata",
            "number",
        ]
        .contains(&k)
        {
            bail!("{k} is a reserved field; use its command option or a relationship command")
        };
        r.fields[k] = serde_json::from_str(v).unwrap_or(json!(v));
    }
    Ok(())
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
fn bump(g: &mut Graph, id: &str, actor: &str) {
    let r = g.records.get_mut(id).unwrap();
    r.revision += 1;
    r.actor = actor.into();
}
fn receipt(op: &str, hash: &str, actor: &str, command: &Cmd, result: &Value) -> Result<String> {
    put(
        "operations",
        op,
        &json!({"op_id":op,"args_hash":hash,"actor":actor,"command":command,"result":result,"committed_at":now()}),
    )
}
fn wrap(store: &Store, data: Value, branch: &str) -> Result<Value> {
    Ok(
        json!({"schema_version":1,"ok":true,"revision":store.revision()?,"branch":branch,"data":data}),
    )
}
fn proposal_name(s: &Store, input: &str) -> Result<String> {
    if s.branches()?.contains(&input.to_string()) && input.starts_with("proposal/") {
        Ok(input.into())
    } else {
        bail!("unknown proposal {input}; use proposals")
    }
}
pub fn run(cli: &Cli) -> Result<Value> {
    let init = matches!(cli.command, Cmd::Init);
    if cli.command.mutation() {
        actor(cli)?;
    }
    if init && cli.dry_run {
        return Ok(
            json!({"schema_version":1,"ok":true,"data":{"dry_run":true,"store":cli.store,"dolt_version":storage::DOLT_VERSION}}),
        );
    }
    let store = Store::open(&cli.store, &cli.dolt, init)?;
    let branch = cli.proposal.as_deref().unwrap_or("main");
    if cli.proposal.is_some() {
        proposal_name(&store, branch)?;
    }
    store.checkout(branch)?;
    if init {
        if branch != "main" {
            bail!("init operates on main")
        }
        let op = cli
            .op_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let hash = digest(serde_json::to_vec(
            &json!({"command":"init","actor":cli.actor,"role":cli.role}),
        )?);
        if let Some(result) = store.receipt(&op, &hash)? {
            return wrap(&store, result, branch);
        }
        let result = json!({"store":store.root,"store_id":store.one::<String>("metadata","store_id")?,"dolt_version":storage::DOLT_VERSION,"op_id":op});
        store.transaction(
            &receipt(&op, &hash, actor(cli)?, &cli.command, &result)?,
            "initialize store receipt",
            actor(cli)?,
        )?;
        return wrap(&store, result, branch);
    }
    if matches!(cli.command, Cmd::Doctor) {
        return wrap(
            &store,
            json!({"schema_version":store.one::<u32>("metadata","schema")?,"store_id":store.one::<String>("metadata","store_id")?,"dolt_version":storage::DOLT_VERSION,"clean":true,"branches":store.branches()?,"import":store.one::<ImportInfo>("metadata","import")?,"policy":"cooperative local roles; not authenticated identity"}),
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
        if let Some(result) = store.receipt(&op, &hash)? {
            return wrap(&store, result, branch);
        }
        if schema == 2 {
            return wrap(
                &store,
                json!({"schema_version":2,"already_current":true}),
                branch,
            );
        }
        if store.proposals()?.iter().any(|p| p.status == "pending") {
            bail!("accept or reject pending proposals before migration");
        }
        let store_id = uuid::Uuid::new_v4().to_string();
        let result = json!({"schema_version":2,"store_id":store_id,"op_id":op});
        if cli.dry_run {
            return wrap(&store, json!({"dry_run":true,"result":result}), branch);
        }
        let mut sql = put("metadata", "schema", &2)?;
        sql.push_str(&put("metadata", "store_id", &store_id)?);
        for rel in ["related_work", "implemented_by", "produced_by"] {
            sql.push_str(&put(
                "registry",
                &format!("rel:{rel}"),
                &json!({"relation":rel,"requires_why":false}),
            )?);
        }
        sql.push_str(&receipt(&op, &hash, actor(cli)?, &cli.command, &result)?);
        store.transaction(&sql, "upgrade Scrolls to schema 2", actor(cli)?)?;
        return wrap(&store, result, branch);
    }
    let local = crate::beads::Local::load(&store.root)?;
    let before = store.graph()?;
    if let Cmd::Beads { command } = &cli.command {
        if schema != 2 {
            bail!("Beads integration requires scrolls migrate");
        }
        if matches!(command, BeadsCommand::Publish(_)) {
            actor(cli)?;
        }
        let data = crate::beads::run(cli, &store, &before, command)?;
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
    match &cli.command {
        Cmd::New(n) => body_value = Some(body(&n.body, "", false)?),
        Cmd::Edit { id, body: b, .. } => {
            let id = graph.resolve(id)?;
            body_value = Some(body(b, &graph.records[&id].body, true)?);
        }
        Cmd::Answer { text, body: b, .. } => {
            body_value = Some(if let Some(t) = text {
                t.clone()
            } else {
                body(b, "", false)?
            })
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
    // Fingerprint effective bodies, not only stdin/file paths. Output flags and op-id are not arguments.
    let hash = digest(serde_json::to_vec(
        &json!({"command":cli.command,"body":body_value,"actor":cli.actor,"role":cli.role,"proposal":cli.proposal,"source_commit":imported.as_ref().map(|i|&i.info.commit)}),
    )?);
    let op = cli
        .op_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if cli.command.mutation() {
        if let Some(result) = store.receipt(&op, &hash)? {
            return wrap(&store, result, branch);
        }
    }
    if matches!(cli.command, Cmd::Review { .. }) {
        return review(cli, &store, &before, &op, &hash);
    }
    if let Cmd::Import { .. } = &cli.command {
        if branch != "main" {
            bail!("import only operates on main")
        }
        let imported = imported.unwrap();
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
            store.transaction(
                &receipt(&op, &hash, actor(cli)?, &cli.command, &result)?,
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
        let mut sql = storage::graph_changes(&before, &imported.graph)?;
        for f in &imported.files {
            sql.push_str(&put("source_files", &f.path, f)?);
        }
        sql.push_str(&put("metadata", "import", &imported.info)?);
        sql.push_str(&put("metadata", "routing", &imported.routing)?);
        sql.push_str(&put(
            "metadata",
            "import_edges",
            &imported.graph.edges.values().collect::<Vec<_>>(),
        )?);
        sql.push_str(&receipt(&op, &hash, actor(cli)?, &cli.command, &result)?);
        store.transaction(
            &sql,
            &format!("import {} by {}", imported.info.commit, actor(cli)?),
            actor(cli)?,
        )?;
        return wrap(&store, result, branch);
    }
    let mut affected = BTreeSet::new();
    let mut result = json!({});
    let who = cli.actor.as_deref().unwrap_or("reader");
    match &cli.command {
        Cmd::New(n) => {
            let mut r = Record::new(&n.kind, &n.title, body_value.as_deref().unwrap(), who)?;
            r.owner = n.owner.clone().unwrap_or_else(|| who.into());
            r.confidence = n.confidence.clone();
            r.review_every = n.review_every;
            set_fields(&mut r, &n.field)?;
            let id = r.id.clone();
            graph.records.insert(id.clone(), r);
            for about in &n.about {
                let to = graph.resolve(about)?;
                graph.add_edge(&id, "about", &to, "", who);
            }
            for evidence in &n.supports {
                let from = graph.resolve(evidence)?;
                graph.add_edge(&from, "supports", &id, "", who);
                affected.insert(from);
            }
            policy::extract_links(&mut graph, &id, who);
            affected.insert(id.clone());
            result = json!({"id":id});
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
        } => {
            let id = graph.resolve(id)?;
            let r = graph.records.get_mut(&id).unwrap();
            r.check_edit(*expect)?;
            r.body = body_value.unwrap();
            if let Some(t) = title {
                r.title = t.clone()
            }
            if let Some(o) = owner {
                r.owner = o.clone()
            }
            if let Some(c) = confidence {
                r.confidence = c.clone()
            }
            if let Some(c) = review_every {
                r.review_every = *c
            }
            if let Some(d) = last_reviewed {
                chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
                    .context("--last-reviewed must be YYYY-MM-DD")?;
                r.last_reviewed = d.clone()
            }
            set_fields(r, field)?;
            policy::extract_links(&mut graph, &id, who);
            affected.insert(id.clone());
            result = json!({"id":id});
        }
        Cmd::Link {
            from,
            rel,
            to,
            expect,
            expect_to,
            ..
        }
        | Cmd::Unlink {
            from,
            rel,
            to,
            expect,
            expect_to,
        } => {
            let from = graph.resolve(from)?;
            let to = if to.starts_with("beads://")
                || (crate::work_ref::relation(rel) && to.contains(':') && !to.starts_with("glob:"))
            {
                local.resolve(to)?.uri()
            } else if rel == "blocks" || to.starts_with("glob:") {
                to.clone()
            } else {
                graph.resolve(to)?
            };
            graph.records[&from].check_edit(*expect)?;
            if let Some(target) = graph.records.get(&to) {
                if policy::reference_relation(rel) {
                    if let Some(expected) = expect_to {
                        target.check_revision(*expected)?;
                    }
                } else {
                    target
                        .check_edit(expect_to.context(
                            "relationships affecting two records require --expect-to",
                        )?)?;
                    affected.insert(to.clone());
                }
            }
            if matches!(cli.command, Cmd::Unlink { .. }) {
                let key = Edge {
                    from: from.clone(),
                    rel: rel.clone(),
                    to: to.clone(),
                    why: String::new(),
                    actor: String::new(),
                    created: String::new(),
                }
                .key();
                if graph.edges.remove(&key).is_none() {
                    bail!("edge does not exist")
                }
            } else {
                let why = if let Cmd::Link { why, .. } = &cli.command {
                    why
                } else {
                    unreachable!()
                };
                graph.add_edge(&from, rel, &to, why, who);
            }
            affected.insert(from.clone());
            result = json!({"from":from,"rel":rel,"to":to});
        }
        Cmd::Status { id, state, expect } => {
            let id = graph.resolve(id)?;
            let r = graph.records.get_mut(&id).unwrap();
            r.check_edit(*expect)?;
            if r.kind == "question" && state == "answered" {
                bail!("use accept ANSWER to answer a question")
            }
            if r.kind == "answer" && state == "accepted" {
                bail!("use accept ANSWER to enforce question ownership")
            }
            transition(&r.kind, &r.status, state)?;
            r.status = state.clone();
            affected.insert(id.clone());
            result = json!({"id":id});
        }
        Cmd::Supersede {
            old,
            new,
            why,
            expect,
            expect_to,
        } => {
            let old = graph.resolve(old)?;
            let new = graph.resolve(new)?;
            graph.records[&old].check_edit(*expect)?;
            graph.records[&new].check_edit(*expect_to)?;
            let r = graph.records.get_mut(&old).unwrap();
            let state = if r.kind == "decision" {
                "superseded"
            } else {
                "retired"
            };
            transition(&r.kind, &r.status, state)?;
            r.status = state.into();
            graph.add_edge(&new, "supersedes", &old, why, who);
            affected.extend([old.clone(), new.clone()]);
            result = json!({"old":old,"new":new});
        }
        Cmd::Ask {
            title,
            about,
            owner,
            context,
            if_yes,
            if_no,
        } => {
            let mut r = Record::new("question", title, context, who)?;
            r.owner = owner
                .clone()
                .or_else(|| {
                    about
                        .first()
                        .and_then(|a| graph.resolve(a).ok())
                        .map(|id| graph.records[&id].owner.clone())
                })
                .unwrap_or_else(|| who.into());
            r.fields = json!({"context":context,"if_yes":if_yes,"if_no":if_no,"asked_by":who,"consumed_session":cli.session});
            let id = r.id.clone();
            graph.records.insert(id.clone(), r);
            for a in about {
                let target = graph.resolve(a)?;
                graph.add_edge(&id, "asks_about", &target, "", who);
            }
            policy::extract_links(&mut graph, &id, who);
            affected.insert(id.clone());
            result = json!({"id":id});
        }
        Cmd::Answer {
            question, expect, ..
        } => {
            let q = graph.resolve(question)?;
            let question = &graph.records[&q];
            question.check_edit(*expect)?;
            if question.kind != "question" || question.status != "open" {
                bail!("answer requires an open local question")
            }
            let r = Record::new(
                "answer",
                &format!("Answer: {}", question.title),
                body_value.as_deref().unwrap(),
                who,
            )?;
            let id = r.id.clone();
            graph.records.insert(id.clone(), r);
            graph.add_edge(&id, "answers", &q, "", who);
            policy::extract_links(&mut graph, &id, who);
            affected.extend([id.clone(), q]);
            result = json!({"id":id});
        }
        Cmd::Accept {
            id,
            expect,
            expect_question,
        } => {
            if cli.role != Role::Human {
                bail!("answer acceptance requires --role human")
            }
            let id = graph.resolve(id)?;
            graph.records[&id].check_edit(*expect)?;
            let q = graph
                .edges
                .values()
                .find(|e| e.from == id && e.rel == "answers")
                .context("record has no answers edge")?
                .to
                .clone();
            let question = &graph.records[&q];
            question.check_edit(*expect_question)?;
            if question.owner != who {
                bail!(
                    "answer acceptance requires question owner {}",
                    question.owner
                )
            }
            if question.status != "open" {
                bail!("question is no longer open")
            }
            transition("answer", &graph.records[&id].status, "accepted")?;
            graph.records.get_mut(&id).unwrap().status = "accepted".into();
            graph.records.get_mut(&q).unwrap().status = "answered".into();
            affected.extend([id.clone(), q]);
            result = json!({"id":id});
        }
        _ => {}
    }
    if cli.command.mutation() {
        for (key, e) in &graph.edges {
            if before.edges.get(key) != Some(e) {
                affected.insert(e.from.clone());
                if !policy::reference_relation(&e.rel) && graph.records.contains_key(&e.to) {
                    affected.insert(e.to.clone());
                }
            }
        }
        for (key, e) in &before.edges {
            if !graph.edges.contains_key(key) {
                affected.insert(e.from.clone());
                if !policy::reference_relation(&e.rel) && graph.records.contains_key(&e.to) {
                    affected.insert(e.to.clone());
                }
            }
        }
        for id in &affected {
            if let Some(old) = before.records.get(id) {
                if old.frozen {
                    bail!("{} is immutable imported compatibility history", id)
                }
                bump(&mut graph, id, who);
            }
        }
        let debt = info
            .as_ref()
            .map(|i| i.lint_debt.clone())
            .unwrap_or_default();
        if schema < 2
            && graph
                .edges
                .iter()
                .any(|(k, e)| e.to.starts_with("beads://") && before.edges.get(k) != Some(e))
        {
            bail!("Beads references require scrolls migrate");
        }
        policy::validate_change(&before, &graph, &debt)?;
        for entry in crate::triggers::routing_entries(&routing)? {
            if entry["id"].as_str().is_some_and(|id| affected.contains(id)) {
                crate::triggers::imported(entry)?;
            }
        }
        let protected = affected
            .iter()
            .any(|id| graph.records[id].protected(cli.role.name()));
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
        let mut sql = storage::graph_changes(&before, &graph)?;
        if let Some(p) = proposal {
            store.run(&["checkout", "-b", &target])?;
            sql.push_str(&put("metadata", &target, &p)?);
        }
        sql.push_str(&receipt(&op, &hash, who, &cli.command, &result)?);
        store.transaction(
            &sql,
            &format!("{} by {who}", serde_json::to_string(&cli.command)?),
            who,
        )?;
        return wrap(&store, result, &target);
    }
    let revision = store.revision()?;
    let mut consumed = vec![];
    let data = match &cli.command {
        Cmd::List {
            kind,
            status,
            about,
        } => {
            let about = about.as_ref().map(|a| graph.resolve(a)).transpose()?;
            json!(
                graph
                    .records
                    .values()
                    .filter(|r| kind.as_ref().is_none_or(|k| r.kind == *k)
                        && status.as_ref().is_none_or(|s| r.status == *s)
                        && about.as_ref().is_none_or(|id| graph
                            .edges
                            .values()
                            .any(|e| e.from == r.id && e.rel == "about" && e.to == *id)))
                    .collect::<Vec<_>>()
            )
        }
        Cmd::Show { id } => {
            let name = if let Some(rest) = id.strip_prefix("scrolls://") {
                let (uuid, record) = rest.split_once('/').context("invalid Scrolls URI")?;
                let current: String = store
                    .one("metadata", "store_id")?
                    .context("run scrolls migrate first")?;
                if uuid != current {
                    bail!("Scrolls store identity mismatch; select the matching --store");
                }
                record
            } else {
                id
            };
            let id = graph.resolve(name)?;
            consumed.push(id.clone());
            json!({"uri":store.one::<String>("metadata","store_id")?.map(|s|crate::work_ref::document_uri(&s,&id)),"record":graph.records[&id],"edges":graph.edges.values().filter(|e|e.from==id||e.to==id).collect::<Vec<_>>(),"related_work":crate::beads::related(&graph,&id,&local)})
        }
        Cmd::Search { terms, limit } => {
            let terms: Vec<_> = terms.split_whitespace().map(str::to_lowercase).collect();
            let rows: Vec<_> = graph
                .records
                .values()
                .filter(|r| {
                    r.kind != "note"
                        && terms.iter().all(|t| {
                            format!("{}\n{}", r.title, r.body)
                                .to_lowercase()
                                .contains(t)
                        })
                })
                .take(*limit)
                .collect();
            consumed = rows.iter().map(|r| r.id.clone()).collect();
            json!(rows)
        }
        Cmd::Walk { id, rel, depth } => json!(query::walk(
            &graph,
            &graph.resolve(id)?,
            rel.as_deref(),
            *depth
        )),
        Cmd::Context { id, bead, budget } => {
            let packet = if let Some(reference) = bead {
                let reference = local.resolve(reference)?.uri();
                let mut packet = query::bead_context(&graph, &reference, *budget)?;
                packet["reference"] = json!(reference);
                packet["bead"] = local.summary(&reference);
                packet
            } else {
                query::context(
                    &graph,
                    &graph.resolve(id.as_deref().context("missing context ID")?)?,
                    *budget,
                )?
            };
            consumed = packet["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v["id"].as_str().map(str::to_string))
                .collect();
            packet
        }
        Cmd::Doctrine { paths } => query::doctrine(&graph, &routing, paths)?,
        Cmd::Prime { paths, budget } => {
            let packet = query::prime(&graph, &routing, paths, who, *budget)?;
            consumed = packet["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v["id"].as_str().map(str::to_string))
                .collect();
            packet
        }
        Cmd::Inbox { owner } => json!(
            graph
                .records
                .values()
                .filter(|r| r.kind == "question"
                    && r.status == "open"
                    && !r.frozen
                    && owner
                        .as_ref()
                        .is_none_or(|o| r.owner == if o == "me" { who } else { o }))
                .collect::<Vec<_>>()
        ),
        Cmd::Proposals => json!(store.proposals()?),
        Cmd::History { id } => {
            if let Some(id) = id {
                let id = graph.resolve(id)?;
                json!(store.sql(&format!("SELECT commit_hash,committer,commit_date,payload FROM dolt_history_records WHERE id={} ORDER BY commit_date,commit_hash",storage::literal(&id)))?)
            } else {
                json!(store.sql("SELECT * FROM dolt_log ORDER BY date,commit_hash")?)
            }
        }
        Cmd::Lint => {
            let mut lint = policy::lint(&graph);
            lint.extend(policy::lint_routing(&routing));
            let debt = info
                .as_ref()
                .map(|i| i.lint_debt.clone())
                .unwrap_or_default();
            json!({"errors":lint.difference(&debt).collect::<Vec<_>>(),"historical_debt":lint.intersection(&debt).collect::<Vec<_>>(),"warnings":graph.records.values().filter(|r|r.kind!="entity"&&!graph.edges.values().any(|e|e.from==r.id&&e.rel=="about")).map(|r|format!("{}:no-about",r.id)).chain(graph.edges.values().filter(|e| e.rel=="blocks" && !e.to.starts_with("beads://")).map(|e|format!("{}:unqualified-work:{}",e.from,e.to))).collect::<Vec<_>>()})
        }
        Cmd::Stale => json!(
            graph
                .records
                .values()
                .filter(|r| {
                    let date = r
                        .last_reviewed
                        .get(..10)
                        .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
                    date.is_some_and(|d| {
                        (chrono::Utc::now().date_naive() - d).num_days() > i64::from(r.review_every)
                    })
                })
                .collect::<Vec<_>>()
        ),
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
        Cmd::Render { out, format } => crate::render::render(
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
            },
        )?,
        _ => bail!("unsupported command"),
    };
    if !consumed.is_empty() {
        store.audit(
            &cli.session,
            who,
            &revision,
            &serde_json::to_string(&cli.command)?,
            &consumed,
        )?;
    }
    wrap(&store, data, branch)
}
fn review(cli: &Cli, s: &Store, main_graph: &Graph, op: &str, hash: &str) -> Result<Value> {
    let Cmd::Review {
        action,
        branch: arg,
        reason,
    } = &cli.command
    else {
        unreachable!()
    };
    let action = action.as_str();
    let arg = arg.as_deref();
    let reason = reason.as_deref();
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
        let base = s.head("main")?;
        s.checkout(&name)?;
        let proposed = s.graph()?;
        let ancestor = s.run(&["merge-base", "main", &name])?.trim().to_string();
        let old_rows = s.sql(&format!(
            "SELECT payload FROM records AS OF {}",
            storage::revision_literal(&ancestor)
        ))?;
        let old: std::collections::BTreeMap<String, Record> = old_rows
            .iter()
            .map(|v| {
                let r: Record = serde_json::from_str(v["payload"].as_str().unwrap())?;
                Ok((r.id.clone(), r))
            })
            .collect::<Result<_>>()?;
        let changes: Vec<_> = proposed
            .records
            .iter()
            .filter(|(id, r)| old.get(*id) != Some(*r))
            .map(|(id, r)| json!({"id":id,"before":old.get(id),"after":r}))
            .collect();
        let diff = s.run(&["diff", "main", &name, "edges"])?;
        return wrap(
            s,
            json!({"proposal":proposal,"main_revision":base,"merge_base":ancestor,"records":changes,"graph_diff":diff}),
            &name,
        );
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
        if !cli.dry_run {
            s.checkout(&name)?;
            let mut sql = put("metadata", &name, &proposal)?;
            sql.push_str(&receipt(op, hash, who, &cli.command, &result)?);
            s.transaction(&sql, &format!("reject {name}: {reason}"), who)?;
        }
        return wrap(
            s,
            if cli.dry_run {
                json!({"dry_run":true,"result":result})
            } else {
                result
            },
            &name,
        );
    }
    let base = s.head("main")?;
    let temp = format!("review/{}", uuid::Uuid::new_v4().simple());
    s.run(&["checkout", "-b", &temp, "main"])?;
    s.journal_review(&temp, &base)?;
    // The temporary branch is the only branch that may get merge conflicts or dirty state.
    let outcome = (|| -> Result<Value> {
        s.run(&["merge", "--no-commit", "--no-ff", &name])?;
        let conflicts = s.sql("SELECT * FROM dolt_conflicts")?;
        if !conflicts.is_empty() {
            bail!("merge conflicts on {name}; main remains at {base}")
        }
        let merged = s.graph()?;
        let debt = s
            .one::<ImportInfo>("metadata", "import")?
            .map(|i| i.lint_debt)
            .unwrap_or_default();
        policy::validate_change(main_graph, &merged, &debt)?;
        let routing = s.one::<Value>("metadata", "routing")?.unwrap_or(json!({}));
        let errors = policy::lint_routing(&routing);
        if !errors.is_empty() {
            bail!(
                "invalid routing triggers: {}",
                errors.into_iter().collect::<Vec<_>>().join("; ")
            );
        }
        if cli.dry_run {
            return Ok(json!({"dry_run":true,"validated":true,"proposal":name,"base":base}));
        }
        proposal.status = "accepted".into();
        proposal.reason = reason.map(str::to_string);
        let result = json!({"proposal":proposal,"op_id":op,"validated_base":base});
        let mut sql = put("metadata", &name, &proposal)?;
        sql.push_str(&receipt(op, hash, who, &cli.command, &result)?);
        s.transaction(&sql, &format!("accept {name} by {who}"), who)?;
        if s.head("main")? != base {
            bail!("main changed after validation; validated merge retained on {temp}")
        }
        s.checkout("main")?;
        s.run(&["merge", "--ff-only", &temp])?;
        Ok(result)
    })();
    // We own this temporary working set and can abandon it. Never reset main.
    let active = s.sql("SELECT active_branch() AS branch")?;
    if active[0]["branch"] == temp {
        let _ = s.run(&["merge", "--abort"]);
        s.run(&["reset", "--hard"])?;
        s.run(&["checkout", "main"])?;
    }
    fs::remove_file(s.root.join("review-journal.json"))?;
    if outcome.is_ok() {
        s.run(&["branch", "-D", &temp])?;
    }
    wrap(s, outcome?, "main")
}
