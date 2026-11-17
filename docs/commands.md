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
| `import plan markdown DIR --rev COMMIT --git-web-url URL --out PLAN` | Human-only shared editable bootstrap planning; see [contract](shared-markdown.md). |
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
An explicit `format` is required: use `format: 2` for new ontologies.
`format: 1` remains supported with its original meaning; other formats are refused. Each kind defines `initial`, `states`, `transitions`,
`write: {human: direct|proposal, agent: direct|proposal}`, `fields`,
`additional_fields`, optional `guards` and `cardinality`.

Fields define `type`, `required`, `default`, `values` (enum), `items` (list),
`minimum` (number), `nonempty` (string), and `glob` (string/list of globs).
Relations define `from`/`to` kind lists (`'*'` means any), `external`
(`work`, `glob`, `url`, `text` in format 2), `affects: source|both`, `reason`, `self_link`,
`cycles`, and `same_kind`. An empty endpoint list permits no local kind.
Cardinality defines `name`, `relation`, `direction`, `min`, `max`, optional
`status`/`related_status`, and `skip_frozen`. A guard defines target state `to`,
`human`, and optionally `relation`/`direction`; absent a relation it uses the
record owner. Configured `body_links` maps `local`/`work` to source-only
relations for automatic Markdown link assertions. Custom schemas may omit it.
The `work` target accepts canonical `beads://PROJECT-UUID/FULL-ISSUE-ID` references
and resolves locally bound `NAME:ID` on link/unlink. Body `](beads://…)` links use
`body_links.work`. In format 1, both the external type and body-link key are named
`beads` instead of `work`. Each format rejects the other's external type; format 2
also rejects unknown body-link keys. Existing stored
references and format-1 schema revisions are unchanged. See the
[ontology format-2 upgrade guide](ontology-format-2.md).
Format 2 also accepts top-level `invariants`, a list of named requirements.
Each entry has `name`, `description`, `require` (a query predicate), and a
selection: `kinds` (nonempty kind list), `when` (query predicate), or both.
With both, the record must match both. `skip_frozen: true` excludes frozen
records; the default is false. Names use letters, numbers, underscores or
hyphens, must be unique, and must not match any cardinality name.

For example, add this section to the handbook ontology to require every
published public page to have an outgoing chapter pointing at another published page:

```yaml
invariants:
  - name: published-chapter
    description: Published public pages link to a published chapter
    kinds: [page]
    when:
      op: all
      args:
        - {op: compare, field: status, cmp: eq, value: published}
        - {op: compare, field: fields.audience, cmp: eq, value: public}
    require:
      op: relation
      relation: chapter
      direction: outgoing
      min: 1
      where: {op: compare, field: status, cmp: eq, value: published}
    skip_frozen: true
```

Predicates use the query language below, including nested `all`, `any`, `not`,
`exists`, `compare`, `relation` and `date_plus`. Invariants reject `$now`,
`$actor`, `path_glob` and `applicability` anywhere in their predicates; they
have no clock, actor, routing or path input. No other operator is excluded.
Kind selectors (including literal `kind` comparisons) and relations must be
configured. A predicate evaluation error counts as a violation.

Lint reports each failed requirement as `RECORD_ID:INVARIANT_NAME`. Writes
that introduce a violation are refused with `graph_write_refused`, including
schema changes. Existing violations are tolerated only for the same record
and an unchanged invariant definition. `schema validate FILE` previews the same
check without writing. An absent or empty list is omitted when storing the
ontology, preserving existing schema revisions.

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

## Graph write refusals

`KnowledgeStore` enforces the engine's full-graph check inside each atomic
write. This covers direct mutations (`new`, `edit`, `link`, `unlink`, `status`,
`batch`, `schema apply`), proposal writes and publication, local/shared acceptance,
and qualified `import source` writes. The before graph belongs to the destination
branch; acceptance checks against current main. Commands may also check plans and
dry runs before attempting a store write.

A new violation is refused without advancing branch heads. An existing violation
is tolerated only for the same rule and record; changing a rule cannot hide behind
old debt. The check includes frozen-history immutability, record fields,
cardinality, relation endpoints, body links, cycles, ontology validity and pins.
Authority, review tokens, guards and importer health remain additional checks.

With `--json`, a graph refusal returns a nonzero exit and this diagnostic shape:

```json
{"schema_version":2,"ok":false,"error":{"message":"structural validation failed: sc-example:missing-support","diagnostic":{"code":"graph_write_refused","severity":"error","violations":["sc-example:missing-support"]}}}
```

`violations` lists the lint identities (`RECORD:RULE`,
`RECORD:body-link:TARGET`, `RECORD:RELATION-cycle`, `edge:KEY:REASON`). Ontology
errors use `ontology:schema:REASON` and unresolved pins use `ontology:pin:PIN`.
Callers can use `code` and `violations` without parsing `message`.

The initial legacy import and editable Markdown bootstrap also use checked store
writes. Only these empty-store bootstrap writes may record the imported graph's
lint violations as `lint_debt` in that same write. Receipt-only replays, rejects
and identical retries need no graph check. Existing-store `migrate` maintenance
keeps its separate format and `validation_debt` behavior.
