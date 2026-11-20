# Scrolls 0.3 design

Scrolls is a local document graph backed by private Dolt 2.2.0. Its engine is
independent of an information architecture. `schemas/knowledge.yaml` supplies
the bundled knowledge ontology; `examples/handbook.yaml` and
`examples/research.yaml` demonstrate different kinds, fields, and relations.

Work identities are a separate host-supplied seam, `work_ref::Adapter`. Its four
behaviours are reference ownership and canonicalization (with malformed owned
input refused), caller-local resolution at link time, Markdown work-link
recognition, and offline summaries. `canonicalize` and `resolve_local` return
`None` for syntax they do not own and `Some(Err(_))` for malformed owned input.
The default `body_references` recognises inline Markdown link targets using
`canonicalize`, retaining malformed targets for validation. Local short forms
never enter the graph; stored portable aliases fail canonical validation.

Standalone Scrolls supplies `beads::Local`, backed by `beads.json` bindings and
cache; its canonical form remains `beads://PROJECT-UUID/FULL-ISSUE-ID`. The CLI,
HTTP server and shared Dolt paths supply this adapter. Embedders pass their own
`&dyn work_ref::Adapter` to policy checks, `mutation::apply`, import building and
application, `query::bead_packet`, and `read::View::work`. Every adapter method
is offline: reads start no subprocess and make no network call. The read
summary envelope retains its `bead` field for compatibility; its payload comes
from the supplied adapter. Work relations are selected from the ontology's
`external` target, never from a fixed list of relation names.

The active ontology is a typed configuration in Dolt metadata. YAML (or JSON,
a YAML subset) is its authoring and exchange representation. Configuration
format 2 defines kinds, fields, lifecycle transitions, actor write policy,
ownership guards, endpoint rules, cardinality, named declarative invariants,
cycle restrictions, instructions,
and ordered startup pins. A deterministic content digest identifies the schema
revision. `schema apply --expect` always creates a reviewed proposal. Acceptance
merges into a temporary branch, validates the resulting configuration and graph
against current main, then advances main atomically. No transformation migrations
or silent kind/field renames are provided.

A format-2 kind can declare `alias_sequence: {prefix: ADR-, width: 4}`. Prefixes
are nonempty and unique; widths are positive. `KnowledgeStore::allocate_aliases`
prepares aliases for new native records relative to main. Its caller holds the
same exclusive lock or transaction from reading main through committing records,
proposal status and receipt. The rule is shared by direct landing, CLI review
preview and both accept implementations. Allocation scans held aliases rather
than storing a counter, so existing stores need no migration. It leaves imports
and existing main records untouched. Alias-bearing sequences and their held
aliases remain protected by full-graph change validation.

Local accept allocates on the merged temporary review branch before validation,
then commits aliases, proposal metadata and receipt together. Shared accept adds
the allocated record after-images to its durable intent delta and fingerprint.
Candidate validation and recovery replay that exact delta. A review preview is
not persisted; the existing token's main-revision binding makes it stale if
another landing changes the next number. No token or shared intent format changes
are needed.

Format 2 names the generic external work target `work`; standalone Scrolls resolves
it to the same canonical `beads://` references as format 1's `beads` target. The
format-specific name is selected in `Schema`, shared by relation validation,
link resolution and body-link extraction. Format 1 remains readable and writable
without normalization: stored ontology JSON and its content digest change only
through explicit configuration writes. Ontology format is independent of store
format (4 local, 5 shared), import-profile format and Markdown-plan format. New
stores and migration from store formats below 3 install the bundled format-2
ontology; migrating a later store preserves its configured ontology. The
[ontology upgrade guide](ontology-format-2.md) describes reviewed conversion and
older-writer limitations.

Records retain stable IDs, revisions, attribution, optional domain metadata,
custom JSON fields, original imported snapshots, paths and aliases. Field rules
include strings, numeric values, integers, booleans, enums, dates, timestamps,
typed lists and JSON. Glob validation occurs on writes. Imported validation debt
may persist unchanged; new violations fail. Frozen compatibility history remains
immutable. Reference assertions can affect only their source, so referring to an
immutable or independently owned document does not rewrite that document.

A batch is an ordered list of `new`, `edit`, `status`, `link`, and `unlink`.
`$handle` identifies a record created earlier in that batch. Expected revisions
refer to the initial snapshot; intermediate changes do not increment revisions.
Each affected existing record increments once. Transitions are checked in order,
and the complete graph is validated before one transaction writes graph changes
and one operation receipt. Any review requirement routes the whole batch to a
proposal. Ownership comes from pre-change records and relationships; review
checks the current main graph and persisted original ownership requirements.
Cooperative actor roles are attribution, not authenticated identities.

`prime` delivers the schema guide, authored instructions, and explicit pins.
It never selects documents automatically. Its soft budget measures the compact
JSON content object containing `guide`, `instructions`, and `items`. All content
is delivered, and overruns are explicit. Pins resolve to stable IDs when applied;
reads use their current versions on the selected branch. Consumption auditing
includes delivered record IDs and revisions.

