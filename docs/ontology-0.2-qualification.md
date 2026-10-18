# Scrolls 0.2.0 qualification

Implementation is based on verified main `2af8f0565661b811a2a5c9b9854ef94d53268f4b`.
The final version is 0.2.0, store format 3, ontology format 1, CLI JSON envelope 2;
external Beads backlink payloads remain version 1. No live ledger migration or
documentation-authority cutover was performed. The shared checkout's unrelated
`docs/what-is-scrolls.md` was preserved.

## Local checks

Formatting and warnings-denied Clippy pass. All 35 Rust tests pass: 15 Dolt
integration tests, 13 existing/updated unit tests, five ontology regressions and
one Beads integration test plus a 3,900-case relation-policy comparison against
the verified 2af8f05 implementation. The release build includes all targets. The updated
demonstration passes against the release binary.

The handbook and research schemas create, validate, review, query, traverse,
prime and export records without bundled knowledge kind names. Tests also cover
adding a kind in a schema proposal and authoring that kind before acceptance,
configuration/data revalidation after concurrent main changes, stable pins and
byte accounting, glob failures, typed fields, null defaults, cycles, direction,
ambiguity and offline references.

Ownership regressions cover owner changes in the same batch, relinking before
acceptance, swapping already accepted answers, unauthorized direct changes and
unauthorized review acceptance. Batch tests cover rollback, initial revision
checks, once-only revision increments, content-aware retries and long operation
identifiers. Existing interrupted-write and review-recovery tests remain active.

Format 2 migration preserves identity, records, receipts and readable historical
branches. Imported debt is translated from preserved original records and import
edges, rather than current potentially invalid data; new violations still fail.

## Beads

Disposable qualification against checksum-pinned Beads 1.3.0 passes: review
isolation, offline references, explicit refresh, publication preview, human
backlinks, idempotent publication, independent store namespaces, closed-issue
cleanup after receipt loss, and preservation of unrelated metadata and work state.

## Reproducible evidence

The isolated worktree retains logs and disposable stores under
`.verification/ontology-020/`. The final Rust test log is `tests-accepted.log`;
release build, demo and Beads logs use the `*-accepted.log` suffix.


The pinned corpus is orchestrator commit
`77f9200961d6b344fdf9ae89d3b612271d04a98d`, using its checksum-verified policy pack.
The independent source validator has one existing ADR-0238 `delivered_by`
diagnostic and intentionally cannot reach the live Beads ledger. Qualification
requires identical diagnostics on source and all exports; it does not claim that
the source validator is clean.

Final candidate binary SHA-256: `cb7f2cbbd01cb667b73a5eac96930e51526258e952de27862a1cb28e80cbc86b`.

## Pinned corpus result

The final candidate preserves all 9,242 source paths byte-for-byte (8,199 Markdown
files and 1,043 assets), authors all ten bundled kinds plus three supporting
records, and verifies 3,407 frozen backlog/question files. All six generated
indexes match the pinned independent renderer. Source, pristine export, edited
export and native additions produce identical validator diagnostics and exit
codes `[1, 1, 1, 1]`: zero additional diagnostics. The executable demonstration
also runs against the imported store.

The complete report and its binary digest are retained at
`.verification/ontology-020/corpus-accepted/report.json`; associated source,
rendered, edited and native validator logs are in the same directory.

## Integration with revision-bound review (2026-10-18)

Integrated `b6215fe` with main `e4faa65`, retaining the README onboarding layout,
revision-bound acceptance, configuration diffs and historical receipt fingerprints.
The integrated tree passes formatting, warnings-denied Clippy, all 40 Rust/Dolt
tests, the release build, generic demo and disposable Beads 1.3.0 qualification.
The complete pinned corpus qualification at `d2093e8` passes: all 9,242 paths
are byte-identical, all six indexes match, 3,407 frozen files remain preserved,
and 13 native records plus the imported-store demo succeed. Source, pristine,
edited and native validators have identical exit codes `[1, 1, 1, 1]` and zero
additional diagnostics. That binary has SHA-256
`e8c88b670e1c800f0795ff10b6e97b710850db8193c880b1c4b26f702cbf325b`.
CI for that checkpoint passed in run `39147421136`.

The writer-fence follow-up checks the format again after selecting a branch.
A historical checkout can no longer let a 0.2 writer switch to newer-format main
and mutate it. All 41 Rust/Dolt tests, formatting, warnings-denied Clippy and
the all-target release build pass with this check. The repeated qualification
reports for this final binary are retained under `.verification/integrated-020/`;
the corpus report records the tested commit, binary hash and exact diagnostics.
