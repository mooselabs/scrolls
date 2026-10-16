# Scrolls v1 migration qualification

Date: 2026-10-16. This is local tool qualification, not a documentation authority
cutover. No orchestrator file, Beads record, live operating rule, shared Dolt
binary, or shared Dolt configuration was changed.

## Pinned input and roundtrip

The source was the committed `docs/` tree of the Liskov orchestrator at
`77f9200961d6b344fdf9ae89d3b612271d04a98d`. Import reads Git objects rather than
the working tree. Repository entities and doctrine routes came from the
manifest and AGENTS.md at that same commit.

| Check | Result |
| --- | ---: |
| Tracked Markdown files | 8,199 |
| Other tracked assets | 1,043 |
| Total source paths compared | 9,242 |
| Markdown archives without frontmatter | 223 |
| Pristine Liskov export byte mismatches | 0 |
| Independently regenerated directory indexes | 6 of 6 exact |
| Frozen backlog/question paths compared after an edit | 3,407 unchanged |
| Additional legacy validator diagnostics | 0 |

The manifest produced 34 graph-only repository entities, explicitly listed in
Liskov export manifests.

Original bytes, source Git object IDs, SHA-256 hashes, paths, modes, and
frontmatter metadata are retained in Dolt. Untracked files are excluded.
Legacy IDs and paths resolve; external references and asset references remain
in original metadata and bytes without fabricated graph records. Questions
remain frozen historical records; no answers were inferred from prose.

The Liskov adapter was also exercised by changing the title of imported
`raw/2026-06-22-repo-inventory-readback.md`. Only that document and
`raw/README.md` changed. The six regenerated indexes match the pinned Liskov
renderer, including its literal escape and multiline-scalar behavior.

## Validator baseline

Validation ran in three separate exports of the pinned repository: source,
pristine rendered, and rendered after the representative edit. The immutable
PROOF policy pack was copied only after verifying its pinned content digest.
Both Beads workspace and helper were set to deliberately unavailable local
paths. No live ledger was contacted.

All three validators exited **1 with exactly the same existing diagnostic**:
ADR-0238's `delivered_by` refers to `EPIC-20261015-k7wd`, which is absent from
the committed offline catalogue. Therefore this is a **no-regression pass**,
not a claim that the historical corpus validates cleanly without its ledger.
Frozen-file hashes and generated indexes were checked independently of that
validator result.

The initial Scrolls mechanical lint records 71 historical findings. New
mutations and accepted merges may not introduce structural errors relative to
their current base; fixing an old error does not authorize reintroducing it.

## CLI and persistence qualification

Formatting, warnings-denied Clippy, eleven automated tests (six real-Dolt
integration tests and five unit tests), and the release build pass locally.
The Dolt integration tests cover:

- A failed multi-statement transaction leaves neither the mutation nor receipt.
- A transaction killed **after its insert executes** rolls back; a known
  interrupted temporary review recovers without resetting main.
- Same-operation retries return the original result; changed inputs fail,
  including retries whose receipts live only on proposal branches.
- Concurrent CLI processes serialize; stale revisions fail.
- Protected writes stay off main; agent acceptance is refused.
- Merge conflicts and failed post-merge structural validation preserve main.
- Rejection reasons and proposal history survive.
- Local questions can be answered and accepted by their human owner.
- Imports exclude untracked files, preserve binary assets and untyped archives,
  resolve legacy aliases and supersession, reject changed-source replay, and
  keep imported questions frozen.
- Fresh-output, existing-directory, and symlink checks guard rendering.

The executable demonstration creates evidence, proposes supported knowledge,
reviews and accepts it, retrieves entity context, asks and answers a local
question, inspects history, and renders the resulting native store. It runs
both on an empty store and on the imported corpus. The latter demonstrates
local additions without attempting a legacy authority cutover.

Dolt 2.2.0 is privately installed and checksum-verified, matching the
[Beads backend qualification](https://beads.gascity.com/architecture/dolt).
Its archive SHA-256 on Linux amd64 is
`1f7ad8c2622995789420a3fb0f2d16b4aa7430000a825dd91d5938f36480cbf6`.
The Rust library invokes the CLI directly; there is no server connection.

## Reproduction and artifacts

Use the commands in [README.md](../README.md). `scripts/verify-corpus.py`
creates source/rendered/edited exports, validator logs, a report, and a full
workflow transcript. Its output location must be fresh. `scripts/demo.py`
provides the smaller standalone demonstration. CI runs the Rust gates, real
Dolt tests, release build, and standalone demonstration; the private full
corpus remains an explicit local qualification.

The final corpus report and transcript are in
`.verification/release-qualification/report.json` and
`.verification/release-qualification/demo/transcript.json`.
Local artifacts are under the ignored `.verification/` directory; the release
binary is `target/release/scrolls`. A verified `cargo install --locked --offline`
copy is at `.tools/install/bin/scrolls`. Corpus bytes and local databases are not
committed or uploaded to CI. The installable source includes `Cargo.lock`, the
private Dolt installer, CI, and fish-compatible instructions.

## Boundaries

Actors and human/agent roles are cooperative local attribution, not identity
authentication. Search is deterministic text matching. Context accounts for
serialized item bytes rather than estimating tokens. Reads serialize on the
same store lock as writes. Dolt conflict resolution is at the keyed JSON row
boundary. The full import is a local operation over several hundred megabytes
of SQL and preserved source data; it is not a streaming or remote sync engine.

Registry editing, arbitrary SQL, remote synchronization, embeddings, model
calls, executable doctrine checks, hooks, and web/Slack/MCP interfaces remain
deferred. Imported frozen history cannot be changed. Liskov export refuses
native additions and unsupported graph/metadata changes; native export carries
the complete normalized graph alongside original paths.

## Beads integration qualification — 2026-10-16

The Beads integration uses a private SHA-256-pinned Beads 1.3.0 test binary and
disposable embedded stores with an isolated home directory. It does not connect
to the live orchestrator ledger. `scripts/verify-beads.py` qualifies reviewed
relationships, offline context, explicit refresh, publication preview, human
Beads backlinks, repeat publication, separate Scrolls namespaces, and closed-issue
cleanup after receipt loss. Metadata and work-state preservation are checked.
Rust boundary fixtures additionally exercise timeout, malformed output, partial
failure, identity mismatch, concurrent unrelated metadata, operation-ID conflicts,
and interruption after an external write. CI runs both suites.

Corpus qualification uses orchestrator commit
`77f9200961d6b344fdf9ae89d3b612271d04a98d`:

- 9,242 paths: 8,199 Markdown documents and 1,043 assets; zero byte mismatches.
- All six directory indexes match independently regenerated content.
- 3,407 frozen backlog/question files remain unchanged.
- The representable evidence edit changes only its document and `raw/README.md`.
- Zero additional validator diagnostics. Source, pristine export, and edited
  export all return validator exit 1 with the same pre-existing diagnostic:
  ADR-0238 references `EPIC-20261015-k7wd`, which is absent from the frozen Git
  work records while the live Beads ledger is intentionally disconnected.
  This proves diagnostic parity, not a clean source-validator result.

Reproduce with the verification commands in the README, passing the commit above
to `verify-corpus.py`. The corpus itself and local integration state remain
untracked. Schema-1 upgrades, rejected proposal history, migration replay,
native work-link rendering, and Liskov export rejection have real-Dolt tests.
