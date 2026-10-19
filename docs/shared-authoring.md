# Shared authoring, review and imports development preview

This unreleased unit connects the existing graph engine to the pinned Dolt
2.2.0 SQL session. It is not a migration-ready release. Local operation remains
the default. A shared database must be externally provisioned with a named SQL
account; Scrolls neither starts a server nor provisions accounts. Use loopback
or an explicitly secured private tunnel: this transport does not provide TLS.

Keep connection JSON outside repositories, with restrictive file permissions:

```json
{"host":"127.0.0.1","port":3307,"database":"knowledge","user":"alice","password":"REPLACE_LOCALLY"}
```

`--connection /absolute/machine-local.json` selects shared mode. Numeric IP
addresses are required. Connect timeout is five seconds, lock acquisition ten
seconds, and socket IO thirty seconds. SQL accounts authenticate; `--actor` and
`--role` remain cooperative attribution and policy, not authenticated identity.
Raw SQL access can bypass client rules. Credentials are not printed or stored
in document bindings. No local Dolt executable is needed by shared clients.

## Fresh initialization

The selected database must have no user tables, only `main`, no tags, one initial
Dolt commit and no different commit retained in its reflog. Plan without writing:

```sh
scrolls --connection /absolute/machine-local.json --actor alice init --dry-run --schema examples/handbook.yaml --json
```

Review `data.base_revision` and `data.ontology_digest`, then supply those exact
values when applying the same schema:

```sh
scrolls --connection /absolute/machine-local.json --actor alice --role human --op-id init-knowledge init --schema examples/handbook.yaml --expect-base REVIEWED_BASE --expect-schema REVIEWED_DIGEST --json
```

Omit `--schema` in both commands for the bundled ontology. Apply validates the
empty graph and creates all nine tables, ontology, UUID, shared format 5 and
writer generation 3. It commits a non-ready layout, creates the versioned
`scrolls-operations` recovery branch, then commits the ready marker and receipt.
Only that final commit makes initialization successful. It refuses existing local stores, partial
initialization, additional branches, retained tags, reflog history and non-initial history. There is no reset,
conversion, maintenance migration or repair path. A failed or interrupted
operation must be inspected; unexplained dirty state is preserved and blocks
use. Existing store migration still requires separately qualified exclusive
maintenance and verified backup.

## Direct writes and recovery

```sh
scrolls --connection /absolute/machine-local.json --actor alice --role human --op-id page-1 new page --title Start --body 'Read this first' --json
```

Every non-dry-run shared write requires a stable `--op-id`. One connection holds
a database-wide advisory lock before reading the graph and keeps it through
validation, receipt insertion and durable Dolt commit. Whole-graph invariants
and expected record revisions use the same engine as local writes. Socket loss
never triggers automatic publication retries. A lost acknowledgement reports
an indeterminate commit; reconcile by repeating the same operation ID and input
from either client. The receipt lives in committed database state, not in the
originating client's directory. Input files can live at different client paths,
but their bytes and actor/role must match. Changed input or attribution refuses.
An omitted edit body means keep the body: subsequent edits do not change that
operation's fingerprint. Successful responses identify the authoritative HEAD
at readback; a replay returns the original operation result within that current
snapshot, without applying it again.

Supported direct mutations are `new`, `edit`, `link`, `unlink`, `status` and
`batch`, subject to ontology policy. Protected mutations create isolated proposals. Shared proposal selection,
inspection, human acceptance/rejection and reviewed ontology changes use the
same engine as local mode. Profile-based Git imports are supported. Legacy Markdown import, maintenance
migrations, Beads and consumption-audit commands remain explicitly unavailable. They never fall back
to local state.

Ordinary reads bypass local Beads files and do not append local consumption
logs. Their JSON envelope includes store, schema, Dolt and consumed record
revisions. List/search/show/context report selected records; graph-wide
query/walk/render/lint conservatively report the graph they evaluate. This is
an input declaration, not a persistent shared consumption journal. Cache-backed
reads, scopes, historical revision selection and repository discovery follow in
later units; these reads currently require connectivity and take the lock.

