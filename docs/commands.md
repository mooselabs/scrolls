# Scrolls 0.3 command reference

Global flags: `--connection FILE`, `--store DIR`, `--dolt FILE`, `--json`, `--actor NAME`,
`--role human|agent`, `--proposal BRANCH`, `--op-id ID`, `--dry-run`, `--session ID`.
Writes need an actor. The JSON envelope has `schema_version: 2`, `ok`, Dolt
`revision`, `branch`, and `data` (or `error`). Beads backlink payloads remain v1.

| Command | Contract |
|---|---|
| `serve [--bind 127.0.0.1:0]` | Read-only loopback HTTP; local or shared snapshot reads, JSON startup address. See [HTTP API](http-api.md). |
| `init [--schema FILE]` | Create a format 4 store using a supplied or bundled ontology. |
| `schema show` | Active typed configuration and schema revision. |
| `schema export` | YAML on stdout, suitable for editing (also with `--json`). |
| `schema validate FILE` | Check format and compatibility against the selected graph. |
| `schema apply FILE --expect REV` | Propose configuration; always requires human review. |
| `new KIND --title TEXT [--body TEXT] [--owner NAME] [--field KEY=JSON]` | Create using configured defaults and policy. |
| `edit ID --expect N` | Edit title, body, owner, fields or optional domain metadata. |
| `status ID STATE --expect N` | Configured transition; cardinality and guards apply. |
| `link FROM REL TO --expect N [--expect-to N] [--why TEXT]` | Add an assertion. Both-endpoint relations require both existing revisions. |
| `unlink FROM REL TO --expect N [--expect-to N]` | Remove an assertion under the same ownership rules. |
| `batch FILE` | Atomic ordered YAML/JSON generic mutations. |
| `proposals` | List pending, accepted and rejected proposals. |
| `review BRANCH` | Record, edge and configuration differences. |
| `review accept BRANCH --review-token TOKEN` | Human review, merged-data revalidation and guarded acceptance. |
| `review reject BRANCH --reason TEXT` | Human rejection with reason. |
| `prime [--budget N]` | Complete schema guide/instructions/pins under a soft byte budget. |
| `show ID` | Full record and immediate references. |
| `context ID [--budget N]` | Document first, immediate references, hard byte budget. |
| `context --bead REF [--budget N]` | Cached Beads metadata and direct document references. |
| `list [--kind K] [--status S] [--owner NAME] [--field KEY=JSON]` | Deterministic filtered records; `--owner me` binds the actor. |
| `search TERMS [filters] [--limit N]` | Same filters; case-insensitive AND over title/body. Includes notes. |
| `query --file FILE [--path PATH] [--at RFC3339]` | Structured selection with explicit bindings. |
| `walk ID [--rel R] [--direction incoming\|outgoing\|both] [--depth N] [--full]` | References and traversed edges; default depth 2, maximum 100. |
| `history [ID]` | Dolt history. |
| `lint` | Structural errors and retained imported debt; nonzero on new errors. |
| `consumed [--for-session ID]` | Audited deliveries and record revisions. |
| `doctor` / `migrate` | Inspect / explicitly upgrade a store. |
| `import markdown DIR --rev COMMIT` | Pinned Git Liskov adapter; requires bundled knowledge ontology. |
| `render --out DIR [--format native\|liskov]` | Fresh destination export with complete graph and configuration. |

Bodies accept `--body`, `--body-file FILE` (`-` for stdin), or `--editor`.
Optional domain metadata uses `--confidence`, `--review-every`, and on edit,
`--last-reviewed YYYY-MM-DD`. Custom schemas can omit its validation/defaults.
Expected revisions always refer to records, not the enclosing Dolt revision.
Retries use the same `--op-id`, actor, role, arguments and file contents.

## Batch format

A top-level ordered list; unknown operation keys fail. New handles omit expected
revisions. Every affected existing assertion owner supplies its initial revision.
Source-only references do not require target revisions; an explicit one is checked.

```yaml
- op: new
  handle: response
  kind: answer
  title: Proposed answer
  body: Yes, based on the measurement.
- op: link
  from: $response
  rel: answers
  to: QUESTION_ID
  expect_to: 1
```

Accept existing records together (by the question owner, or propose as an agent):

```yaml
- {op: status, id: ANSWER_ID, state: accepted, expect: 1}
- {op: status, id: QUESTION_ID, state: answered, expect: 2}
```