Record labels add movable names for exact revisions of a record. A human
moves a label on local main in the same commit as its receipt. Labels are
versioned registry rows and require no store migration. A run that reads
through a label records the immutable store revision it resolved and the
record revision at that snapshot.

`show` and `context` share immediate relationship references. References contain
direction, relation, ID, title, kind, status and revision, without neighboring
bodies. Context puts the document first, enforces a hard budget, and identifies
the minimum required size when the document will not fit. Beads context contains
cached metadata and directly linked references only; ordinary reads stay offline.

Discovery uses consistent kind/status/owner/field filters. Structured query
files are selection expressions with Boolean composition, comparisons,
existence, relation counts, globs and date arithmetic. Actor, paths and evaluation
time appear in output bindings. Applicability combines configured glob fields,
glob edges, and imported routing; unresolved text remains diagnostic data.
Walk is bounded and direction-aware, returning references and traversed edges
unless full records are explicitly requested.

Store format 4 and JSON envelope 2 accompany package version 0.3.0. Migration
requires resolved proposals, preserves configured ontologies from format 3, and
installs the bundled ontology for older stores. It preserves record revisions,
store identity, receipts, legacy source files and history. Format-4 DDL is prepared
and validated on a journaled temporary Dolt branch before main advances.
Historical branches remain readable. Native export includes the complete graph,
active schema and schema revision. The Liskov import/export adapter retains its
explicit compatibility checks and independent corpus qualification.

Retrieval profiles, execution stages, workflow languages, generated summaries,
embeddings, automatic expansion, live ledger migration and documentation-authority
cutover are outside this release. This design supersedes proposals for retrieval
profiles and ontology-specific authoring commands.

Configurable [import profiles](import-profiles.md) map committed Git files into
the active ontology. `markdown_frontmatter` and `rst_headers` preserve originals,
raw metadata and source spans. Plans bind a read-only store snapshot, embedded
profile, source tree, actor and candidate digest. Apply recomputes the candidate
and uses one transaction for graph, provenance, originals and operation receipt.
Protected or guarded imports route to one proposal with the same isolation.

Engine-owned source membership protects captured properties and imported edges
across mutations, schemas and acceptance. Local notes and relationships can cite
captured records. Native export keeps original source subtrees and executable
modes, plus normalized views, source links and reference assertions. Generic
sources cannot use Liskov export. Changed-source reconciliation is deferred.

## Engine and store boundary

The engine modules are `model`, `schema`, `store`, `policy`, `mutation`, `query`,
`triggers`, `presentation`, `work_ref`, `profile`, `review`, `review_token`, `read`,
`render`, `import`, `source_import`, `markdown_import`, `markdown_links` and
`beads`. They accept typed requests and reach stored knowledge through
`KnowledgeStore` in `src/store.rs`. Its branch selection chooses main or a named
proposal as the session view for subsequent unpinned reads and commits.

The Dolt backend lives in `src/storage.rs` and `src/storage/`. It owns SQL
encoding, request sizing, branch checkout, transactions and the shared recovery
protocol. The CLI adapter (`src/cli.rs`, `src/main.rs`, `src/app.rs`) and HTTP
adapter (`src/http.rs`) open concrete stores and translate external requests.
Importer adaptation lives in `src/app.rs`: local source imports release their
read-only store before building and opening the write-locked store; shared
imports retain one session from the first base read through publication.

The engine keeps intentional I/O: Git reads of pinned import sources, the render
export directory, Beads `bd` calls and `beads.json`, and plan and profile files.
`source_import::persist` and `review::receipt` retain SQL-text wrappers for tests
and the shared protocol. The adapters keep concrete Dolt maintenance operations,
and revisions, proposal branch names and diff text still carry Dolt's shape.

## Atomic graph validation

The enforcement point is the `KnowledgeStore` write, not the command.
`policy::validate_change` is the single backend-neutral graph check, with no SQL
or backend calls. It compares a destination's before graph to its candidate,
including field rules, frozen history, cardinality, endpoints, links, cycles,
`Schema::validate` and pin resolution. Existing violations are tolerated only
for the same rule and record; changed rules are checked against existing rows.
Guards, write/review authority, review tokens and importer health are separate.

`commit`, `commit_checked`, proposal publication and acceptance enforce the
check, including shared recovery/resume of a graph write. Local writes assemble
typed after-images under the process's exclusive store lock. Local acceptance
checks the merged candidate against current main before publishing it. Shared
writes check the transaction's actual graph before its Dolt commit, under the
database-wide publication lock. Raw Dolt SQL and migration maintenance remain
below this engine contract.

`bootstrap` is the sole debt-creating write: the legacy initial import and shared
Markdown bootstrap require an empty store and record exactly the imported
graph's lint as `lint_debt` in the same write, using it as the tolerated baseline.
Ordinary writes cannot create import debt. Receipt-only replays/rejections/retries
may skip the graph check. Refusal leaves branch heads unchanged and carries the
`graph_write_refused` diagnostic and lint identities documented in
[commands](commands.md#graph-write-refusals).
