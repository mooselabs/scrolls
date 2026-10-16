# Scrolls 0.2 design

Scrolls is a local document graph backed by private Dolt 2.2.0. Its engine is
independent of an information architecture. `schemas/knowledge.yaml` supplies
the bundled knowledge ontology; `examples/handbook.yaml` and
`examples/research.yaml` demonstrate different kinds, fields, and relations.

The active ontology is a typed configuration in Dolt metadata. YAML (or JSON,
a YAML subset) is its authoring and exchange representation. Configuration
format 1 defines kinds, fields, lifecycle transitions, actor write policy,
ownership guards, endpoint rules, cardinality, cycle restrictions, instructions,
and ordered startup pins. A deterministic content digest identifies the schema
revision. `schema apply --expect` always creates a reviewed proposal. Acceptance
merges into a temporary branch, validates the resulting configuration and graph
against current main, then advances main atomically. No transformation migrations
or silent kind/field renames are provided.

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

Store format 3 and JSON envelope 2 accompany package version 0.2.0. Migration
requires resolved proposals, installs the bundled schema with empty pins, and
preserves record revisions, store identity, receipts, source files and history.
Historical branches remain readable. Native export includes the complete graph,
active schema and schema revision. The Liskov import/export adapter retains its
explicit compatibility checks and independent corpus qualification.

Retrieval profiles, execution stages, workflow languages, generated summaries,
embeddings, automatic expansion, live ledger migration and documentation-authority
cutover are outside this release. This design supersedes proposals for retrieval
profiles and ontology-specific authoring commands.
