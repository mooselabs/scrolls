# Read-only loopback HTTP API

Start an existing store (fish-compatible examples):

```fish
scrolls --store ./knowledge --actor alice serve --bind 127.0.0.1:0 --json
scrolls --connection /absolute/machine-local.json --actor alice serve --bind '[::1]:0' --json
scrolls --store ./knowledge --proposal proposal/BRANCH serve --bind 127.0.0.1:8080
```

The server validates the store before listening, then prints the actual address
once. JSON startup output is one record with `event: "listening"`, `address`,
`api_version: "v0"`, `read_only: true`, `schema_version: 2` and `ok: true`.
Only numeric loopback addresses are accepted; port zero selects a free port.
Use a tunnel for remote access. No authentication system is added. Actor names
are query bindings and cooperative attribution, not identities or permissions.
SIGINT, SIGTERM and SIGHUP stop admission and drain in-flight requests.

Use the printed address in these examples:

```fish
set endpoint http://127.0.0.1:8080
curl "$endpoint/v0/context"
curl "$endpoint/v0/graph"
curl "$endpoint/v0/records?kind=evidence&limit=50"
curl "$endpoint/v0/records/ALIAS?provenance=true"
curl "$endpoint/v0/records/ID/context?budget=16000"
curl "$endpoint/v0/records/ID/walk?direction=outgoing&depth=2&full=true"
curl -H 'Content-Type: application/json' --data '{"expression":{"op":"compare","field":"owner","cmp":"eq","value":"$actor"},"actor":"alice","paths":["src/app.rs"],"at":"2026-10-19T00:00:00Z"}' "$endpoint/v0/query"
```

The checked-in [OpenAPI contract](openapi.json), also available at
`GET /v0/openapi.json`, defines every route, parameter and response. The API
shares typed knowledge operations with the CLI, including the configured ontology,
text/field filters, aliases and native IDs, immediate references, edges, cached
related-work references, provenance, traversal and query predicates.
`/v0/schema`, `/v0/prime`, `/v0/proposals`, `/v0/proposals/{id}` and `/v0/history`
complete the discovery and inspection surface. Proposal IDs accept either the
suffix or `proposal/NAME`. Percent-encode IDs/aliases in paths. There is no
mutation, acceptance, file-export, Beads refresh/publication or SQL endpoint.

HTTP uses envelope version 3: `schema_version: 3`, `ok`, `store_id`, `view`,
`store_revision`, `schema_revision` and `data`. `view` is `main` or
`proposal/NAME`; discovery repeats the same `view`. `store_revision` is an
opaque, non-empty string with no prescribed length or hash format. Errors also
use `schema_version: 3`. The CLI keeps its version 2 envelope with `branch` and
`revision`, including the `serve --json` listening startup record (which has
no selection fields). Document results retain their integer record revisions.
`--proposal` selects ordinary reads; main is the default. Proposal inspection includes the current main and proposal revisions,
`base_revision`, changed/deleted documents, typed relationship/configuration changes
and import summary. `base_revision` is the common ancestor used for record inspection
on Dolt and the recorded proposal base on a change-set backend. The proposal collection
adds `store_revision`, the immutable revision of each proposal view.
`graph_changes` and `configuration_changes` contain rows with `table`, `id`,
`before` and `after`. Values are decoded JSON; a null side denotes insertion or deletion.
Graph rows use table `edges`; configuration rows use `metadata` (including ontology).
Rows are sorted by id. Both local and shared Dolt compare main with the proposal head;
a change-set backend lists its proposal's own changes (ADR-0280 decision 5). The returned review token is only usable through existing CLI
review; HTTP cannot accept a proposal.

`/v0/history` without `id` returns typed log entries: `store_revision`, `actor`,
`time` (UTC RFC3339) and `message`. With `id`, it returns typed record-history
entries with `record` in place of `message`: the full record snapshot at that
revision. Neither endpoint exposes backend system-table columns or serialized
record payloads. The CLI `history [ID]` uses these same entries.

`/v0/records` and `/v0/history` return `data.items` and `data.next_cursor`.
The page size defaults to 50, with a maximum of 200 (minimum 1). Repeat the same
filters and limit with `cursor=NEXT_CURSOR`. Cursors bind the request, store,
branch, immutable snapshot and last item. A changed snapshot returns
`409 snapshot_changed`; restart pagination. A reused cursor with changed filters
is malformed. Records keep CLI ID ordering; history keeps chronological order.
`q` searches title/body with case-insensitive AND terms. Repeat `field=KEY=JSON`
for multiple filters; URL-encode values. `owner=me` binds the server actor.