## Review and interrupted-operation recovery

Inspect `scrolls proposals`, then `scrolls review proposal/ID --json`. Acceptance
requires the returned revision-bound token:

```sh
scrolls --connection /absolute/machine-local.json --actor alice --role human --op-id accept-1 review accept proposal/ID --review-token ORIGINAL_TOKEN --json
```

The database-wide lock spans the inspection checks, complete candidate graph
validation and advancement of main. An intent is committed on
`scrolls-operations` before creating or modifying the operation's temporary
branch. That journal commit does not advance main or invalidate the reviewed
main revision. Acceptance merges the inspected proposal onto a dedicated
`review/ID` branch, validates the candidate, commits its receipt, records its
exact commit in the journal, and only then fast-forwards main. A candidate
receipt alone is never acknowledged as publication. Replay verifies that the
recorded candidate is an ancestor of main, including after subsequent writes.

If interrupted, another client can use the durable intent without the original
body file, schema file or client directory:

```sh
scrolls --connection /absolute/machine-local.json recover accept-1 --json
scrolls --connection /absolute/machine-local.json --actor alice --role human recover accept-1 --resume --json
```

Resume uses the positional original operation ID; omit `--op-id`. Attribution
must match the original actor and role. It reuses the original inspected main
and proposal revisions and token. A stale token requires a new human inspection
and a new decision operation; recovery never refreshes it automatically.
Direct writes without a journaled intent still reconcile by repeating their
original command and operation ID.

An owner can explicitly abandon an unpublished intent:

```sh
scrolls --connection /absolute/machine-local.json --actor alice --role human --op-id cancel-1 recover accept-1 --abandon --reason 'Reinspect against changed main' --json
```

Abandonment has its own durable receipt and can itself resume with
`recover cancel-1 --resume`. It cannot undo a published operation. Cleanup only
resets the exact owned, uncommitted temporary state: all table schemas, working
and staged row hashes, merge source/target and conflict state are checked.
The full versioned inventory also rejects hidden Dolt system-table changes
relative to the pristine initialization root, including unexpected triggers.
Unknown state is preserved. Clean committed candidate branches remain as
evidence. Pending, failed and abandoned operations reserve their IDs; changing
input never reuses one. Branches and recovery history are retained; automatic
pruning is not implemented.

Writer generation 2 clients refuse generation 3 stores. Generation 3 refuses
older shared stores; upgrading them requires a future exclusive maintenance
procedure with verified backup. These preview stores are disposable.

## Captured-source imports

The existing profile engine supports shared `import profiles validate`,
`import plan git` and `import apply`. Bundled profile list/show commands need
no store connection. Planning reads shared state and writes only its requested
new plan file; it requires no publication operation ID. For example, as a human:

```sh
scrolls --connection /absolute/machine-local.json --actor alice --role human import plan git /path/to/source --rev FULL_GIT_COMMIT --root . --source handbook --profile handbook --out /tmp/handbook.plan.json --json
scrolls --connection /absolute/machine-local.json --actor alice --role human --op-id handbook-1 import apply /tmp/handbook.plan.json --dry-run --json
scrolls --connection /absolute/machine-local.json --actor alice --role human --op-id handbook-1 import apply /tmp/handbook.plan.json --json
```

Agents use `--role agent`; apply must retain the plan's actor and role. Store
UUID, main/schema revisions, profile and exact candidate digest remain bound
by the plan. One SQL connection and lock cover the whole shared operation.
Git reads ignore replacement refs and inherited `GIT_*` redirection, preserving
the recorded commit/tree/blob identities. Source bytes and modes are retained
unchanged. Captured records remain source-owned; editable adoption is not
implemented by this unit.

Direct imports commit records, relationships, source files, provenance and the
receipt atomically. Protected imports use the durable proposal protocol above.
Prepared intents reserve namespaces even before a proposal branch exists.
Another client can resume creation without the originating checkout, plan or
profile file using `recover ORIGINAL_OP --resume`; it then inspects the proposal
to obtain a review token. Interrupted acceptance uses the already-issued token.
Explicit abandonment releases an unpublished namespace reservation but never
makes its original operation ID reusable with changed input.

