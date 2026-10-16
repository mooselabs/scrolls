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
| `render --out NEW_DIR [--format native\|liskov]` | Export a complete fresh tree. Both formats include normalized `scrolls/graph.json`, an ID-to-path manifest, and views for every native kind. Liskov rejects incompatible edits to imported legacy documents. |
| `list [--kind KIND] [--status STATE] [--about ENTITY]` | List matching records in stable ID order. |
| `show ID` | Record, revision, and incident graph edges. IDs, unique legacy IDs, and source paths resolve. |
| `search TERMS [--limit N]` | Case-insensitive AND search over title/body, stable ID order, excluding notes. |
| `walk ID [--rel REL] [--depth N]` | Deterministic breadth-first graph neighborhood, traversing incoming and outgoing edges. |
| `context ID [--budget BYTES]` | The design's fixed graph traversal, with supporting evidence and neighboring entities as titles only. |
| `prime [--for PATH]... [--budget BYTES]` | All matched doctrine, then optional current primers, owned open questions, and answered questions with their accepted answers. |
| `new KIND --title TITLE [BODY] [--owner NAME] [--about ENTITY]... [--supports EVIDENCE]... [--field KEY=VALUE]...` | Create a record; protected kinds and changes to protected assertions land on proposals. Referencing a protected target through `about`, `asks_about` or `related` stays direct. |
| `edit ID --expect REV [BODY] [--title TITLE] [--owner NAME] [--field KEY=VALUE]...` | Optimistic edit. Reserved fields use dedicated options or graph commands. |
| `link FROM REL TO --expect REV [--expect-to REV] [--why TEXT]` | Add an attributed edge. `about`, `asks_about` and `related` revise only FROM; their `--expect-to` is an optional read check. Other relations with a Scrolls target require both revisions. |
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
A changed native review cadence on an imported record requires native export.

The default context budget is 16,000 bytes of compact JSON `items`, including
brackets and separators. `used_bytes` reports the actual size; the envelope
and omission list are excluded. Context projections omit `original` snapshots;
`show` and the graph export retain them. `context` enforces a hard budget.
`prime` includes every matched doctrine body once, in stable ID order, without
truncation. Its `mandatory_bytes` is the size of the mandatory-only array
(zero when there are no matches). Mandatory doctrine consumes the budget first
and may exceed it; `over_budget_bytes` reports that overrun in JSON and human
output. No optional records fit after an overrun. Unresolved scope clauses
remain explicit in `undecided_doctrine`.

An answered local question and its unique accepted answer form one optional
budget group: both are included in full, adjacent, or both are omitted. The
question exposes `accepted_answer_id`, the answer exposes `question_id`, and
both IDs enter the consumption audit when delivered. Attribution and revision
are retained. Offered and declined alternatives are excluded. Missing or
multiple accepted answers fail explicitly. Imported frozen questions remain
excluded.

Doctrine `--field glob=VALUE` accepts a nonempty string or an array of nonempty
strings; `[]` means no glob triggers. `--field scope_clause=TEXT` requires a
nonempty string. Globs compile before creation, edits, activation and acceptance
commit, including imported routes and `applies_to glob:…` assertions. Invalid
active triggers fail routing explicitly, and `lint` reports invalid triggers.
Scope-only doctrine and valid unmatched globs are valid.

The source owns `about`, `asks_about` and `related`, including `new --about`,
`ask --about` and extracted body links. Only the source revision and attribution
change. A supplied `--expect-to` checks the target revision without editing it;
a frozen target is allowed. Frozen sources remain immutable, protected sources
still require proposals, and other relations retain their endpoint safeguards.
Old proposals are not rewritten; reject and recreate one if its unnecessary
endpoint edits conflict.

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
Beads access is explicit through the integration below. Dolt uses a private
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
parents, unsafe archived paths, collisions, incompatible imported edits, and
frozen changes are rejected before publication. `RENDERED` and
`SCROLLS-MANIFEST.json` carry versions without changing legacy frontmatter.
The manifest's `records` maps every stable ID to its rendered path. Both export
formats include `scrolls/graph.json` with complete native fields, exact
lifecycles, attribution and edges.

| Native kind | Rendered location |
| --- | --- |
| Evidence | `raw/YYYY-MM-DD-sc-….md` |
| Knowledge and primer | `knowledge/sc-….md` |
| Note | `work-notes/sessions/YYYY-MM-DD/YYYY-MM-DD-sc-….md` |
| Decision, doctrine, entity, collection, question, answer | `scrolls/<kind>/sc-….md` |

Dates come from creation time, so filenames stay stable across edits. Every
local view appears in `scrolls/README.md`, with human-readable relationships
and accepted-answer links on its page. Imported manifest entities also receive
supplemental views. Compatibility frontmatter uses `raw`, `knowledge`,
`work_note`, `index` or `meta`; each page visibly retains its actual Scrolls kind
and lifecycle. Only imported manifest repository entities enter `related_repos`.
Stable ID body links resolve to relative exported paths with anchors preserved;
stored bodies stay unchanged. Affected legacy directory indexes refresh for
additions as well as edits. Unchanged imported files, including frozen history,
remain byte-for-byte copies.

Imported numbered ADRs carry `metadata.number`, such as `"0236"`, and retain
legacy lookup aliases. Older stores derive missing numbers during reads without
writing or revising records. New decisions never receive sequential numbers
and are discoverable through the supplemental index. Local questions and
answers never become legacy `Q-…` files or ledger records. Liskov's ADR-specific
readers require a later migration before native decisions can be authoritative
catalogue entries. An imported ADR cannot be marked superseded by a native
unnumbered decision in Liskov output; that requires a numbered legacy successor.

