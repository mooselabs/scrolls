# Scrolls v1 command reference

Every command accepts `--json`, `--store PATH`, `--dolt PATH`, `--actor NAME`,
`--role agent|human`, `--session NAME`, and `--proposal BRANCH`. Main is the
default read branch. Proposal selection is explicit. Each successful response
has `schema_version: 1`, `ok: true`, `revision` (Dolt commit), `branch`, and
`data`. Errors have `ok: false` and `error.message`; they exit nonzero. Human
output renders bodies and review changes as readable text; other structured
results use pretty JSON. Every read prints its Dolt revision.

Writes accept `--op-id ID` and `--dry-run`. Replaying the same operation with
identical effective body, command, role, actor, source commit, and proposal
returns the stored result. A changed input fails. Receipts on pending and
rejected branches remain discoverable. The envelope identifies the revision
read by the retry; `data` is the original operation result.

| Command | Behavior |
| --- | --- |
| `init` | Initialize the private database and default registry; existing stores are checked. |
| `doctor` | Check the version pin, schema, clean working state, branches, and import provenance. |
| `import markdown DIR --rev REV` | Resolve a Git commit and import only committed documentation files, manifest repository entities, and AGENTS.md doctrine routes. |
| `render --out NEW_DIR [--format native\|liskov]` | Export a complete fresh tree. Default native includes normalized `scrolls/graph.json` and new records under `scrolls/`. Liskov rejects native additions and unrepresentable graph changes. |
| `list [--kind KIND] [--status STATE] [--about ENTITY]` | List matching records in stable ID order. |
| `show ID` | Record, revision, and incident graph edges. IDs, unique legacy IDs, and source paths resolve. |
| `search TERMS [--limit N]` | Case-insensitive AND search over title/body, stable ID order, excluding notes. |
| `walk ID [--rel REL] [--depth N]` | Deterministic breadth-first graph neighborhood, traversing incoming and outgoing edges. |
| `context ID [--budget BYTES]` | The design's fixed graph traversal, with supporting evidence and neighboring entities as titles only. |
| `prime [--for PATH]... [--budget BYTES]` | Matched doctrine, current primers, owned open questions and answered questions asked by this actor. |
| `new KIND --title TITLE [BODY] [--owner NAME] [--about ENTITY]... [--supports EVIDENCE]... [--field KEY=VALUE]...` | Create a record; protected kinds and changes touching protected records land on proposals. |
| `edit ID --expect REV [BODY] [--title TITLE] [--owner NAME] [--field KEY=VALUE]...` | Optimistic edit. Reserved fields use dedicated options or graph commands. |
| `link FROM REL TO --expect REV [--expect-to REV] [--why TEXT]` | Add an attributed edge. Both existing endpoints require revisions. |
| `unlink FROM REL TO --expect REV [--expect-to REV]` | Remove an edge, subject to the same endpoint policy. |
| `status ID STATE --expect REV` | Apply an allowed lifecycle transition. Answer acceptance has its own command. |
| `supersede OLD --with NEW --why TEXT --expect REV --expect-to REV` | Mark a decision superseded, or retire replaced knowledge/doctrine/entities, and attach the replacement edge. |
| `ask TITLE [--owner NAME] [--about ENTITY]... [--context TEXT] [--if-yes TEXT] [--if-no TEXT]` | New local question. Owner defaults to the first entity's owner, then the actor. |
| `answer QUESTION [TEXT] [BODY] --expect REV` | Offer an answer. Human answers are direct; agent answers are proposals. |
| `accept ANSWER --expect REV --expect-question REV` | Human question owner accepts an offered answer and marks its open question answered. |
| `inbox [--owner NAME\|me]` | Open local questions; imported frozen questions are excluded. |
| `proposals` | Pending, accepted, and rejected proposals with attribution and reasons. |
| `review BRANCH` | Body and metadata before/after plus graph changes. |
| `review accept BRANCH [--reason TEXT]` | Human-only temporary merge, structural validation, and guarded main advance. Does not automatically change record lifecycle status. |
| `review reject BRANCH --reason TEXT` | Human-only rejection retained on the original proposal branch; history remains. |
| `history [ID]` | Dolt commits, or historical record snapshots with commit attribution. |
| `lint` | Structural errors, imported debt, and missing-about warnings. Mutations reject newly introduced structural errors. |
| `stale` | Records whose review cadence has elapsed. |
| `doctrine [--for PATH]...` | Active rules matching globs; scope clauses are reported as undecided with a reason. No model calls or executable checks. |
| `consumed [--for-session NAME]` | Separate local JSONL audit of show/search/context/prime results and exact revisions. |

`BODY` means `--body TEXT`, `--body-file PATH` (or `-`), piped stdin, or
`--editor`. `new` also accepts `--confidence high|medium|low` and
`--review-every DAYS`; `edit` accepts these plus `--last-reviewed YYYY-MM-DD`.
A changed native review cadence on an imported record requires native export. The budget bounds the compact JSON `items` array in **bytes**, not
estimated model tokens. The packet reports the accounting and omitted IDs;
its metadata envelope and omission list are outside that bound.

Default kinds: decision, knowledge, primer, evidence, question, answer,
doctrine, entity, collection, note. Default relations: supports, derived_from,
supersedes, contradicts, about, applies_to, asks_about, answers, promoted_to,
depends_on, blocks, related. `blocks` can target an external work ID;
`applies_to` can target `glob:PATTERN`. `supersedes`, `contradicts`, and
`depends_on` require a reason. Body links to stable Scrolls IDs get a fallback
`related` edge when no stronger edge exists. Lifecycle and edge policies are
implemented in the library; registry editing is deferred.

## Storage and recovery

A filesystem lock serializes all access. Dolt tables contain stable keyed JSON
rows, so the merge conflict unit is a record or edge, not a Markdown file.
Original source files live in `source_files`, including hashes, modes, and
base64 bytes. Import metadata includes the exact commit and lint baseline.
There is no SQLite substitute or Beads connection. Dolt uses a private
`dolt-home` configuration with version checks and telemetry disabled; no shared
Dolt configuration is changed.

Every authoring/import/review mutation writes its receipt in the same explicit
SQL transaction and Dolt commit as its changes. A killed SQL transaction rolls
back. A review journal permits recovery of a known temporary review branch;
main is never reset during recovery. Otherwise a dirty database stops commands
with an actionable error. Inspect it using the private binary from the
`.scrolls/dolt` directory before deciding how to recover; Scrolls does not
silently discard unexplained changes. The consumption audit is independent of
Dolt commits and protected by the same process lock.

Rendering stages a complete tree beside the destination, then reserves and
renames into a fresh directory. Source paths, existing outputs, symlinked
parents, unsafe archived paths, unsupported Liskov additions, and frozen
changes are rejected before publication. `RENDERED` and
`SCROLLS-MANIFEST.json` carry versions without changing legacy frontmatter.
Six directory indexes are refreshed after representable edits. Unchanged
exports are byte-for-byte copies, with index content checked independently.
