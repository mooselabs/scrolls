# Shared editable Markdown bootstrap

This development preview imports committed Markdown into a fresh shared store.
Git remains the source authority until its owner performs a separate handover.
It uses Dolt 2.2.0, shared format 5 and writer generation 4. Existing preview
stores require fresh initialization; this unit does not migrate them.

## Plan, inspect, apply

Initialize with the bundled knowledge ontology using the reviewed procedure in
[shared authoring](shared-authoring.md). As a human, pin a full source commit:

```sh
scrolls --connection /absolute/connection.json --actor alice --role human --json import plan markdown /path/to/orchestrator/docs --rev FULL_COMMIT --git-web-url https://github.com/proof-computer/liskov-agent-orchestrator --out /tmp/markdown.plan.json
scrolls --connection /absolute/connection.json --actor alice --role human --json --op-id markdown-initial import apply /tmp/markdown.plan.json --dry-run
scrolls --connection /absolute/connection.json --actor alice --role human --json --op-id markdown-initial import apply /tmp/markdown.plan.json
```

Inspect the plan's source commit, inventory, counts, frozen records, validation
debt and reference report, then the apply dry run's encoded SQL bytes and server
budget. The plan destination must be fresh. Planning changes no database state.
Apply requires the same actor/role, store UUID, main and schema revision, and
recomputed candidate digest. Bootstrap refuses nonempty document/source stores,
pending proposals, unresolved operations and custom ontologies. Human roles
remain cooperative client policy, rather than SQL account authentication.

The explicit web URL is an HTTPS GitHub repository root, without credentials,
`.git`, a branch or query. Other Git hosts are not qualified by this unit.

## Selection, fidelity and ownership

Only regular committed files whose names end in lowercase `.md` beneath DIR
are selected. Tracked symlinks in that selection refuse. Working-tree changes
are not imported. Git objects are read without replacement refs, inherited Git
redirection or archive export transformations. Source limits are 64 MiB per
file, 1 GiB total and 100,000 selected files; shared encoded SQL limits also
apply. A failed or timed-out write requires reconciliation with its original
operation ID. Bulk connections can explicitly select the existing bounded
`io_timeout_seconds` setting.

The existing local mapping preserves original Markdown bytes and modes,
metadata, stable IDs, ADR/work aliases, repository entities, relationships,
routing and historical lint debt. Originals and the imported baseline are
checksummed separately from editable current records. `doctor` reports
`markdown_health`; render, review and database backup check originals too.
Non-Markdown contents remain in Git; only their pinned inventory is retained.

Imported current documents use ordinary `edit --expect`, proposals, revision-bound
human acceptance and new-document commands. Backlog, epic, milestone, question
and untyped historical records retain the local importer's frozen behavior.
This does not turn historical questions into a new live work tracker.

## Retry and recovery

Documents, edges, source originals, import metadata and the operation receipt
commit atomically under one shared publication lock. Canonical hex-literal DML
is sent in approximately 1 MiB batches within that one SQL transaction (a larger
single statement stays intact). The total encoded-operation limit still applies;
batching never publishes partial progress. Completed receipt lookup
precedes stale-base and source-checkout access. Copy the plan to another client
and repeat the original operation ID and attribution to reconcile a lost
acknowledgement, even after later edits or source deletion. Reusing that plan
with another ID can record an identical-source no-op; it never replaces edits.
Changed-source refresh is unsupported and requires a fresh store.

An unpublished retry still needs its pinned Git objects. Bootstrap is a direct
atomic write, not a proposal intent: `recover --resume` is reserved for the
existing journaled proposal/review operations. Preserve indeterminate state;
never substitute a fresh operation ID to guess whether publication succeeded.

## Generated Markdown references

Both export formats keep imported-document links relative. Verified references
to other pinned Git files or directories become GitHub `blob`/`tree` URLs at the
imported commit, preserving query and fragment suffixes. Images use commit-pinned
GitHub raw content URLs so Markdown image renderers receive image bytes; a shared
reference definition used by an image also uses raw content. Projection covers
CommonMark links, images, reference definitions and top-level `source_documents`
scalars. It preserves labels, titles, code examples and unrelated formatting.
HTML links and arbitrary prose/path literals are outside this projection.

This changes generated views only: record bodies, original bytes and frozen
records remain unchanged in storage. `SCROLLS-MANIFEST.json` includes
`git_references.rewritten` and `git_references.unresolved`, with original target
spellings, source-document byte offsets, resolved paths and projected URLs. GitHub repository
permissions continue to govern retained references.
The plan carries a corresponding original-source report. View offsets refer
to the pre-projection rendered document; plan offsets refer to original bytes.

Missing paths, member checkout references and generated-file references remain
unchanged and are reported. They do not prevent bootstrap; their disposition
belongs to the later document rehearsal. The importer never invents a pinned
member-repository revision or stores retained asset contents.

## Qualification

`cargo test --locked --all-targets` includes two-client bootstrap, stale/wrong-store
plans, roles, packet refusal, Git hostility, originals, frozen history, authoring,
review, corruption, lost acknowledgement and backup/restore with a pending edit.
The parser unit tests cover exact spans, Unicode metadata, nested images,
reference definitions and code preservation.

Run the independent full-corpus oracle against a pinned repository and a fresh
output directory:

```sh
python3 scripts/verify-markdown.py /path/to/orchestrator --rev FULL_COMMIT --binary target/release/scrolls --out /tmp/markdown-qualification
```

It starts and stops its own server, compares every stored original against raw
Git blobs/modes/IDs, exercises generated references and native/shared authoring,
and verifies database backup/restore. Its receipt records the binary digest,
source revision, counts, timings, packet budget and reference coverage. It never
accepts a live connection or switches documentation authority.
