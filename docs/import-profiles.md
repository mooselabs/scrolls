# Configurable import profiles

Status: implemented for Scrolls 0.3.0. Store format 4, profile/plan format 1,
JSON envelope 2. Qualification is recorded in [the release report](import-profiles-qualification.md).

An import profile describes how a source collection becomes Scrolls records:
which files are documents, how to read their metadata, what identifies each
document, and how source fields and references map into the active ontology.
The engine owns pinned Git reads, validation, identity, provenance, review and
atomic persistence. Profiles are small declarative YAML documents.

The first release imports an external snapshot into an existing store, preserves
its originals, and makes its documents addressable and searchable. It supports
multiple independent sources but does not update an already imported source to
a different snapshot. Identity and provenance are designed now so that later
updates can reconcile records without replacing their IDs.

## What this solves

The PEP experiment at Python commit
[`6068058b896441388bbf22f2607ab681193f6a3d`](https://github.com/python/peps/tree/6068058b896441388bbf22f2607ab681193f6a3d)
contains 744 PEPs and 820 tracked files under `peps/`. The current importer
preserves the files but creates zero records because it recognizes only `.md`
and Liskov frontmatter. An external adapter demonstrated that all 744 PEPs can
be held as searchable evidence with their original headers and assets retained.

The desired result is that a user can select a PEP profile and import the pinned
collection directly. A handbook using Markdown frontmatter should use the same
engine with a different profile. Neither collection should need a Rust mapping
function or a temporary Git repository of converted files.

## Configuration boundaries

| Component | Owns |
| --- | --- |
| Store ontology | Valid kinds, fields, initial states, relations, cardinality, cycles and write/review policy. |
| Import profile | File selectors, codec choice, source keys, aliases, field extraction and reference mappings. |
| Source invocation | Local checkout, Git commit, root inside the commit and stable source namespace. |
| Import engine | Snapshot reads, digests, source ownership, diagnostics, schema validation, plans, transactions and receipts. |
| Retrieval and rendering | How stored documents are queried, budgeted and displayed. |

A profile cannot define kinds, weaken constraints, change review policy, install
startup pins or define automatic rule activation. It uses the ontology already
active in the target store. Unknown kinds, fields or relations produce errors; there is no
fallback to another kind or relation.

The 0.2 ontology implementation is integrated with revision-bound review.
Profiles reuse its schema validation, defaults, graph constraints and policy.
They also work with custom schemas that disallow additional fields.

## User workflow

These commands are implemented. `import markdown DIR --rev REV` remains the
explicit Liskov compatibility command with its existing behavior.

```fish
scrolls import profiles list
scrolls import profiles show pep
scrolls import profiles validate ./my-peps.yaml

set peps /path/to/peps
set revision (git -C $peps rev-parse HEAD)
scrolls import plan git $peps --root peps --rev $revision \
    --source python.peps --profile pep --out ./peps.plan.json
scrolls import apply ./peps.plan.json --op-id peps-initial
```

`--profile pep` selects a bundled profile. `--profile-file ./my-peps.yaml`
selects a caller-supplied file; the flags are mutually exclusive. There is no
automatic discovery or execution of profiles from the source repository.
`--root .` is valid. Root and commit resolve within the given Git repository;
uncommitted and untracked files are excluded. Import never clones or fetches.

`profiles validate` checks syntax and compatibility with the selected store's
schema. Planning additionally checks actual files and the complete candidate
graph. Both are read-only to the store. Planning writes only the requested fresh
plan file and emits a human summary or versioned JSON with `--json`.

The plan includes resolved source commit/tree/root, source namespace, exact
profile bytes and hashes, engine/codec contract versions, target store UUID,
base Dolt revision, schema digest, candidate digest, coverage, diagnostics and
prospective changes. The candidate digest excludes observation timestamps and
random operation/proposal IDs. All source mapping output is deterministic.

Apply reads the pinned Git objects, recomputes and validates the plan, and
requires the same store UUID, base revision, schema and candidate digest. A stale
plan fails with instructions to replan. Editing the original profile file after
planning has no effect: apply uses the embedded profile. A plan is inspectable
input, not an approval token, SQL script, or permission to bypass review.

Protected writes route the **entire import**, including source files and the
manifest, into one proposal. Direct writes land in one main transaction.
Acceptance revalidates source identities, aliases, graph constraints and policy
against current main. If the pinned schema digest changed, reject acceptance
and require a fresh plan/proposal; do not reinterpret defaults during review.
Import does not accept its own proposal. A plan
cannot apply on an unrelated proposal branch in the first release.

## Profile format 1

The canonical runtime PEP and handbook profiles are
[pep.yaml](../profiles/pep.yaml) and
[handbook.yaml](../profiles/handbook.yaml). The format is independent
of ontology and database format versions.

| Key | Contract |
| --- | --- |
| `format` | Exactly `1`. |
| `id` | Descriptive profile name; not a source namespace or record identity. |
| `description` | Human explanation. |
| `documents` | Nonempty list of uniquely named document rules. |
| `assets` | `unmatched`: archive every other regular file under the selected root. |
| `coverage.min_documents` | Positive minimum, default 1. Zero-document imports fail. |
| `coverage.min_by_rule` | Optional nonnegative counts keyed by rule ID. Unknown rules fail. |

Each document rule has `id`, `include`, optional `exclude`, `codec`, `values`,
`identity`, `record`, and optional `references`. Globs are case-sensitive,
root-relative POSIX paths; `*` does not cross `/`, and `**` can. Overlapping
document rules fail instead of choosing the first match. A matched file that
fails parsing or mapping is an error, never silently reclassified as an asset.
Files excluded from document selection remain archived assets. Format 1 has no
discard-files mode; use a narrower root when appropriate.

Unknown keys, duplicate YAML keys, custom tags, anchors, aliases and merge keys
fail. A compiled profile is normalized to canonical JSON with recursively sorted
object keys; array order is retained. Its SHA-256 identifies meaning under the
versioned interpreter. The raw file hash and bytes are retained as well.

### Codecs

The first two codecs are `markdown_frontmatter` and `rst_headers`.

Both return a document object containing `/path`, `/stem`, `/body`, and parsed
metadata (`/frontmatter` or `/headers`), plus source spans and diagnostics.
`path` is relative to the selected root; `stem` is the final filename without
its extension. Text must be UTF-8. Raw source bytes are retained separately.

`markdown_frontmatter` accepts an optional opening YAML block and preserves the
remaining Markdown body. An absent block is an empty object. A present malformed
block or non-object metadata fails. This is deliberately stricter than the
legacy Liskov parser, which stays in the compatibility adapter.

`rst_headers` reads the initial RFC-style header block through its first blank
line. Header names are case-insensitive and exposed as lowercase keys. Indented
continuations are unfolded to a single space for mapping. Duplicate header names,
orphan continuations, malformed header lines and a missing terminator fail.
Ordered original names, values and spans remain in provenance, preserving
unknown headers even when the target schema disallows additional custom fields.
The RST body remains source text; no Sphinx build, include directive, raw HTML
execution or external resource fetch occurs.

A codec is reviewed Rust functionality. Profiles select codecs; they cannot
load executables, Python, JavaScript, shell hooks, network handlers or model calls.
New formats require a codec implementation; new mappings for supported formats
require only a profile.

### Values and mapping

`values` defines named scalar/list values. Each entry selects one document value
using `from`, a JSON Pointer into the codec result, followed by an optional
`normalize` operation. No selector searches the target store. Named values do
not refer to one another, so evaluation cannot recurse.

Initial normalizers are `trim`, `unsigned_integer`, `unsigned_integer_list`,
`string_list`, `lookup`, and `date` with an explicit format. No normalizer means preserve
the parsed type. Unsigned integers are decimal, nonnegative and bounded to u64;
leading zeros normalize away. Integer lists split a header on commas or
whitespace and reject every non-integer token. `string_list` accepts a string as
one item or an array of strings; it never splits author names on commas. `date`
uses a declared locale-independent format and returns `YYYY-MM-DD`.
`lookup` requires a nonempty `cases` object keyed by exact source strings, such
as `{adr: decision, raw: evidence}`. An unknown value fails; preserved original
metadata remains available. Only one normalizer is allowed per value in format 1.
Lookup results are still subject to target schema validation, so a mapping table
cannot introduce an undeclared kind or enum value.

`required: true` rejects missing and null values. Missing optional values remain
absent; an explicit `default` is used only for absence, never for null, false,
zero or an empty collection. Type errors fail even for optional values. Text
emptiness is checked by identity/title rules and the target schema.

Mapping expressions have exactly one of `literal`, `value`, or `template`.
`value` names a declared value. Templates interpolate only declared scalar values
and the built-in `source_id`; supported forms are `{name}` and `{name:04d}` for
zero-padded unsigned integers. There are no expressions, filters or recursive
templates. Missing template operands fail. A field mapping referring directly
to an absent optional value omits that field; required schema fields must still
be satisfied by a mapping or schema default.

`record.kind`, `record.title`, and optional `record.owner` use these expressions.
`record.fields` maps domain field names to expressions; the target schema owns
their types and defaults. Unknown fields fail when the schema rejects them.
An omitted owner is the invoking actor; source authorship is a separate mapped
field and does not implicitly grant local ownership.

Records start in the target kind's declared **initial** state. Format 1 does
not assign noninitial local states or execute lifecycle transitions. A source
`Status: Accepted` remains, for example, `fields.pep_status: Accepted` on an
`evidence/captured` record. The existing Liskov snapshot adapter retains its
explicit legacy-state mapping. Importing into another ontology is supported;
forcing arbitrary source approval states into local workflow is a separate
capability, not a permissive import flag.

`record.body` selects `source: body` and a representation:
`markdown` for a Markdown codec body or `fenced_source` with a language label
for literal source. The engine chooses a fence longer than any run in the
content. PEPs initially use fenced `rst`. Markdown conversion, Sphinx visual
parity, section retrieval and search ranking remain separate work. The raw body
format and source bytes remain recoverable regardless of presentation.

## Identity and source ownership

The caller supplies a stable `source_id`, such as `python.peps`. It has 1–64
lowercase ASCII letters, digits, dots, underscores or hyphens and starts with a
letter or digit. It identifies the collection independently of its local clone,
Git remote spelling, branch, import profile or latest commit. It is an explicit
name, not an authenticated claim of ownership.

Each rule produces a nonempty `identity.key` from the extracted values. For
PEP 8 it is `pep/8`. Identity template components must also be nonempty; a missing
or empty source ID field cannot turn into a shared key such as `chapter/`.
The record ID is `sc-` plus the full SHA-256 hex digest of
the canonical JSON array `["scrolls.import.record.v1", source_id, key]`.
Commit, path, profile hash and target kind are intentionally excluded. Moving a
file with an explicit key preserves its ID; using its path as a key makes a
rename an identity change. Duplicate keys anywhere in the source fail, even
when the documents have different kinds or rules.

Every record receives a canonical qualified lookup alias
`python.peps:pep/8`, plus a qualified source-path alias. Profile aliases add
forms such as `PEP-0008`, `PEP 8` and the canonical PEP URL. Qualified versions
always resolve within the source. Unqualified aliases resolve only when unique
across the selected graph; ambiguity identifies the qualified alternatives.
Duplicate aliases within a source are errors. An existing native ID collision
fails; imports never adopt or overwrite a native record by title or alias.
Aliases cannot shadow a stable record ID or another record's qualified alias.
Keys are exact UTF-8 strings after declared normalization; no implicit Unicode
normalization, case folding or number coercion is applied to identity templates.

Profile imports are **source-owned snapshots**. Their kind, local state, owner,
projected fields, body, aliases and imported assertions cannot be locally edited.
Local interpretations are separate native records. New local graph assertions
remain allowed under the ontology, including an imported evidence record
supporting a local conclusion. The normal endpoint revision and review checks
still apply; mechanical revision/actor changes do not rewrite captured content.

Membership supplies an engine ownership check over the captured payload and
imported edge keys. Do not implement it by setting the legacy `frozen` flag,
which would also prevent useful local assertions. Removing an imported edge
fails; adding an already imported edge is a no-op and does not transfer its
ownership. Local edges with other keys can be added/removed normally. Source
updates will have a distinct reconciliation path. Ordinary authoring and schema
changes cannot remove the captured-payload ownership guard.

## References without invented semantics

Reference rules use a named value containing one or more target keys and a
template such as `pep/{target}`. Resolution is a second pass over **all**
candidate identities, independent of file order. Format 1 resolves only within
the same source. It does not guess from global titles, short aliases or URLs.

Each rule names the source relationship (`Requires`, `Replaces`, etc.), direction
(`outgoing` or `incoming`) and optional target-schema `relation`.
The target template permits only `{target}` and integer padding. Missing optional
reference values produce no assertions. Malformed values fail parsing.

With a relation mapping, resolved references become ordinary graph edges and
must satisfy the ontology's endpoint, reason, cardinality and cycle rules.
Reasons identify the original source field and pinned path/commit. Multiple
source occurrences of one edge collapse to one edge while retaining every
provenance occurrence. `Replaces` and inverse `Superseded-By` therefore need not
create duplicate edges.

Without a relation mapping, the reference is retained as an **unmapped source
assertion**, including its target ID when resolved. It is visible in the plan,
`show` provenance and export but is not a graph edge. The portable PEP profile
leaves `Requires` unmapped because the knowledge schema does not allow
`depends_on` between evidence records. It maps the two replacement headers to
`supersedes`. A compatible custom ontology can supply an appropriate `requires`
relation through a changed profile; the profile itself cannot create one.

`on_missing: retain` keeps an unresolved assertion and warning; `error` blocks
the import. It never creates a placeholder record or drops the source text.
Invalid declared relations always fail, even if all their targets are missing.
Header references are in the first release; RST `:pep:` roles and general body
link extraction are deferred. The report explicitly states this coverage.

## Storage and export contract

Persist these logical collections, versioned with the store format:

| Collection | Key and contents |
| --- | --- |
| Source manifest | `source_id`; accepted snapshot commit/root/tree, source locator, full profile bytes/hashes, schema/interpreter digests, import actor/time and receipt. |
| Source files | `(source_id, path)`; original bytes, Git blob OID, SHA-256, mode, size and document/asset classification. |
| Import membership | `(source_id, key)`; stable record ID, source file and codec, raw metadata/spans, mapping rule, canonical mapped baseline and source reference occurrences/resolution. |
| Ordinary graph | Existing records and edges, validated by the active ontology. |

Use three new Dolt tables: `import_sources`, `import_files`, and `import_members`.
Each follows the existing keyed JSON-row convention (`id VARCHAR(128) PRIMARY
KEY`, `payload LONGTEXT NOT NULL`). A source manifest uses its source ID as key;
file and membership keys use SHA-256 of version-tagged canonical JSON tuples.
Store the unhashed tuple inside the payload and reject a hash collision rather
than overwriting a different tuple. Membership includes imported edge keys and
their source occurrences, so graph mutation guards can distinguish local edges.
The existing `source_files` table continues to hold only legacy adapter files.

Membership owns provenance; the record's domain `fields` must not become a
second provenance authority.
`show` can expose provenance on request, and render includes it; ordinary context
omits raw metadata and baseline copies. Source schemas cannot spoof engine
provenance through user fields.

Store format 4 adds these tables and the ownership-aware writer contract to
the format-3 ontology foundation. Old binaries must refuse writes. Migration preserves existing
legacy IDs, paths, aliases, source files, receipts and proposal history. Legacy
imports retain their old namespace/format interpretation; do not hash their
records again. Unresolved proposals block migration under the existing policy.

Generic records leave `Record.path` and `Record.original` empty. Source paths and
the canonical mapped baseline live in membership; native render paths use stable
record IDs. Existing legacy records keep their current path/original fields.
This prevents the current Markdown renderer from writing YAML into an original
`.rst` file and avoids another duplicate baseline inside query projections.

For a generic source, export untouched files under
`sources/<source_id>/<root-relative-path>` and normalized records in the normal
native views. The manifest maps record IDs to both their rendered view and
original source file, and includes each source's profile and import provenance.
All original files under a source retain their relative layout. The source
subtree is byte- and mode-preserving; normalized views do not claim to rebuild
the source's website. Unmapped/unresolved references remain explicit sidecar
data. The renderer must reserve its own manifest paths and reject collisions.

Generic imports are supported by native render first. Liskov render must either
use that same supplemental layout with qualification or reject a mixed generic
source explicitly; the first release chooses explicit rejection. Existing
legacy-only Liskov render stays byte-compatible. A dedicated reverse PEP/RST
editor or write-back exporter is outside this design.

## Transactions, replay and future updates

All candidate records, edges, source files, membership, the manifest and the
operation receipt commit together. Parsing or validation failure leaves none
of them behind. Proposal data is branch-scoped and is not visible on main before
acceptance. Import does not add its errors to historical lint debt.

An identical accepted import returns its existing result without revising
records. The same pending import returns its proposal. Receipt lookup precedes
the stale-plan check for an exact `--op-id` replay, so a retry after successful
apply can recover the original result. A different request with the same op ID
fails. Rejected receipts remain rejected; a new explicit operation can propose
the same source again. Rejection does not reserve the namespace on main.

The request fingerprint binds actor/role, store, source identity/commit/root,
compiled profile, interpreter/codec contract and schema. Apply verifies raw
profile bytes and plan integrity as well. A matching source snapshot already
in main is a no-op even under a new op ID. A pending namespace with a different
snapshot or mapping requires resolving that proposal before another import.

A new source can be added to a nonempty store. A changed commit, root, profile,
identity mapping or schema interpretation for an existing accepted source
fails with `source_update_unsupported` in the first release. Matching a content
hash at another path is not sufficient to infer a rename or identity merge.

Later reconciliation will compare the stored source baseline with the new
candidate, preserve IDs by source/key, and handle changed/deleted source records
and their local references explicitly. Imported fields are source-owned; local
records and local assertions remain independent. Deletion/tombstone policy,
conflict review, namespace renames and identity-key migrations need a separate
design before enabling updates. This release must not expose a misleading
`sync` command.

## Coverage and failure reporting

Every selected tracked regular file is accounted for as one document or one
asset. The plan reports total files/bytes, documents per rule and codec, assets
by extension, emitted kinds, ignored-for-mapping metadata keys, mapped/unmapped/
unresolved references, aliases, review requirements and proposed writes.
Unknown metadata is preserved, so "unmapped" never means "discarded".

Zero documents is an error for generic profiles. Users wanting only an archive
use a separate archival workflow; `min_documents: 0` is not accepted. Profiles
may declare a minimum per rule, but exact corpus counts belong in pinned tests,
not a PEP profile that would break when a new PEP is added.

Diagnostics have stable codes, severity, source path, rule, field/source span,
reason and remedy. Required error classes include `invalid_profile`,
`ambiguous_rule`, `parse_error`, `missing_value`, `duplicate_identity`,
`duplicate_alias`, `schema_mismatch`, `unresolved_reference`,
`empty_document_set`, `stale_plan`, `source_update_unsupported` and
`unsupported_file_type`. Invalid record/edge diagnostics retain the original
source location. Reports never suggest success merely because archival bytes
were accepted.

Symlinks, gitlinks and other unsupported Git entries fail before persistence;
they are not followed. Paths must remain inside the selected tree. Parsing has
bounded file, metadata, nesting and total-input limits, recorded in the plan;
profiles cannot raise engine resource limits. Apply takes the store lock, checks
the pinned base, then uses the existing transaction/recovery mechanism. Slow
Git extraction and parsing should happen before that exclusive lock, followed
by complete base and digest revalidation.

## Implementation sequence and acceptance

1. **Profile compiler and codecs.** Add typed configuration with unknown-key
   rejection, canonical hashing, selectors, mapping expressions and the two
   codecs. Keep the Liskov importer separate. Freeze codec normalization and
   header-continuation fixtures before making the profile contract public.
2. **Planner.** Inventory committed Git files, classify and parse, derive stable
   identities, resolve aliases/references, validate the complete graph against
   the selected ontology, then produce the plan and diagnostics. Read-only
   planning must not initialize a store or add receipts.
3. **Atomic apply and ownership.** Introduce the explicit store migration and
   provenance collections, enforce source ownership in every mutation/review
   path, and reuse policy/transaction primitives. Handle retry, proposal
   isolation and schema/base changes as specified above.
4. **Render and qualification.** Export original source subtrees and normalized
   views; include provenance and unresolved assertions. Document all proposed
   commands only once executable. Keep the existing Liskov corpus gate.

| Acceptance case | Required evidence |
| --- | --- |
| Pinned PEP corpus | 744 records and 820 source files; every header preserved; numbered and URL aliases; every declared header reference accounted for; zero original byte/mode mismatches. |
| Simple handbook | Markdown title/ID extraction, path fallback in its own rule, tags/date mapping, and readable bodies without a PEP-specific code path. |
| Unrelated ontology | Map documents into the custom research schema without knowledge-specific kind names; missing fields and forbidden relations fail. |
| Identity | Same source/key survives clone relocation, file rename and profile presentation changes in planner fixtures; changed key changes ID; duplicate key fails. Apply still rejects changed-source snapshots in v1. |
| Multiple sources | Same filenames and short aliases in two sources coexist; qualified lookup works and ambiguous short lookup errors. |
| Metadata edges | Continuations, leading-zero keys, empty/missing/null/false/zero, duplicate headers, reversed replacement edges and unresolved targets are exercised. |
| Failure atomicity | One malformed file among valid files leaves no imported graph, files, membership or successful receipt. Killed transaction recovery is covered with real Dolt. |
| Review and retry | Protected import is invisible on main until accepted; exact retry recovers the same result; edited profile cannot alter a saved plan; concurrent schema/main changes invalidate the plan. |
| Source ownership | Ordinary edit/status/unlink/schema changes cannot rewrite captured payload or remove imported assertions; native notes and local support/reference edges can cite imports under normal endpoint/review rules. |
| Compatibility | Existing legacy store migration and historical reads preserve IDs and receipts; pinned Liskov pristine/edited exports have no additional diagnostics. |
| Honest output | Zero-document import fails; every unmapped/unresolved reference is counted; RST output is identified as literal source; large-context limitations remain explicit. |

The YAML examples were sanity-checked on 2026-10-18 with a disposable Python
interpreter against the pinned PEP Git objects. They produced 744 document
projections, 76 assets, 113 header reference occurrences, 46 unique supersession
edges and 40 unmapped `Requires` occurrences; all header targets resolved. A
two-document handbook fixture, ID stability across a file rename, missing versus
null/false/zero values and malformed headers were also checked. This validates
the proposed mappings against the corpus, not the Rust implementation. The implementation qualification is recorded in
[the release report](import-profiles-qualification.md).

The design does not require incremental synchronization, a general RST renderer,
custom ontology authoring inside profiles, arbitrary expression evaluation,
retrieval profiles, or a plugin execution system. These boundaries keep the
first implementation useful and independently testable.

## Implementation limits and migration

Fixed limits are 1 MiB per profile/metadata block, 64 MiB per file, 1 GiB total,
100,000 files and nesting depth 64. Plans capture these limits and interpreter
contract `scrolls.import.v1:profile1:markdown1:rst1:git-raw1`. Imported record
creation timestamps use the pinned Git commit timestamp; manifest import time
is recorded separately. Candidate identity excludes generated import time and
operation/proposal IDs. Plan files embed the profile and are written atomically
to a fresh destination. Moving the source clone requires a new plan.

Planning reads main at one pinned Dolt revision under a short lock, then releases
it for Git parsing. It does not recover a journal, change branches, initialize a
store or rewrite configuration. Dirty branches or recovery journals fail explicitly.
Apply reparses outside the lock and repeats the main/namespace checks under lock.

Format-4 migration uses a temporary `migration/` Dolt branch and a durable journal.
It validates expected table contents before a guarded fast-forward. Recovery
verifies ownership and expected content, cleans up after a completed fast-forward,
and never resets main. Unexpected state is retained for inspection.