Completed receipt replay precedes stale-base and source-object checks: a copied
plan can recover the original result after later main writes and deletion of
the source checkout. Verified identical-source imports may record a no-op
receipt under a new operation ID. Changed snapshots in an accepted or pending
namespace remain refused; there is no automatic import reconciliation.

Source limits remain 64 MiB per file, 1 GiB total and 100,000 files. Shared
publication also has an encoded SQL text budget: the lesser of the server's
`max_allowed_packet` and 1 GiB, minus the command byte. JSON/base64/SQL encoding
means that budget is not equal to source-file bytes. `doctor` reports it as
`shared_query_bytes`. Oversized requests fail with `input_limit` and roll back;
clients never truncate files or change server settings automatically. Split
sources into independent namespaces or qualify an appropriate server setting.
Dolt 2.2.0's tested default yields 1,073,741,823 SQL text bytes. Shared reads
currently take the same lock; long imports can cause other clients' bounded
lock acquisition to time out until snapshot caches are implemented.

## Qualification

The repository-owned CI runs the same complete Rust, demo, Beads and pinned-PEP
import checks as local mode. `tests/shared_session.rs` starts disposable Dolt
2.2.0 services and independent CLI processes. It exercises contention, session
branches, transaction isolation, graph-cycle and stale-edit races, committed
receipts, cross-client replay, ignored corrupt/colliding local Beads state,
initialization preconditions, protected proposals/unsupported refusal, disconnect,
process death, lost acknowledgements, restart and portable input paths. The
bundled, handbook and research configurations run without external services.

The old-writer fixture requires an actual clean checkout at
`3ae0a38dcc4da0a508e82ce4737d226e06d6f2b4` in
`.verification/scrolls-0.3.0`. CI checks it out explicitly. For local qualification:

```sh
git clone https://github.com/mooselabs/scrolls.git .verification/scrolls-0.3.0
git -C .verification/scrolls-0.3.0 checkout --detach 3ae0a38dcc4da0a508e82ce4737d226e06d6f2b4
cargo test --locked --all-targets
```

The test builds that source into a separate temporary Cargo target, stops the
disposable server, removes any stale server marker, and runs the old writer. It
requires the actual `unsupported store schema` fence and an unchanged clean
Dolt database, rather than accepting a server-marker refusal. No tests skip
when the pinned binaries or baseline source are absent.

Shared review qualification also covers proposal/rejection replay, lost branch,
merge, candidate-commit and main-advance acknowledgements, stale main/proposal
tokens, concurrent acceptance, cycle-producing and conflicting merges, staged-only foreign data, hidden trigger state,
interrupted abandonment and reviewed handbook/research configuration. The
previous shared writer is built from an additional pinned clean checkout:

```sh
git clone https://github.com/mooselabs/scrolls.git .verification/scrolls-shared-previous
git -C .verification/scrolls-shared-previous checkout --detach a4bf897811abbb19657a824b5eb3470a2733ce16
```

Both baseline checkouts must exist before running the full test command above.

The pinned-PEP verification script now runs both local and disposable shared
Dolt stores using the same binary snapshot, with a named fixture SQL account
and a second client directory. It compares all 820 paths, bytes and modes and
744 documents across both stores, records timings and the transport budget,
and shuts down its owned server. It never accepts a live endpoint. Reference
coverage remains declared metadata only, not body links or RST roles.

Verified database-only backup creation and disposable restoration are available
through `backup create` and `backup restore`. See [shared backups](shared-backups.md)
for host attestation, proof contents, refusal behavior and the separate account
and service-configuration restoration requirement.

Machine-local connection JSON optionally accepts `io_timeout_seconds` (11–600,
default 30). This bounds each socket read/write and must exceed the fixed
10-second publication-lock acquisition budget. Bulk qualification explicitly
uses 300 seconds: the PEP transaction can exceed 30 seconds on a busy host.
A timeout is not success; preserve the original operation ID and reconcile its
receipt before retrying. The setting does not change locks, byte limits,
transaction atomicity, stale-revision checks or review tokens.
