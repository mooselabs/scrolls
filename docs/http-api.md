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

Responses retain the CLI envelope (`schema_version: 2`, `ok`, `revision`,
`branch`, `data`) and add `store_id` and `schema_revision`. Document results
retain record revisions. `--proposal` selects ordinary reads; main is the
default. Proposal inspection includes the current main and proposal revisions,
merge base, changed/deleted documents, relationship/configuration diffs and
import summary. The returned review token is only usable through existing CLI
review; HTTP cannot accept a proposal.

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
filtering. The graph uses the server-selected branch and schema.

Fabriscope can attach with `--scrolls-url http://127.0.0.1:PORT` or start a private
server with `--scrolls STORE`. It requires discovery capability `graph`, with no
render fallback. Conditional requests still read and validate the backend, so
Fabriscope revalidates at a 30-second interval. No storage migration, installed
service change or documentation/work authority cutover is required.