Context keeps the CLI's hard document budget: insufficient space returns 400.
Prime keeps its soft budget and always includes the guide, instructions and pins,
with overrun reporting. Walk defaults to both directions, depth 2, references;
maximum depth is 100. Queries accept the current expression language as JSON
values with optional actor, paths and RFC3339 evaluation time; they never read
server-local filenames. Query bodies are limited to 65,536 bytes.

Successful GETs have strong SHA-256 ETags covering the complete representation,
including proposal revisions when relevant. `If-None-Match` can return 304 only
after a successful current backend read. All responses set
`Cache-Control: private, no-cache` and `X-Content-Type-Options: nosniff`.
No stale response is served during backend failure. Query responses are not
conditionally cached. `/v0/openapi.json` returns the raw specification.

`/healthz` reports process liveness and stays healthy during backend failure.
`/readyz` performs a real store read, including format/state/ontology checks;
connectivity loss returns 503. Store-state conflicts return 409. Missing,
incompatible, dirty or interrupted stores fail startup without initialization,
migration, journal repair or branch checkout. HTTP never appends consumption
logs, creates receipts, modifies configuration or publishes application rows.

Each operation captures its immutable graph/schema/provenance revisions under
the existing local lock or shared publication lock, then releases the session
before sending the response. Blocking work runs off the async executor. One
operation is admitted per server; admission and local-lock waits each return
`503 busy` after five seconds. Shared connection, lock and IO budgets retain
the [existing connection settings](shared-authoring.md). No lock remains between
requests, persistent cache or background refresh loop is introduced.

Requests carrying `Origin` are rejected and no CORS headers are sent. `Host`
must use `localhost`, canonical IPv4/IPv6 loopback, or the bound loopback address,
at the actual listener port. Unknown query parameters and duplicate scalar
parameters fail. Unsupported methods return 405. Errors use
`error.code` and a sanitized `error.message`: malformed reads 400, missing
records/proposals 404, snapshot/state conflicts 409, oversized bodies 413,
backend/admission failures 503 and unexpected failures 500. Request logs contain
only method, route template, status and duration, without IDs, bodies or credentials.

`GET /v0/graph` (discovery operation `graph`) returns a complete snapshot in the
same envelope. `data.records` is keyed by record ID; `data.edges` retains the
existing graph keys and every relationship, including external Beads targets.
Records include bodies, fields, paths, aliases and revisions; `original` is
omitted. Import-original payloads, import metadata and source-file bytes are
not included. Empty stores return two empty objects. There is no pagination or
filtering. The graph uses the server-selected view and schema.

Fabriscope's envelope-version-2 client refuses discovery against this build
until its version-3 update (ADR-0280 D27, BKLG-20261111-21om). That update maps
`branch` to `view` and `revision` to `store_revision`, and accepts HTTP
`schema_version: 3`; the CLI listening record remains at version 2.
Clients can attach with `--scrolls-url http://127.0.0.1:PORT` or start a private
server with `--scrolls STORE`. Graph discovery and conditional reads remain
the same; no store migration or documentation/work authority cutover is required.

## Backend conformance

Hosts implement `scrolls::read::SnapshotSource` and mount the public
`scrolls::http::router(source, loopback_address)`. Each source opens one immutable
`read::Snapshot` per request, owning a `KnowledgeStore` handle and its graph,
store id, selected view, revision, actor, routing and offline work adapter.
Keep any lock/session alive in that handle until the snapshot drops. Classify
backend errors as `read::BackendFailure::{Unavailable, Busy, Conflict}`; the
router owns stable HTTP status codes, sanitized messages and error envelopes.
The standalone adapter is `storage::http::DoltSource`.

`scrolls::conformance::seed(&store)` commits the fixed fixture through
`KnowledgeStore::commit` into a fresh initialized backend. Expose a
`proposal/conformance` view at the seeded revision as well as `main`; the
fixture includes its stored proposal metadata. Alternatively provision the same
`conformance::fixture()` through the backend's own fixture loader. Sources use
actor `reader`, the seeded routing and an empty offline work cache.
Run `conformance::check(source).await` inside a Tokio runtime. It exercises
every `/v0` route and `/readyz` through the public router and checks the exact
OpenAPI-required envelope, revision opacity, discovery selection, OpenAPI bytes,
security, ETags and errors. Compare its returned `Report` across backends to
check row-read parity; store-derived document URIs are normalized. History and
proposal payload internals retain their backend representation.
`cargo test --locked --test conformance` compares local Dolt, shared Dolt and
an in-memory `KnowledgeStore` that starts no processes.
