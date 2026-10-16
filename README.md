# Scrolls

**Versioned knowledge graphs for humans and agents, backed by [Dolt](https://github.com/dolthub/dolt).**

Scrolls keeps evidence, decisions, questions, and working rules connected across
sessions. Capture a finding, link the understanding it supports, review the
change, and retrieve the context for the next task—with attribution and history.
Markdown bodies stay readable; typed records and relationships make them queryable.

Scrolls is the knowledge companion to [Beads](https://github.com/steveyegge/beads).
Use Beads to track work and Scrolls to record what you know, why you believe it,
and which rules apply. The two keep separate databases and connect through
explicit references.

**Qualified platform:** Linux x86_64 · **CLI:** Rust · **Storage:** private Dolt 2.2.0

[Quick start](#quick-start) · [Commands](#essential-commands) ·
[Agent setup](#agent-setup) · [Beads integration](#beads-integration) ·
[Documentation](#documentation)

```mermaid
flowchart LR
    evidence["Capture evidence"] --> proposal["Propose knowledge or a decision"]
    proposal --> review["Human review"]
    review --> knowledge_store["Versioned knowledge graph"]
    knowledge_store --> context["Retrieve context and doctrine"]
    context --> evidence
    knowledge_store --> markdown["Render Markdown"]
    knowledge_store <-->|"Explicit work references"| beads["Beads"]
```

## Quick start

Build from source with **Rust 1.85+** and Git. The Dolt installer also uses
`curl`, `tar`, and `sha256sum`; the workflow below uses `jq` to read returned IDs.
Commands with shell variables use **fish**.

```fish
git clone https://github.com/mooselabs/scrolls.git
cd scrolls
./scripts/install-dolt.sh
cargo install --locked --path .
set -gx SCROLLS_ACTOR your-name
scrolls init
```

Ensure Cargo's install directory (normally `~/.cargo/bin`) is on your `PATH`.
The installer checks the pinned Dolt archive's SHA-256 and installs it at
`.tools/dolt`, leaving shared Dolt installations alone. Run these examples from
the checkout; [installation and paths](#installation-and-paths) explains using
Scrolls elsewhere.

Capture evidence, then propose a conclusion supported by it:

```fish
set evidence (scrolls --json --op-id evidence-demo new evidence \
    --title 'Rollback readback' \
    --body 'The interrupted transaction left no rows.' | jq -r .data.id)

set proposal (scrolls --json --op-id knowledge-demo new knowledge \
    --title 'Atomic local writes' \
    --body 'Writes and their receipts commit together.' --supports $evidence)
set record (echo $proposal | jq -r .data.id)
set branch (echo $proposal | jq -r .data.branch)

scrolls --proposal $branch status $record current --expect 1
scrolls review $branch
```

After inspecting the proposal, a human accepts it:

```fish
scrolls --role human review accept $branch
scrolls show $record
scrolls history $record
scrolls render --out ./knowledge-export
```

The export destination must be new. Review acceptance merges the proposal; the
explicit `status` command above is what makes the knowledge `current`.
The demo operation IDs identify these exact writes: reuse them for retries,
and choose new IDs for new work.

## Features

- **Typed knowledge:** ten record kinds with explicit lifecycles, owners,
  confidence, review cadence, and attributed relationships.
- **Local history:** Dolt commits retain changes to records and edges. Stable
  `sc-…` IDs survive edits and exports; imported IDs remain lookup aliases.
- **Reviewable changes:** protected records use proposal branches with readable
  body, metadata, and graph diffs before human acceptance.
- **Agent context:** deterministic search and graph traversal, versioned JSON
  output, and a session audit of consumed records and revisions.
- **Rules in context:** path-based doctrine routing; `prime` includes matched
  rules in full, even when they exceed the requested context budget.
- **Reliable authoring:** expected revisions reject stale writes, and operation
  receipts make retries with identical inputs safe.
- **Markdown interchange:** import committed documentation at a pinned Git
  revision; export all native kinds, relationships, and an ID-to-path manifest.
- **Beads connections:** find knowledge attached to a bead, explicitly refresh
  cached work details, and publish backlinks into Beads metadata.

## Essential commands

`ID`, `REV`, `BRANCH`, and `PATH` below are placeholders. Use `show` to obtain a
record's numeric revision and write results to obtain proposal branch names.

| Command | Purpose |
| --- | --- |
| `scrolls new evidence --title 'Finding' --body 'Observed result'` | Capture an attributed source. |
| `scrolls list --kind decision --status accepted` | List accepted decisions. |
| `scrolls show ID` | Read a record, its revision, and relationships. |
| `scrolls search 'rollback'` | Search titles and bodies; notes are excluded. |
| `scrolls walk ID --depth 2` | Explore incoming and outgoing graph edges. |
| `scrolls context ID --budget 16000` | Retrieve context around a record or entity. |
| `scrolls prime --for src/storage.rs` | Load matched doctrine and optional session context. |
| `scrolls edit ID --expect REV --body-file PATH` | Edit against the revision you read. |
| `scrolls ask 'Which retention policy should we use?'` | Open an owned question. |
| `scrolls inbox --owner me` | List your open local questions. |
| `scrolls proposals` / `scrolls review BRANCH` | Find proposals and inspect their changes. |
| `scrolls --role human review accept BRANCH` | Accept a reviewed proposal. |
| `scrolls history ID` | Inspect historical record snapshots. |
| `scrolls stale` / `scrolls lint` / `scrolls doctor` | Check review dates, graph integrity, and store health. |
| `scrolls render --out NEW_DIR` | Export a fresh Markdown tree and graph. |

Bodies accept `--body`, `--body-file PATH`, `--body-file -`, or piped stdin.
`edit` opens `$VISUAL` / `$EDITOR` on a terminal; `--editor` selects that behavior
explicitly. See the [full command reference](docs/commands.md) for flags,
relationships, question answers, dry runs, and JSON response contracts.

## Agent setup

Add guidance like this to the instructions your agent reads, after configuring
the store and Dolt paths for your workspace:

```markdown
This project uses Scrolls for versioned knowledge.

- Set SCROLLS_ACTOR to your agent identity before writing.
- Start with `scrolls prime --for <path>` for each path in scope.
- Use `scrolls context <id>` and `scrolls show <id>` to inspect relevant knowledge.
- Capture observations as evidence and link conclusions to their supporting evidence.
- Use `--json` for automation, `--expect` for revisions, and stable `--op-id` values for retries.
- Inspect proposals with `scrolls review <branch>` and leave acceptance to a human.
- Keep task status and execution dependencies in the project's work tracker.
```

`init` creates the store; agent instructions are configured manually. Every write
requires an actor, through `SCROLLS_ACTOR` or `--actor`. The default role is
`agent`. Roles provide cooperative local policy and attribution, not authenticated
identities; filesystem access can bypass them.

`context` has a hard byte budget. `prime` gives mandatory doctrine priority,
reports any overrun, and fits optional context afterward. Answered questions and
their accepted answers are delivered together or omitted together. Scope clauses
that require judgment remain explicitly undecided. No model is called.
Use `--session NAME` and `consumed --for-session NAME` to inspect the read audit.

## Knowledge and review workflow

| Kind | Use | Write policy |
| --- | --- | --- |
| `evidence` | Captures, observations, and source material | Direct |
| `knowledge`, `primer` | Supported understanding and introductions | Proposal |
| `decision` | Durable choices and their rationale | Proposal |
| `doctrine` | Rules with path or scope triggers | Proposal |
| `entity` | Repositories, services, teams, or other graph anchors | Proposal |
| `question` | An owned open question | Direct |
| `answer` | A response to a local question | Human direct; agent proposal |
| `collection`, `note` | Curated views and session notes | Direct |

Protected kinds use proposals for both human and agent authors. Reads use main
unless you explicitly select `--proposal BRANCH`. Human review accepts or rejects
the branch, while lifecycle changes such as `draft` → `current` and
`proposed` → `accepted` are separate operations. Rejected proposals retain their
history. Accepting an answer also requires the human question owner.

Relationships capture support, derivation, replacement, contradiction, and subject
matter. `about`, `asks_about`, and `related` assertions belong to their source;
referencing a protected target does not edit it. Other internal graph mutations
can require both endpoint revisions. See [relationship rules](docs/commands.md).

## Beads integration

The adapter is qualified against **Beads 1.3.0**. It keeps work references in
Scrolls and requires explicit commands to contact Beads. Existing schema-1 stores
need `scrolls migrate` after pending proposals are accepted or rejected; new
stores use schema 2.

Bind an existing Beads workspace and link the knowledge record from the quick
start. Replace `bd-example` with a full issue ID from that workspace:

```fish
scrolls beads bind work --directory /path/to/beads-workspace --bd /path/to/bd
set revision (scrolls --json show $record | jq -r .data.record.revision)
set link (scrolls --json link $record implemented_by work:bd-example --expect $revision)
set link_branch (echo $link | jq -r .data.branch)
scrolls review $link_branch
# A human accepts after reviewing the change.
scrolls --role human review accept $link_branch

scrolls beads refs work:bd-example
scrolls context --bead work:bd-example
scrolls beads refresh --all
scrolls --dry-run beads publish --all
scrolls beads publish --all
```

References are many-to-many and persist as `beads://PROJECT-UUID/FULL-ISSUE-ID`;
the `work:` alias and machine paths stay local. Reads remain offline. `refresh`
caches observed titles and statuses; `publish` reconciles main's backlinks into
this Scrolls store's Beads metadata key, preserving unrelated metadata.
Publication does not change issue status, assignees, dependencies, or scheduler
gates. `implemented_by` records intent, and question `blocks` links are
informational. See the [Beads command contract](docs/commands.md#beads-references).

## Installation and paths

The supported installation path is the source build in the quick start. Python 3
is needed for demonstrations and qualification scripts, not ordinary CLI use.
The private Dolt installer currently supports Linux x86_64 only.

`--store` defaults to `.scrolls` and `--dolt` to `.tools/dolt`, both relative to
the current directory. To use a different workspace, pass absolute paths on
each command:

```fish
scrolls --dolt /path/to/scrolls/.tools/dolt --store /path/to/project/.scrolls init
scrolls --dolt /path/to/scrolls/.tools/dolt --store /path/to/project/.scrolls prime
```

After installation, ordinary authoring and reads need no Git repository, daemon,
remote, or model service. Importing Markdown requires a Git source repository.

## Storage and history

The database lives at `.scrolls/dolt`. Scrolls invokes its pinned Dolt CLI with a
private configuration and serializes store access with a filesystem lock.
Authoring changes and operation receipts commit in one explicit transaction.
Records and edges are keyed JSON rows; those row payloads are the merge units.

The consumption audit and Beads bindings/cache are separate local state. An
unexplained dirty database stops commands for inspection; Scrolls does not
silently discard it. Start with `scrolls doctor` and the
[storage and recovery contract](docs/commands.md#storage-and-recovery).

This release provides local operation and proposal review. Remote synchronization,
server storage, authenticated access, embeddings, executable doctrine checks,
and web/MCP interfaces remain outside the implemented scope.

## Markdown import and export

For a Liskov import, start with an empty store and pin the source commit:

```fish
set source /path/to/liskov-agent-orchestrator
set revision (git -C $source rev-parse HEAD)
scrolls --json import markdown "$source/docs" --rev $revision
scrolls doctor
scrolls show ADR-0236
scrolls doctrine --for repos/liskov-rs/src/example.sql
scrolls render --format liskov --out ./liskov-docs-export
```

Import reads committed Git objects, preserving original metadata, paths, and
assets. Repeating the same source commit is safe; importing a changed source
requires a new store. Unchanged imported files render byte-for-byte. Imported
backlog, epic, milestone, question, and untyped archive records remain immutable.

Both native and Liskov exports include every native kind, `scrolls/graph.json`,
and `SCROLLS-MANIFEST.json`. Native decisions use stable `sc-…` identities and
separate views; imported ADR numbers remain metadata and aliases. Local
questions and answers are independent of the legacy inbox.

**Import and export do not transfer documentation authority.** The original Git
documentation remains authoritative for the orchestrator; this release makes
no authority cutover.

## Development and verification

From this checkout, after installing the private Dolt binary:

```fish
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release --all-targets
python3 scripts/demo.py --binary target/release/scrolls --out /tmp/scrolls-demo
./scripts/install-beads.sh
python3 scripts/verify-beads.py --binary target/release/scrolls
```

Real-Dolt tests fail if the private binary is missing. CI runs the Rust checks,
demonstration, and Beads qualification. Use a fresh demonstration output path.
Full corpus qualification is a separate local check against the private source:

```fish
python3 scripts/verify-corpus.py /path/to/orchestrator --rev COMMIT --out /tmp/scrolls-qualification
```

The pinned policy pack must already be present in that source repository's
verified cache. See the qualification reports for exact results and limitations.

## Documentation

- [Command reference](docs/commands.md): CLI, JSON, lifecycle, storage, and Beads contracts.
- [Design](docs/design.md): implementation boundary followed by the broader design.
- [Migration report](docs/migration-report.md): pinned corpus import/export evidence.
- [Correctness qualification](docs/correctness-qualification.md): doctrine, relationships, answers, and export checks.

The README's onboarding structure is inspired by the
[Beads README](https://github.com/steveyegge/beads/blob/main/README.md); the commands
and support boundaries above describe Scrolls.