For supersession, batch the old document's configured terminal transition with
`link NEW supersedes OLD`, providing both existing revisions and `why`.
A batch cannot authorize itself by transferring ownership or relinking. An agent
can propose a guarded change; the human reviewer must satisfy the original guard.

## Schema format

See the complete [bundled schema](../schemas/knowledge.yaml),
[handbook](../examples/handbook.yaml), and [research](../examples/research.yaml).
`format: 1` is required. Each kind defines `initial`, `states`, `transitions`,
`write: {human: direct|proposal, agent: direct|proposal}`, `fields`,
`additional_fields`, optional `guards` and `cardinality`.

Fields define `type`, `required`, `default`, `values` (enum), `items` (list),
`minimum` (number), `nonempty` (string), and `glob` (string/list of globs).
Relations define `from`/`to` kind lists (`'*'` means any), `external`
(`beads`, `glob`, `url`, `text`), `affects: source|both`, `reason`, `self_link`,
`cycles`, and `same_kind`. An empty endpoint list permits no local kind.
Cardinality defines `name`, `relation`, `direction`, `min`, `max`, optional
`status`/`related_status`, and `skip_frozen`. A guard defines target state `to`,
`human`, and optionally `relation`/`direction`; absent a relation it uses the
record owner. Configured `body_links` maps `local`/`beads` to source-only
relations for automatic Markdown link assertions. Custom schemas may omit it.
`envelope_fields` optionally constrains confidence, review_every and last_reviewed.
`instructions` is authored text and `pins` is an ordered list of IDs or aliases,
resolved and deduplicated on application.

## Queries

Files contain one expression. `all`/`any` take `args`, `not` takes `arg`.
`compare` uses `field`, `cmp: eq|ne|lt|le|gt|ge`, and a typed JSON/YAML `value`.
`exists` takes `field`. Custom fields use `fields.NAME`. `$actor` and `$now`
are explicit bound values. `relation` supports `direction`, `min`, `max` and
an optional `where` expression evaluated on the neighboring record.
`path_glob` matches a field against supplied `--path` values. `applicability`
combines configured globs, external glob links and imported routing, preserving
unresolved scope clauses as diagnostics. `date_plus` adds integer days (or
`{field: review_every}`) to a date/timestamp, then compares to a date or `$now`.
Examples replacing removed read commands are in [queries](../examples/queries/).

## Beads references

`beads bind NAME --directory DIR [--bd FILE] [--timeout SECONDS]`, `beads list`,
`beads refs REF`, `beads refresh REF|--all`, `beads publish REF|--all`.
References are qualified `beads://STORE_UUID/ISSUE_ID` or locally bound `NAME:ID`.
Ordinary reads never contact Beads. Refresh and publication are explicit;
publication requires main and preserves unrelated metadata. The external backlink
payload version is unchanged. Local aliases, caches and publication receipts
remain separate from document authority. No live ledger is migrated by this release.

Acceptance requires the `review_token` returned by `review BRANCH`. It binds both
main and proposal revisions; any intervening write requires a fresh inspection.

## Configurable imports (0.3)

- `import profiles list` and `import profiles show NAME` need no store.
- `import profiles validate FILE` checks strict YAML and the active ontology.
- `import plan git REPO --root PATH --rev REV --source SOURCE_ID --profile NAME --out PLAN`
  captures a read-only main snapshot; use `--profile-file FILE` instead of `--profile`.
- `import apply PLAN --op-id ID` revalidates all bindings and commits one complete
  source or routes it to one proposal. `--dry-run` writes no receipt or graph data.
- `show ID --provenance` includes captured metadata, spans, assertions and source.

Plans bind the store UUID, main/schema revisions, actor/role, Git commit/tree/root,
embedded profile, interpreter and candidate digests. Exact receipt retries are
checked before stale-plan errors. Rejected operations remain rejected; a new
operation ID can propose again. Changed accepted sources return
`source_update_unsupported`. Review tokens remain required for acceptance.

Native export includes `sources/SOURCE_ID/`, `scrolls/imports.json`, linked native
views and manifest provenance. Review displays file paths, hashes and modes.
Captured properties and imported edges cannot be changed by authoring, batches,
schema changes or review. New local relationships still follow endpoint revision,
ownership and proposal rules. See [the complete contract](import-profiles.md).
