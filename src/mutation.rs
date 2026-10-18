//! Ordered mutations, checked against one snapshot and committed by the caller.
use crate::{model::*, schema::Schema};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Mutation {
    New {
        #[serde(default)]
        handle: Option<String>,
        kind: String,
        title: String,
        #[serde(default)]
        body: String,
        #[serde(default)]
        owner: Option<String>,
        #[serde(default)]
        fields: BTreeMap<String, Value>,
    },
    Edit {
        id: String,
        #[serde(default)]
        expect: Option<u64>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        owner: Option<String>,
        #[serde(default)]
        fields: BTreeMap<String, Value>,
        #[serde(default)]
        confidence: Option<String>,
        #[serde(default)]
        review_every: Option<u32>,
        #[serde(default)]
        last_reviewed: Option<String>,
    },
    Status {
        id: String,
        state: String,
        #[serde(default)]
        expect: Option<u64>,
    },
    Link {
        from: String,
        rel: String,
        to: String,
        #[serde(default)]
        why: String,
        #[serde(default)]
        expect: Option<u64>,
        #[serde(default)]
        expect_to: Option<u64>,
    },
    Unlink {
        from: String,
        rel: String,
        to: String,
        #[serde(default)]
        expect: Option<u64>,
        #[serde(default)]
        expect_to: Option<u64>,
    },
}
pub fn parse(text: &str) -> Result<Vec<Mutation>> {
    let ops: Vec<Mutation> = serde_yaml::from_str(text)?;
    ensure!(!ops.is_empty(), "batch must contain mutations");
    Ok(ops)
}
fn resolve(g: &Graph, handles: &BTreeMap<String, String>, name: &str) -> Result<String> {
    if let Some(h) = name.strip_prefix('$') {
        return handles
            .get(h)
            .cloned()
            .with_context(|| format!("unknown local handle {name}"));
    }
    g.resolve(name)
}
fn check(before: &Graph, id: &str, expect: Option<u64>, edit: bool) -> Result<()> {
    if let Some(r) = before.records.get(id) {
        let rev = expect
            .context("existing records require an expected revision from the initial snapshot")?;
        if edit {
            r.check_edit(rev)?;
        } else {
            r.check_revision(rev)?;
        }
    } else {
        ensure!(
            expect.is_none(),
            "new batch records do not take expected revisions"
        );
    }
    Ok(())
}
pub fn apply(
    before: &Graph,
    ops: &[Mutation],
    actor: &str,
    local: &crate::beads::Local,
) -> Result<(Graph, BTreeSet<String>, Value)> {
    let mut g = before.clone();
    let mut affected = BTreeSet::new();
    let mut authored = BTreeSet::new();
    let mut handles = BTreeMap::new();
    let mut created = vec![];
    for op in ops {
        match op {
            Mutation::New {
                handle,
                kind,
                title,
                body,
                owner,
                fields,
            } => {
                let mut r = g.schema.new_record(kind, title, body, actor)?;
                if let Some(owner) = owner {
                    r.owner = owner.clone();
                }
                for (f, v) in fields {
                    r.fields[f] = v.clone();
                }
                if let Some(h) = handle {
                    ensure!(
                        !h.is_empty() && !h.starts_with('$') && !handles.contains_key(h),
                        "invalid or duplicate handle {h}"
                    );
                    handles.insert(h.clone(), r.id.clone());
                }
                authored.insert(r.id.clone());
                affected.insert(r.id.clone());
                created.push(r.id.clone());
                g.records.insert(r.id.clone(), r);
            }
            Mutation::Edit {
                id,
                expect,
                title,
                body,
                owner,
                fields,
                confidence,
                review_every,
                last_reviewed,
            } => {
                let id = resolve(&g, &handles, id)?;
                check(before, &id, *expect, true)?;
                let r = g.records.get_mut(&id).unwrap();
                if let Some(v) = title {
                    r.title = v.clone();
                }
                if let Some(v) = body {
                    r.body = v.clone();
                }
                if let Some(v) = owner {
                    r.owner = v.clone();
                }
                if let Some(v) = confidence {
                    r.confidence = v.clone();
                }
                if let Some(v) = review_every {
                    r.review_every = *v;
                }
                if let Some(v) = last_reviewed {
                    chrono::NaiveDate::parse_from_str(v.get(..10).unwrap_or(v), "%Y-%m-%d")?;
                    r.last_reviewed = v.clone();
                }
                for (f, v) in fields {
                    r.fields[f] = v.clone();
                }
                authored.insert(id.clone());
                affected.insert(id);
            }
            Mutation::Status { id, state, expect } => {
                let id = resolve(&g, &handles, id)?;
                check(before, &id, *expect, true)?;
                let r = &g.records[&id];
                g.schema.transition(&r.kind, &r.status, state)?;
                g.records.get_mut(&id).unwrap().status = state.clone();
                affected.insert(id);
            }
            Mutation::Link {
                from,
                rel,
                to,
                expect,
                expect_to,
                ..
            }
            | Mutation::Unlink {
                from,
                rel,
                to,
                expect,
                expect_to,
            } => {
                let from = resolve(&g, &handles, from)?;
                let rule = g
                    .schema
                    .relations
                    .get(rel)
                    .with_context(|| format!("unknown relation {rel}"))?;
                let to = if to.starts_with('$') {
                    resolve(&g, &handles, to)?
                } else if !to.starts_with("beads://")
                    && g.records.values().any(|r| r.aliases.contains(to))
                {
                    g.resolve(to)?
                } else if to.starts_with("beads://")
                    || (rule.external.iter().any(|e| e == "beads")
                        && to.contains(':')
                        && !to.starts_with("glob:")
                        && !to.starts_with("http"))
                {
                    local.resolve(to)?.uri()
                } else {
                    match g.resolve(to) {
                        Ok(id) => id,
                        Err(err) => {
                            if g.records.values().any(|r| {
                                r.aliases.iter().any(|a| a == to)
                                    || r.title.eq_ignore_ascii_case(to)
                                    || r.path.as_ref() == Some(to)
                            }) {
                                return Err(err);
                            }
                            if rule.external.is_empty() {
                                return Err(err);
                            }
                            to.clone()
                        }
                    }
                };
                let key = Edge {
                    from: from.clone(),
                    rel: rel.clone(),
                    to: to.clone(),
                    why: String::new(),
                    actor: String::new(),
                    created: String::new(),
                }
                .key();
                let captured = g
                    .import_members
                    .values()
                    .any(|m| m.edges.contains_key(&key));
                if captured {
                    if let Mutation::Link { why, .. } = op {
                        ensure!(
                            why.is_empty() || why == &g.edges[&key].why,
                            "source_ownership: cannot modify imported assertion"
                        );
                        check(before, &from, *expect, true)?;
                        if !g.schema.reference(rel)? {
                            check(before, &to, *expect_to, true)?;
                        }
                        continue;
                    }
                    bail!("source_ownership: cannot remove imported assertion");
                }
                check(before, &from, *expect, true)?;
                if g.records.contains_key(&to) {
                    if g.schema.reference(rel)? {
                        if let Some(expected) = expect_to {
                            check(before, &to, Some(*expected), false)?;
                        }
                    } else {
                        check(before, &to, *expect_to, true)?;
                        affected.insert(to.clone());
                    }
                }
                if let Mutation::Link { why, .. } = op {
                    g.add_edge(&from, rel, &to, why, actor);
                } else {
                    let key = Edge {
                        from: from.clone(),
                        rel: rel.clone(),
                        to: to.clone(),
                        why: String::new(),
                        actor: String::new(),
                        created: String::new(),
                    }
                    .key();
                    ensure!(g.edges.remove(&key).is_some(), "edge does not exist");
                }
                affected.insert(from);
            }
        }
    }
    for id in &authored {
        crate::policy::extract_links(&mut g, id, actor);
    }
    for id in &affected {
        if let Some(old) = before.records.get(id) {
            ensure!(
                !old.frozen,
                "{id} is immutable imported compatibility history"
            );
            let r = g.records.get_mut(id).unwrap();
            r.revision = old.revision + 1;
            r.actor = actor.into();
        }
    }
    crate::policy::validate_change(before, &g, &BTreeSet::new())?;
    Ok((g, affected, json!({"created":created,"handles":handles})))
}
/// Guard every intermediate transition as well as the final state. Relinking or
/// cycling a guarded status in a batch cannot hide an authorization requirement.
pub fn guards(
    schema: &Schema,
    before: &Graph,
    after: &Graph,
    ops: &[Mutation],
    actor: &str,
    role: &str,
) -> Result<()> {
    schema.guards(before, after, actor, role)?;
    for op in ops {
        if let Mutation::Status { id, state, .. } = op {
            if id.starts_with('$') {
                continue;
            }
            let id = after.resolve(id)?;
            let mut g = after.clone();
            g.records.get_mut(&id).unwrap().status = state.clone();
            schema.guards(before, &g, actor, role)?;
        }
    }
    Ok(())
}
pub fn fields(args: &[String]) -> Result<BTreeMap<String, Value>> {
    args.iter()
        .map(|s| {
            let (k, v) = s.split_once('=').context("field must be KEY=VALUE")?;
            if k.is_empty() {
                bail!("empty field name");
            }
            Ok((k.into(), serde_json::from_str(v).unwrap_or(json!(v))))
        })
        .collect()
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Requirement {
    pub id: String,
    pub owners: BTreeSet<String>,
    pub human: bool,
}
impl Requirement {
    pub fn check(&self, actor: &str, role: &str) -> Result<()> {
        ensure!(
            !self.human || role == "human",
            "{}: lifecycle guard requires human",
            self.id
        );
        ensure!(
            self.owners.contains(actor),
            "{}: lifecycle guard requires pre-change owner",
            self.id
        );
        Ok(())
    }
}
pub fn requirements(before: &Graph, after: &Graph, ops: &[Mutation]) -> Result<Vec<Requirement>> {
    let mut transitions: BTreeSet<(String, String)> = after
        .records
        .values()
        .filter(|r| {
            before.schema.kinds.get(&r.kind).is_some_and(|k| {
                k.guards.iter().any(|guard| {
                    guard.to == r.status && crate::schema::guard_changed(before, after, r, guard)
                })
            })
        })
        .map(|r| (r.id.clone(), r.status.clone()))
        .collect();
    for op in ops {
        if let Mutation::Status { id, state, .. } = op {
            if !id.starts_with('$') {
                transitions.insert((after.resolve(id)?, state.clone()));
            }
        }
    }
    let mut out = vec![];
    for (id, state) in transitions {
        let r = &after.records[&id];
        for guard in before
            .schema
            .kind(&r.kind)?
            .guards
            .iter()
            .filter(|g| g.to == state)
        {
            for owners in crate::schema::guard_owners(before, after, &id, guard) {
                out.push(Requirement {
                    id: id.clone(),
                    owners,
                    human: guard.human,
                });
            }
        }
    }
    Ok(out)
}
