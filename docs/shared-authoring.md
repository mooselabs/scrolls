# Direct shared authoring development preview

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

The selected database must have no user tables, only `main`, and one initial
Dolt commit. Plan without writing:

```sh
scrolls --connection /absolute/machine-local.json --actor alice init --dry-run --schema examples/handbook.yaml --json
```

Review `data.base_revision` and `data.ontology_digest`, then supply those exact
values when applying the same schema:

```sh
scrolls --connection /absolute/machine-local.json --actor alice --role human --op-id init-knowledge init --schema examples/handbook.yaml --expect-base REVIEWED_BASE --expect-schema REVIEWED_DIGEST --json
```

Omit `--schema` in both commands for the bundled ontology. Apply validates the
empty graph and commits all nine tables, ontology, UUID, shared format 5,
writer generation 1 and its receipt. It refuses existing local stores, partial
initialization, additional branches and non-initial history. There is no reset,
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
`batch`, subject to ontology policy. A protected mutation fails before writing;
it does not become a direct publication. Shared proposal selection, creation,
review, schema apply, imports, migrations, Beads and consumption-audit commands
are explicitly unavailable in this unit. They never fall back to local state.

Ordinary reads bypass local Beads files and do not append local consumption
logs. Their JSON envelope includes store, schema, Dolt and consumed record
revisions. List/search/show/context report selected records; graph-wide
query/walk/render/lint conservatively report the graph they evaluate. This is
an input declaration, not a persistent shared consumption journal. Cache-backed
reads, scopes, historical revision selection and repository discovery follow in
later units; these reads currently require connectivity and take the lock.

## Qualification

The repository-owned CI runs the same complete Rust, demo, Beads and pinned-PEP
import checks as local mode. `tests/shared_session.rs` starts disposable Dolt
2.2.0 services and independent CLI processes. It exercises contention, session
branches, transaction isolation, graph-cycle and stale-edit races, committed
receipts, cross-client replay, ignored corrupt/colliding local Beads state,
initialization preconditions, protected/unsupported refusal, disconnect,
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
