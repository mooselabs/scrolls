# Scrolls — design

Status: design baseline with local v1 implementation · Created: 2026-10-16 · Owner: Patrick McEvoy

Proposed extension: [configurable import profiles](import-profiles.md) defines
declarative source mappings, stable source identities, pinned plans, provenance,
and atomic imports. Its example syntax and commands are not yet implemented.

## Implementation boundary (2026-10-16)

The local Rust implementation is described in [commands.md](commands.md) and
qualified in [migration-report.md](migration-report.md). The sections below
retain the original broader design; where they differ, this boundary and the
command reference describe shipped behavior.

Implemented: Rust library and Clap/Serde CLI, private pinned Dolt 2.2.0 CLI
under `.scrolls/dolt` (no SQL server), serialized transactions and operation
receipts, stable identities and legacy aliases, local graph/text queries,
attributed authoring, cooperative human review, local questions, pinned Git
import, lossless original files, and native/Liskov render adapters.

The registry is seeded but not editable. Records and edges are keyed JSON rows;
Dolt merges at that row payload boundary. Consumption is a separate local audit
file. Context budgets count serialized item bytes explicitly. Version stamps
live in `RENDERED` and the root manifest, never in legacy frontmatter. Imported
work records **including questions** and untyped archives remain immutable;
no answers are inferred from prose. Backlog, epic, and milestone files are
preserved as compatibility artifacts rather than discarded. Repository entities
have supplemental views in both formats. Rendered output is always a fresh destination.

Deferred: registry editing, remote synchronization and replication, arbitrary
SQL and as-of commands, models/embeddings, promotion convenience commands,
executable doctrine checks, hooks, authenticated identities, and web/Slack/MCP
surfaces. No documentation-authority cutover or automatic operating-rule change
is made by this release.