## Beads references

Qualified adapter: Beads **1.3.0**, selected with `beads bind --bd`. Binding,
refresh, and publication verify that version and the workspace's persistent
`project_id`. Aliases and machine paths remain local. References persisted in
edges use `beads://PROJECT-UUID/FULL-ISSUE-ID`; commands also accept
`NAME:FULL-ISSUE-ID` after binding. Bare or abbreviated IDs are not inferred.

| Command | Behavior |
| --- | --- |
| `migrate` | Atomically upgrade schema 1 to 2, adding a persistent Scrolls store UUID and relation registry entries. Main only; resolve pending proposals first. Supports actor, dry run, and operation receipts. Historical branches remain readable. |
| `beads bind NAME --directory PATH [--bd PATH] [--timeout SECONDS]` | Pin project identity and a local executable/workspace binding. Default executable is `bd`; timeout defaults to 30 seconds and allows 1–300. Rebinding may change local paths but cannot change project identity. |
| `beads list` | Local bindings, cached observations, and publication results; no Beads contact. |
| `beads refs REF` | Referencing documents with relation, lifecycle, and revision, from the selected Scrolls branch. |
| `context --bead REF [--budget BYTES]` | Linked documents and their entity context, ordered deterministically; no Beads contact. |
| `beads refresh REF` or `beads refresh --all` | Explicit read-only Beads lookup. All means all qualified references on the selected branch. |
| `beads publish REF` or `beads publish --all` | Explicit main-to-Beads backlink reconciliation. Requires actor, rejects proposal selection, supports dry run and operation IDs. |

`link`/`unlink` accept these relations to Beads targets:

- `related_work`: any record.
- `implemented_by`: decision, knowledge, primer; records intent, not completion.
- `produced_by`: evidence, knowledge, primer, note.
- `blocks`: question; records an informational relationship, without scheduler enforcement.

The source revision is required; an external target has no Scrolls revision.
Protected sources still require a proposal, and frozen imported records remain
immutable. Canonical Markdown links acquire a fallback `related_work` edge using
the existing body-link convention. Removing prose does not remove asserted edges;
use `unlink`. Legacy unqualified `blocks` targets are preserved and lint warns;
only qualified references participate in this integration.

`show` includes related work with cached resolution information. It also accepts
`scrolls://STORE-UUID/RECORD-ID` and checks that `--store` selects the matching
store. This URI is a portable CLI reference; no browser protocol handler is installed.
Native exports add related-work navigation and per-bead backlink pages. Both
formats retain work relationships in the normalized graph; Liskov supplemental
records show those references without changing the legacy work ledger.

Context prioritizes open questions, accepted decisions, current knowledge, active
doctrine, then accepted answers. Other linked states and evidence are summaries
with explicit lifecycle; notes remain excluded. Accepted answers to linked
answered questions are included. Entity expansion follows the existing context
rules. This is not a complete path-based doctrine evaluation. Budgets bound the
compact `items` array, as in ordinary context; relationship and bead metadata in
the envelope are excluded. Consumed records retain exact Scrolls commit attribution.

### Local observations and failure behavior

`.scrolls/beads.json` holds bindings, cached summaries, and publication receipts,
protected by the store's process lock and atomic file replacement. These are local
integration state, not reviewed knowledge. Copying a store does not make its
machine bindings portable. Ordinary show/context/render/refs never invoke Beads.

Refresh records the observed title, status, issue update time, observation time,
and latest attempt. Cached state is always labelled as such. On lookup failure,
the previous successful observation remains, with an explicit failure. Missing
bindings, unavailable executables/stores, malformed output, timeouts, and identity
mismatches never turn into an invented current status. Bulk commands return
per-target results, `data.failed`, `ok: false`, and exit 1 on partial failure.

### Published backlinks

Each Beads issue carries one top-level key per Scrolls store:
`scrolls/STORE_UUID_WITHOUT_HYPHENS`. This spelling satisfies Beads' metadata-key
query grammar. The value has `schema_version: 1`, `store_id`, `source_commit`, and
sorted `documents` containing ID, URI, title, kind, lifecycle status, record
revision, and relation names. Main membership does not imply lifecycle acceptance.

Publication uses atomic metadata merges/unsets and preserves every other top-level
key, including other Scrolls stores. It never updates issue descriptions, status,
assignee, dependencies, or `external_ref`. No hooks or remote push are requested
by Scrolls; the adapter runs with Beads' sandbox flag. Beads' own configured local
mutation hooks remain Beads behavior.

`publish --all` considers current graph links, previous publication targets, and
all Beads issues carrying this store's metadata key, including closed issues.
It can therefore remove obsolete backlinks after receipt loss. Removing the last
link removes only this store's key. Unchanged document projections are skipped;
their original `source_commit` remains the version actually published.

The Scrolls lock pins main throughout a publication. Per-target receipts are
saved before/after external work, and every changed projection is read back.
There is no distributed transaction. A crash can leave some beads updated;
rerunning reconciles actual state rather than trusting an old receipt. An explicit
`--op-id` binds the command, actor, and source Scrolls commit; use a fresh ID if
main changes. Dry runs read the proposed diff without updating either store or
local integration state. Publication never runs during review acceptance.