Beads references are now implemented as qualified external edge targets, with
local aliases, offline context/backlink queries, explicit cached refresh, and
explicit publication into per-store Beads metadata. Schema 2 adds a Scrolls store
UUID and the `related_work`, `implemented_by`, and `produced_by` relations. Existing
`blocks` links remain informational. The [command contract](commands.md#beads-references)
defines the implemented boundary; scheduler gates and automatic impact workflows
remain deferred. Separate databases and review/work lifecycles remain independent.
The adapter is qualified against Beads 1.3.0 through disposable fixtures; it has
not been connected to the orchestrator's live work ledger.


Scrolls is Beads for knowledge. Beads gives coding agents a Dolt-backed,
graph-shaped issue tracker with a CLI they can drive without reading prose.
Scrolls gives them the same thing for what a company knows: decisions,
synthesised understanding, evidence, open questions, and the doctrine that
says how agents are allowed to work with all of it.

It replaces the Markdown documentation system that the Liskov orchestrator
runs today under `docs/` (specified in
`docs/meta/documentation-system.md`), in the same way that ADR-0236 replaced
the Markdown backlog with Beads: the records move into Dolt, Markdown becomes
a rendered view, and the existing tooling keeps working through the cutover.

---

## 1. Why

The Markdown system works, but every property that makes it work is bolted
on from the outside:

- Order, dependency, provenance and status live in frontmatter, enforced by a
  validator that has to re-parse the whole tree on every run.
- Relationships between documents are links in prose or paths in
  `source_documents`. An agent can only follow them by reading, and it cannot
  ask "what supports this decision" without a script that walks files.
- Concurrent writers collide on files. The shared checkout goes stale
  silently, and half the orchestrator commits are contention on one file.
- There is no review surface for agent writes other than a git diff of prose.
- Knowledge about the business, the product and the engineering all lives in
  one tree, with no way to say who may read what.
- The tree is written for engineers. The people whose knowledge matters
  most, the CEO, product, marketing and a new hire, do not open a git
  checkout to read or answer anything.

Two requirements follow from how agents actually run and are easy to lose
when the store moves out of files:

- **Local and offline.** Agents read the store hundreds of times a session.
  Every read must be a local call with no network on the path. Every
  command must work with no remote and no model API reachable, degrading
  to "here is what I could not decide and why" rather than failing. The
  doctrine router already behaves this way and Scrolls keeps the promise.
- **Whole of company.** Engineers drive it from a shell. Everyone else
  reads and answers through a surface that does not look like one (§9).

Dolt gives us cell-level merge, branches, diffs, history and remotes on a SQL
store that agents already know how to query. Beads has proven the shape
(27k stars, Gas Town built on it). Nothing open source yet does the same for
knowledge: the nearest attempt, `memdolt`, has the right architecture and no
users; `bd remember` is a flat key-value table; the governed-Markdown
projects (ContextNest, Google's Open Knowledge Format, ontology-atlas) keep
files canonical and add metadata. Scrolls takes Beads' storage model,
memdolt's review model, and the typed-record discipline the Liskov docs tree
already has.

---

## 2. Concepts

| Term | Meaning |
| --- | --- |
| **Scroll** | One record. Has a kind, a lifecycle status, a Markdown body, typed fields, and edges to other scrolls and entities. |
| **Kind** | A registered record type with its own required fields, lifecycle and write policy. Kinds can be added without a code change. |
| **Edge** | A typed, directed, attributed relation between two scrolls or a scroll and an entity. Every edge records who asserted it and why. |
| **Entity** | A noun the graph hangs on: a repo, service, product, team, customer, person or glossary term. Entities are what let an agent reason mechanically instead of by reading prose. |
| **Registry** | The three tables that define kinds, relation types and fields. The ontology is data, not code. |
| **Doctrine** | A scroll whose body is a rule and whose trigger says when the rule is in scope. |
| **Proposal** | A Dolt branch holding agent writes to high-stakes kinds, awaiting human review. |
| **Store** | One Dolt database. A workspace has at least one; visibility partitioning (later) adds more. |

The guiding rule: **the graph must be good enough that an agent can answer
structural questions without opening a body.** "Every accepted decision about
billing that nothing supersedes, its supporting evidence, the open questions
about it, and the doctrine that applies" is one graph walk. The body is for
humans and for the final synthesis step, not for navigation.

---

## 3. The default ontology

Keep the kind list short and put variety in facets, so queries stay simple.

### 3.1 Kinds

| Kind | Purpose | Lifecycle | Default write policy |
| --- | --- | --- | --- |
| `decision` | A durable choice. Facet `domain`: `business`, `product`, `technical`. | proposed → accepted → superseded / retired | proposal |
| `knowledge` | Synthesised current understanding of one topic. Short, navigable, linked to evidence. | draft → current → stale → retired | proposal |
| `primer` | Subkind of `knowledge`. A cross-cutting explainer for someone new to an area. | as `knowledge` | proposal |
| `evidence` | A verbatim capture: a readback, a transcript, a command output, a customer email. Never truth on its own. | captured → archived | direct |
| `question` | Something an agent or human needs answered before work can proceed. | open → answered / withdrawn | direct |
| `answer` | One response to a question. Several may exist; one is accepted. | offered → accepted / declined | direct (humans), proposal (agents) |
| `doctrine` | A rule with a trigger. Governs how agents work with code, with the business, and with Scrolls itself. | proposed → active → retired | proposal |
| `entity` | An ontology anchor. Facet `type`: `repo`, `service`, `product`, `team`, `person`, `customer`, `term`. | active → retired | proposal |
| `collection` | A curated ordered view over other scrolls. Replaces the hand-maintained `README.md` indexes. | current → stale | direct |
| `note` | A session or work note. Excluded from priming and recall by default. | — | direct |

`write policy` is **direct** (commit to main, attributed) or **proposal**
(commit to a proposal branch, human merges). Policies are per kind and
editable; see §6.

### 3.2 Fields every scroll carries

```
id             hash-based, collision-free across parallel writers (as Beads)
kind
title
status         one of the kind's lifecycle states
body           Markdown
owner          a person or seat responsible for keeping it true
confidence     high | medium | low
created        timestamp
last_reviewed  timestamp
review_every   duration; lint flags scrolls past due
actor          who made the last write (human id or agent seat)
supersedes     optional id of the scroll this replaces
visibility     tier label; unused in the internal-first phase (§8)
sync           sync policy: always | on-demand | never; independent of visibility (§8)
fields         JSON of kind-specific and custom fields, validated against the registry
```

Body links and edges are kept consistent, not duplicated by hand. A
Markdown link in the body to another scroll's id is extracted into a
`related` edge at write time if no stronger edge already exists; an edge
whose target is not mentioned in the body is fine. Lint reports a body
link to an id that does not exist. This replaces the current rule that
evidence must be linked both in frontmatter and inline.

### 3.3 Relations

Each relation is a row in the registry declaring the kinds it may join, an
inverse name, whether it is directed and transitive, and whether the CLI
demands a `--why` when asserting it.

| Relation | From → To | Directed | `--why` | Notes |
| --- | --- | --- | --- | --- |
| `supports` | evidence → knowledge, decision | yes | no | Inverse `supported_by`. The provenance edge. |
| `derived_from` | knowledge → knowledge, evidence | yes | no | Synthesis lineage. |
| `supersedes` | same kind → same kind | yes | yes | Mirrors the `supersedes` column; the column is the fast path. |
| `contradicts` | decision, knowledge ↔ same | no | yes | Raised by lint, resolved by a human. |
| `about` | any → entity | yes | no | The main navigation edge. Most scrolls have at least one. |
| `applies_to` | doctrine → entity, path pattern | yes | no | Doctrine scope beyond glob triggers. |
| `asks_about` | question → any | yes | no | |
| `answers` | answer → question | yes | no | |
| `promoted_to` | answer, evidence → decision, knowledge | yes | no | Keeps the trail when a question becomes a decision. |
| `depends_on` | decision → decision | yes | yes | Transitive. Lint rejects cycles. |
| `blocks` | question → external work id | yes | no | A Beads issue id. Lets the delivery queue wait on knowledge. |
| `related` | any ↔ any | no | no | The weak fallback. Lint nags if a stronger edge fits. |

### 3.4 Entities are the spine

Entities are cheap to create and are the thing every other scroll points
at. For Liskov the first batch is mechanical: one `repo` entity per row of
`repos/manifest.json`, one `service` per runtime component, one `team` per
lane, `term` entities from the glossary. `scrolls context billing` works
because `billing` is an entity with inbound `about` edges, not because the
word appears in bodies.

### 3.5 Extending the ontology

```
scrolls kind add  runbook --parent knowledge --lifecycle "draft,current,stale,retired" --write proposal
scrolls field add runbook severity --type enum --values p0,p1,p2 --required
scrolls rel  add  mitigates --from runbook --to entity --directed --why
```

These write rows to the registry on a proposal branch like any other
high-stakes change. The validator reads the registry at run time, so no
release of Scrolls is needed to add a kind.

Retiring is as important as adding. `scrolls field retire <kind> <name>
--replaced-by <name> --migrate "<command>"` keeps the retired field in the
registry with its replacement. A write that uses the retired name is
refused with the replacement's name and the migration command, which is
how the current validator handles retired frontmatter keys and is the
reason agents stop using an old key within a day of its retirement.

---

## 4. Storage

One Dolt database per store, in `.scrolls/` at the workspace root, served
by the same local Dolt SQL server Beads uses. Scrolls and Beads stay
**separate databases** on that server: they move at different speeds, and
coupling knowledge lifecycle to work lifecycle is the mistake the Markdown
tree made with the delivery queue.

```sql
-- registry
kinds      (kind, parent_kind, lifecycle, write_policy, description)
fields     (kind, name, type, required, enum_values, description)
relations  (rel, inverse, directed, transitive, from_kinds, to_kinds, requires_why)

-- records
scrolls    (id, kind, title, status, body, owner, confidence, created,
            last_reviewed, review_every, actor, supersedes, visibility, fields)
edges      (from_id, rel, to_id, why, actor, confidence, created)
entities   (id, type, name, aliases)                  -- entities are scrolls too; this is the index
ownership  (entity_id | domain, owner, role)          -- routes questions
triggers   (doctrine_id, glob | scope_clause)         -- doctrine routing
checks     (doctrine_id, command, applies_to_glob)    -- executable doctrine (§7)

-- audit
operations (op_id, actor, command, args_hash, result, committed_at)   -- idempotent writes (§6.3)
consumed   (session_id, seat, scroll_id, commit_hash, read_at, via)   -- what each session read (§6.2)

-- derived, rebuilt by `scrolls index`
fts        full-text over title + body
embeddings optional, local model, for the second-line retriever
```

Markdown in the body column, not files. `scrolls render` writes a Markdown
tree with the same frontmatter contract the current validator expects, so
`git diff`, `grep`, and the doctrine router keep working. The rendered tree
is a view. Editing it does nothing; the CLI refuses, exactly as `next_work.py`
refuses edits to frozen work records today. Every rendered file carries the
Dolt commit hash it was rendered from in its frontmatter, and the tree root
carries it in a `RENDERED` file, so a stale view says so instead of passing
for current. A view that cannot be tied to a version of the state is the
failure mode StagedWorkspace is about, and it is the one the shared checkout
has today.

Every write is a Dolt commit with the actor in the commit message. History,
blame and point-in-time reads come for free: "what did we believe about
custody on 2026-08-01" is `AS OF`.

Agents may also query the store with read-only SQL through `scrolls sql`.
They already know SQL, the schema is small, and a query the CLI did not
anticipate should not need a release. Writes go through the CLI only, so
the registry, write policy and operation log are never bypassed.

---

## 5. CLI surface

Modelled on `bd`. Everything has `--json`. Everything that writes has
`--dry-run`.

```
# reading
scrolls prime                         doctrine in scope + active primers + my open questions, for session start
scrolls context <entity|path|id>      token-budgeted packet from a deterministic graph walk (§7)
scrolls show <id>                     one scroll with its edges
scrolls search <terms>                full-text, optionally --hybrid
scrolls walk <id> --rel supports --depth 2
scrolls list --kind decision --status accepted --about billing
scrolls history <id>                  Dolt log for one scroll
scrolls as-of <date> show <id>
scrolls sql "<select ...>"            read-only SQL against the store
scrolls consumed [--session <id>]     which scrolls, at which commits, a session read

# writing
scrolls new <kind> --title ... [--field k=v ...] [--about <entity> ...]   body from stdin or --body-file
scrolls edit <id>
scrolls link <from> <rel> <to> --why "..."
scrolls supersede <old> --with <new>
scrolls status <id> <state>

# questions
scrolls ask "..." --about <entity> --context "what I was doing" --if-yes "..." --if-no "..."
scrolls answer <q-id> "..."
scrolls accept <answer-id>
scrolls promote <answer-id|evidence-id> --to decision --title ...
scrolls inbox [--owner me]            open questions routed to me

# doctrine
scrolls doctrine --for <path>... [--diff -]   the router: which bodies must be read before this change
scrolls doctrine check [--for <path>...]      run the executable checks attached to in-scope doctrine
scrolls doctrine list --active

# review
scrolls proposals                     pending proposal branches
scrolls review <proposal>             the Dolt diff, scroll by scroll
scrolls review accept|reject <proposal> [--reason]

# health
scrolls lint [--model]                mechanical pass; --model adds the frontier-model pass
scrolls stale                         scrolls past review_every
scrolls index                         rebuild fts / embeddings

# ontology
scrolls kind|field|rel add|list|retire

# sync and views
scrolls render [--out docs/]          stamped with the commit it came from
scrolls push | pull | sync            honours each scroll's sync policy
scrolls import markdown <dir>         the migration path (§11)
scrolls hooks install                 SessionStart hooks for Claude Code, Codex and Grok (§6.1)
```

Every write accepts `--op-id <ulid>`. Agents always pass one (§6.3).

An MCP server exposes the same verbs for agents that prefer tools over a
shell. The CLI is primary; MCP is a thin wrapper over it. DoltHub's own
position is that agents do not need MCP to use Dolt because they already
speak SQL and git; Scrolls agrees for reads (`scrolls sql`) and disagrees
for writes, which must go through the policy layer.

---

## 6. How agents and humans work with it

### 6.1 Session start

`scrolls prime` injects, in this order and under a token budget:

1. Doctrine that applies to the paths or scope the session has declared.
2. Active primers for the entities in scope.
3. Open questions assigned to this seat, and questions this seat asked that
   now have answers.

This replaces "read AGENTS.md, then docs/README.md, then
active-projects.md" with one call whose output is computed, not curated.

`prime` is wired in the way the orchestrator already wires its checkout
refresh: `scrolls hooks install` writes a SessionStart hook for Claude Code
and Grok into the versioned `.claude/settings.json` and a machine-local
entry for Codex, and the orchestrator's `install-agent-session-hooks.sh`
calls it. The hook runs once at session start. A session that has run for
an hour re-primes itself, which is the same rule as refreshing the checkout
before a claim.

`prime` has no network on its path. With no remote it reads the local
clone; with no model API the scope-clause judgement in the doctrine router
is skipped and the clauses are printed with a line saying they could not be
decided. An agent that cannot reach anything still gets correct doctrine,
primers and questions, just without the advisory judgement.

### 6.2 Context packets

`scrolls context <entity>` runs a fixed walk:

```
entity
  ← about: decisions (accepted, not superseded)         with  ← supports: evidence (titles only)
  ← about: knowledge (current)
  ← asks_about: questions (open)
  ← applies_to: doctrine (active)
  → related entities one hop out, titles only
```

The walk is deterministic, so the same question yields the same packet
until the graph changes. Full-text search is the second line, embeddings an
optional third. ContextNest measured dense retrieval returning a different
document set on 80% of repeated queries; a knowledge base agents act on
should not behave like that.

The Codified Context paper's split is useful vocabulary here: `prime` is
the hot memory every session carries, `context` is the cold memory loaded
on demand. The paper ran 283 sessions on that split and found the cold
store was what carried understanding between sessions. Scrolls makes the
same split a computed one.

**Consumption is recorded.** Every `prime`, `context`, `show`, `search`
and `sql` call appends to the `consumed` table: the session, the seat, the
scroll id, the Dolt commit it was read at, and which command read it. This
is what lets a human reconstruct later which versions of which scrolls an
agent was working from when it made a change, asked a question or wrote a
proposal. `scrolls ask` already captures "what I read" for questions; the
audit table makes it true for everything. It is the piece of ContextNest
worth keeping: provenance of outputs, not just of inputs. The table is
local to the clone and syncs under the `on-demand` policy, so it never
bloats a pull.

### 6.3 Writing

Each kind's write policy decides where a write lands:

- **direct**: a Dolt commit on `main`, attributed. Evidence, questions,
  notes, collections, human answers.
- **proposal**: a commit on `proposal/<seat>/<ulid>`. Decisions, knowledge,
  doctrine, entities, registry changes, agent answers.

`scrolls proposals` is the review queue. `scrolls review <id>` shows the
Dolt diff: new rows, changed cells, new edges, each with the actor and the
`--why`. Accept merges to `main`; reject discards with a reason that is
itself stored as evidence. Pending proposals are invisible to `prime`,
`context` and `search` unless `--include-proposals` is passed, so an agent
cannot cite its own unreviewed claim.

**Accept runs a check before it merges.** When a human accepts a proposal
that touches `decision` or `knowledge`, Scrolls runs the mechanical lint
on the merged result and, if a model is configured, a contradiction check
of the new scrolls against accepted scrolls that share an entity. A lint
failure blocks the merge. A contradiction finding does not block; it is
shown to the reviewer with the conflicting ids and the accept proceeds only
if they confirm. This is memdolt's accept-time check, with the blocking
rule relaxed so an unreachable model never prevents a human from merging.

**Every write is idempotent.** Agents pass `--op-id` with a fresh ULID on
every mutating command. The `operations` table records the id, the actor,
the command and a hash of its arguments, and the result. Replaying an id
with the same arguments returns the original result without writing.
Reusing an id with different arguments is refused. This is the rule the
work-store rehearsal established for mutations and it exists because
agents retry: a timeout on `scrolls new` followed by a retry must not
create two scrolls.

A frontier-model pass (§6.5) may propose. It may never accept.

### 6.4 Questions

The question system is the human inbox, and it is the mechanism by which
agents stop guessing.

1. An agent hits something it cannot resolve from the graph. It runs
   `scrolls ask`, recording the question, what it was doing, which scrolls it
   read, and what it would do under each likely answer.
2. The ownership table routes the question: by entity owner, then domain
   owner, then a default. The owner sees it in `scrolls inbox`, in a small
   web view, or via a Slack notification.
3. If the question blocks work, the agent links it with `blocks` to the
   Beads issue. The Beads delivery queue treats a `blocks` edge from an open
   question the way it treats an unmet `depends_on`.
4. A human answers. Several answers may be offered; the owner accepts one.
5. If the answer is durable, `scrolls promote` turns it into a decision or
   knowledge scroll with a `promoted_to` edge, so the trail from question to
   policy is never lost.

Questions that stay open past a threshold show up in lint. A question
answered by an agent on a proposal branch is just another proposal.

### 6.5 Lint in two layers

The **mechanical pass** always runs and never needs a model:

- schema: required fields per kind, enum values, lifecycle transitions
- graph: dangling edges, edges that violate the relation registry, cycles
  in `depends_on`, `supersedes` chains that loop
- hygiene: orphan scrolls with no `about` edge, `related` where a stronger
  relation fits, stale `last_reviewed`, questions open too long, knowledge
  with no `supports` edge
- links: a body link to an id that does not exist, or a body link with no
  corresponding edge (§3.2)
- views: a rendered tree whose stamped commit is behind `main`
- visibility: an edge from a less restricted store to a more restricted one
  (inactive until §8)

The **model pass** is opt-in (`--model`) and writes only proposals:

- candidate `about` edges for scrolls that mention an entity's name or alias
- contradictions between accepted decisions, raised as `contradicts` edges
  with a one-paragraph rationale
- clusters of unlinked evidence that could become one knowledge scroll, with
  a draft synthesis
- questions whose accepted answer looks durable enough to promote

This is the division of labour the whole design rests on: traditional
information architecture and scripts make the structure trustworthy; the
best available model works on top of that structure and its output goes
through the same review as any agent's.

---

## 7. Doctrine

Doctrine scrolls carry the rule in the body and the scope in `triggers`:
globs that a changed path can match, and scope clauses that no glob can
decide ("a customer-facing capability changes support boundary"). This is
the conditional-doctrine table in the orchestrator's AGENTS.md, stored as
records instead of a hand-maintained table.

`scrolls doctrine --for <paths> --diff -` is `scripts/agent_doctrine.py`
with the registry as its source. Globs are decided mechanically. Scope
clauses go to a calibrated model judgement, advisory only, never changing
an exit code, exactly as ADR-0144 specifies today. With no key or no
reachable service the router prints the clauses and says why it could not
decide, so routing is never silently weaker than it was.

**Doctrine can carry a check.** A doctrine scroll may attach one or more
commands in the `checks` table, each with the glob it applies to.
`scrolls doctrine check --for <paths>` runs the checks whose doctrine is in
scope and reports pass or fail per rule. A rule about SQL migrations can
run the migration linter; a rule about observability parity can run the
script that counts workers without a metric. The body stays the rule a
human reads; the check is the part a machine can enforce. This is
archgate's idea of an ADR as an executable rule, and Straymark's drift
check (declared files against the real diff) is one such check worth
shipping as a default for packets that declare their scope.

A small set of doctrine ships as defaults and governs Scrolls itself:

- Evidence is never truth. Nothing moves from `evidence` to `knowledge` or
  `decision` without a `supports` edge and a human accept.
- Every `knowledge` and `decision` scroll links its evidence. Lint refuses
  otherwise.
- Every scroll has an owner and a review cadence. Stale is a visible state,
  not a silent one.
- An agent never accepts its own proposal. A model pass never accepts
  anything.
- A question is cheaper than a guess. When the graph does not answer, ask.
- Prefer the strongest relation that is true. `related` is a last resort.

---

## 8. Sync, federation and access control

**Internal first.** For the first phase there is one store, one self-hosted
Dolt remote, and `scrolls push` / `pull` / `sync` are thin wrappers over
Dolt's. Every machine has a full clone. Compatible changes merge cell by
cell; real conflicts need a human choice, and `push` has no force option.
This is the Gas Town sync model and it is enough for a team.

**Sync policy is per scroll and separate from visibility.** The `sync`
column says whether a scroll travels: `always` for the graph proper,
`on-demand` for bulky evidence, the consumption audit and session notes,
which are fetched by id when something references them, and `never` for
scratch that should stay on one machine. Visibility says who may read a
scroll; sync says whether it leaves the machine by default. The two are
orthogonal and both exist from day one so that partitioning later is a
data change, not a schema change.

**Later.** Access control is a partitioning decision, not a column. A
row-level ACL in a replicated database is a fiction, because the bytes are
on every clone. When it is needed, visibility maps to separate Dolt
databases (company, per team, exec, restricted). Each person clones only
the stores their entitlement allows, the remote enforces entitlement at
auth, and the CLI presents the union of what is local. Edges may point from
a more restricted store to a less restricted one, never the reverse. The
`visibility` column and the lint rule exist from day one so the partition
can be introduced without a migration. Federation across companies or
partners is the same mechanism with a different remote.

---

## 9. Surfaces for people who do not use a shell

Scrolls is for the whole company or it is just another engineering tool.
The CLI is the primary interface for engineers and agents, and nothing
below adds a second write path: every surface calls the same commands.

- **A read view.** The rendered Markdown tree, served as a small static
  site from `scrolls render`, with the entity pages as the front door.
  Someone in marketing lands on the product entity and sees its accepted
  decisions, current primers and open questions without knowing any of
  those words.
- **The question inbox.** A one-page web view and a Slack flow. A question
  arrives with the agent's context and its "if yes / if no" branches, and
  the owner answers in the thread. The answer is written through
  `scrolls answer` with the Slack user as actor. This is the surface the
  CEO uses, and it is the whole of what they need to use.
- **Proposal review.** The same web view lists proposals as readable diffs
  with accept and reject buttons, for product and business decisions that
  an engineer should not be the one to merge.
- **Capture.** A share-to-Scrolls path (email forward, Slack message
  action) that files a verbatim capture as `evidence` with the sender as
  actor and no other judgement. Evidence is direct-write, so this needs no
  review and is the cheapest way for a non-engineer to put something in.

Each of these is a thin client over the CLI. None stores state of its own.

## 10. Relationship to Beads

| | Beads | Scrolls |
| --- | --- | --- |
| Records | issues, epics | decisions, knowledge, evidence, questions, doctrine, entities |
| Lifecycle speed | hours to days | weeks to years |
| Graph | dependency DAG | typed ontology with entities as anchors |
| Agent writes | direct | per-kind policy; high-stakes via proposal branch |
| Memory | `bd remember` key-value, free text, no types or links | not needed; a `knowledge` scroll is the memory |
| Storage | Dolt, `.beads/` | Dolt, `.scrolls/`, same local server |

Cross-references are by id in both directions: a Beads issue cites the
decision ids it implements; a question `blocks` a Beads issue id. Neither
tool writes to the other's database. The point of keeping them apart is
that a knowledge base should still be correct after every issue is closed.

Gas Town's agent constitution tells agents to use `bd remember` instead of
memory files. In a workspace with Scrolls installed that instruction
becomes "use `scrolls new knowledge`", and `bd remember` is left for the
handful of things that are genuinely a key and a value.

---

## 11. Migration from the Liskov docs tree

The existing frontmatter maps almost one to one, and `work-store/` already
proved the pattern: import at a commit, export, diff bytes, compare the
frontier at one instant.

| Markdown | Scrolls |
| --- | --- |
| `doc_type: raw` | `evidence` |
| `doc_type: knowledge` | `knowledge` |
| `doc_type: adr` | `decision`, `domain: technical` |
| `doc_type: question` (open / answered) | `question` plus `answer` where a body section answers it |
| `docs/meta/doctrine/*` and the AGENTS.md table | `doctrine` with `triggers` |
| `doc_type: business` | `knowledge`, `domain: business` |
| `doc_type: index` (README.md) | `collection` |
| `doc_type: work_note` | `note` |
| `source_documents` | `supports` or `derived_from` edges |
| `related_repos` | `about` edges to `repo` entities from `repos/manifest.json` |
| `confidence`, `last_reviewed`, `owner`, `created` | the same columns |
| milestone, epic, backlog | not imported; those are Beads' |

Milestones:

1. **Import and render.** `scrolls import markdown docs/` at a pinned commit;
   `scrolls render` back out; the rendered tree must pass
   `validate_docs.py` and differ from the source only in ways the import log
   explains. No agent writes yet.
2. **Read-only cutover.** `scrolls prime` and `scrolls context` replace the
   reading steps in AGENTS.md. The Markdown tree is still canonical.
3. **Write cutover.** A `knowledge-authority.json` flag (mirroring
   `gascity/work-authority.json`) flips canonical to Dolt. The Markdown tree
   becomes a rendered view and the CLI refuses edits to it.
4. **Questions live.** Ownership table populated; `scrolls inbox` and Slack
   routing on; `blocks` edges honoured by the delivery queue.
5. **Model lint.** Opt-in contradiction and edge-suggestion pass.

---

## 12. How we will know it works

Nobody in the prior art has shown that a structured knowledge layer makes
agents better. ontology-atlas says plainly it has not measured a
difference against a no-ontology baseline. Right Reasons reports 100%
recall on "why" questions against 0% for Markdown with retrieval, but the
numbers are self-reported from 48 runs on the author's own corpus.
Scrolls should not ship on faith either.

Measures to take before and after each cutover milestone, on the real
orchestrator with real sessions:

- **Questions asked versus guesses made.** Count `scrolls ask` calls and
  count proposals or changes later reverted because the agent assumed
  something a human knew. The first should rise and the second fall.
- **Reads per session.** How many scrolls a session consumes (from the
  `consumed` table) and how many of them it cites in its output. A good
  packet is small and mostly used.
- **Stale reads.** How often a session read a scroll at a commit behind
  `main`. Today this is unmeasurable; the checkout study found sessions
  dozens of commits behind.
- **Proposal acceptance rate** and time to review, by kind and by actor.
- **"Why" recall.** A fixed set of questions of the form "why did we decide
  X" with known answers in the graph, run against `context` with and
  without the graph walk, scored by a human. Right Reasons' experiment,
  done honestly.
- **Lint debt.** Orphans, stale scrolls and open-too-long questions over
  time. These should trend down after milestone 3 and stay down.

If after milestone 3 the first and fifth measures have not moved, the
graph is not earning its keep and the ontology needs rework before
anything else is built.

---

## 13. v1 implementation decisions

- **Language: Rust.** A library and Clap/Serde binary replace the draft's Go
  proposal. No Go or Python runtime is required by the CLI.
- **Storage: private Dolt CLI 2.2.0.** Scrolls invokes a pinned binary directly,
  serializes processes with a local lock, and uses `.scrolls/dolt`. It neither
  runs a daemon nor connects to Beads. Its private configuration disables
  update checks and telemetry.
- **Bodies: Markdown.** Common fields and arbitrary kind-specific JSON fields
  accompany the body. Runtime registry editing remains deferred.
- **Retrieval: deterministic local graph and text.** Embeddings and model calls
  are deferred. Context budgets count compact item-array bytes. `prime` delivers
  mandatory doctrine in full beyond the budget when necessary; accepted local
  question/answer pairs are optional indivisible groups. `context` remains bounded.
- **Reference ownership:** `about`, `asks_about` and `related` belong to their
  source only. Referenced records retain their revision and attribution.
- **Exports:** all native kinds receive stable ID-based views plus a lossless
  graph sidecar in both formats. Imported ADR numbers are optional compatibility
  metadata; native decisions receive no number. See [commands](commands.md) for
  layouts, compatibility mapping and frozen-history preservation.
- **Answers: separate records.** Local questions admit multiple offered answers;
  their human owner accepts one. Imported questions remain immutable history.
- **Authority: unchanged.** Qualification proves a local import, workflow, and
  export. Remote synchronization and documentation authority cutover need their
  own work and acceptance.

---

## 14. Prior art consulted

- Beads (gastownhall/beads) — storage model, hash ids, `prime`, Dolt sync.
  Its memory store (`bd remember`) is a key and a free-text value with no
  types, links or provenance, which is why it does not fill this gap.
- Gas Town (gastownhall/gastown) — one Dolt server per town, sync through
  Dolt remotes, agents told to use `bd remember` over memory files.
- memdolt (kninetimmy/memdolt, Apache-2.0) — proposal branches, `review
  accept`, fact verify and supersede, a 90-day ranking decay on unverified
  facts, an accept-time contradiction check with a local model, per-actor
  attribution, `render`. Right architecture, no users yet; its author has
  not migrated to it.
- Right Reasons (martinarva, BSL 1.1) — 14 object types in four layers
  (beliefs, goals, rules, operations), reified "why" assertions on
  relations, propose → review Dolt diff → promote, read-only SQL for
  agents. Self-reported 100% versus 0% "why" recall against Markdown.
- ContextNest (PromptOwl, AGPL-3.0) — deterministic set-algebraic
  selectors over typed Markdown, hash-chained version history, audit of
  which document versions an agent consumed, point-in-time reconstruction.
  Reported dense retrieval non-deterministic on 80% of repeated queries.
- Google Open Knowledge Format v0.1 (June 2026) — one concept per Markdown
  file with a YAML `type`, links as graph edges. A format, not a platform.
- ontology-atlas (MIT) — Markdown nodes with `kind` in frontmatter,
  mandatory `--why` on dependencies, dry-run writes, mtime guards against
  concurrent human edits, MCP over stdio with a read-only mode. States it
  has not measured a difference against a no-ontology baseline.
- archgate — ADRs with attached checks that run against the codebase.
- Straymark — typed governance documents and a drift check comparing a
  charter's declared files to the real git diff.
- Codified Context (arXiv 2602.20478) — hot-memory constitution, cold-memory
  knowledge base loaded on demand, measured over 283 sessions.
- StagedWorkspace (arXiv 2608.18050) — every view of a workspace should be
  tied to a version of its state by content hash.
- Karpathy's LLM wiki — raw → wiki → index, with ingest, query and lint.
- DoltHub, "Does Dolt need MCP?" — agents already speak SQL and git.
- The Liskov documentation system (`docs/meta/documentation-system.md`),
  the doctrine router (ADR-0144), the stale-checkout study in AGENTS.md,
  and the work-store rehearsal (`work-store/README.md`), which this design
  inherits most of its discipline from.
