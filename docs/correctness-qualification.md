# Scrolls correctness qualification

Qualified on 2026-10-16. Tested implementation commit:
`f23b0f251ce933ed0626076329b18d3f3915062b` (tree `db2b81fd79e43fd29ba7bb6bc60554afdf75592f`).
The worktree was clean throughout the committed corpus qualification, and the
runner checked that the release binary did not change during the run.

Release binary SHA-256: `cda5f4e9cecf26fc731c2474186182264f57d5cd11ae696259e22c9f1058156a`.
This is a local correctness and export qualification. This work did not change
Liskov's documentation authority, the live work ledger or tracked files in the
shared checkout. The previously observed empty untracked `docs/what-is-scrolls.md`
was restored after it was found missing during concurrent work; no existing file was overwritten.
The separate Beads integration worktree was not edited.

The tested tree merges concurrent main commit `6d132e6` (Beads integration).
Both implementations are retained: Beads native navigation follows the stable
render manifest, and Liskov supplemental records and graph retain work references.
All integrated checks use disposable local Beads databases, never the live ledger.

## Behavior and regression coverage

- `prime` delivers matched doctrine once, in deterministic order and in full,
  even beyond its budget. Byte accounting includes array brackets and separators;
  original snapshots are excluded from projections. Optional records are omitted
  when mandatory doctrine exhausts the budget; `context` remains hard bounded.
- One trigger parser validates field types, nonempty strings, glob compilation,
  imported routes and `applies_to` globs. Invalid active rules fail routing, and
  lint reports them. Valid unmatched and scope-only rules remain valid.
- `about`, `asks_about` and `related` revise and attribute only their source.
  Two concurrent evidence writers and independent protected proposals sharing an
  entity succeed without revising it. Tests retain protected-source review,
  genuine same-record conflicts, revision checks, receipts, retries and frozen
  source protection, while allowing references to frozen targets.
- Answered local questions and their unique accepted answers are adjacent,
  indivisible optional groups. Attribution, revision, IDs and consumption audit
  coverage survive; offered/declined alternatives and imported frozen questions
  stay out. Missing/multiple accepted answers fail explicitly.
- Both render formats cover all 28 native kind/lifecycle combinations, stable
  ID-based paths, complete graph fields and edges, relative links with anchors,
  and supplemental indexes. Imported ADR numbers are optional metadata and
  legacy aliases; old-store reads derive numbers without a database write.
- Collision and fresh-destination safeguards remain. An incompatible attempt
  to retire an imported ADR in favor of an unnumbered native decision is refused;
  adding a supplemental assertion without that legacy status edit succeeds.

## Local gates

| Gate | Result |
| --- | --- |
| `cargo fmt --check` | Pass |
| `cargo clippy --locked --all-targets -- -D warnings` | Pass |
| `cargo test --locked --all-targets` | 13 unit and 11 real-Dolt tests pass |
| `cargo build --locked --release --all-targets` | Pass |
| Standalone demonstration | Pass |
| Imported-store demonstration | Pass |
| Beads boundary fixture | 1 test passes |
| Pinned Beads 1.3.0 disposable-store qualification | Pass |

Dolt 2.2.0 is the privately installed pinned binary. Cargo was invoked directly
with the kache compiler wrapper and test runner disabled because their target
protection cannot run inside this session's filesystem sandbox.

## Pinned corpus

Source commit: `77f9200961d6b344fdf9ae89d3b612271d04a98d`. Only Git objects at this
revision were imported. The exact pinned PROOF policy pack was verified before
copying it into offline validation fixtures; the Beads helper and workspace were
set to unavailable fixture paths.

| Check | Result |
| --- | ---: |
| Original paths compared | 9,242 |
| Markdown / other assets | 8,199 / 1,043 |
| Pristine original-byte mismatches | 0 |
| Frozen backlog/question paths preserved after edits and additions | 3,407 |
| Authored records across all ten native kinds | 13 |
| Exact permitted additional content paths | 49 |
| Pinned Liskov directory-index comparisons | 6 indexes in each of 4 views |
| Additional validator diagnostics | 0 |

The 49 additional content paths are 34 imported repository-entity pages,
13 authored record pages, the graph sidecar and supplemental index. The two
export markers (`RENDERED` and `SCROLLS-MANIFEST.json`) are accounted for separately.
The manifest maps every one of the 8,246 normalized records to a rendered file.
Native decisions and local questions/answers remain supplemental views; they
never become numbered ADRs or legacy `Q-…` work records.

The representative imported edit changes only
`raw/2026-06-22-repo-inventory-readback.md` and `raw/README.md`.
After native additions, the only further original-file changes are
`knowledge/README.md` and `work-notes/README.md`. The qualification checks the
exact permitted added paths and independently compares all six directory
indexes against the pinned Liskov Python renderer, even when baseline validator
errors would skip its normal freshness stage.

All four validator runs (source, pristine, edited and native additions) exit
**1 with the same pre-existing diagnostic**: ADR-0238's `delivered_by` names
`EPIC-20261015-k7wd`, which is absent from the offline catalogue. This is a
no-regression qualification, not a claim that the historical corpus validates
cleanly without its live ledger.

## Evidence and reproduction

Run the [README verification commands](../README.md#verification). Local artifacts
are retained under the ignored `.verification/` directory of the isolated
`agent/correctness-1-6` worktree:

- `integrated-checks/report.json`, plus format, Clippy, test and release logs.
- `integrated-corpus/report.json`, four validator logs and source/rendered/edited/native fixtures.
- `integrated-demo/transcript.json` and `integrated-corpus/demo/transcript.json`.

Private corpus bytes, databases and transcripts are not committed or uploaded
to CI. Existing proposals are not rewritten. Remote synchronization, approval
revision redesign, live-ledger reconciliation and authority cutover remain
outside this change. Liskov's ADR-specific readers require a later migration
before native decisions can be authoritative catalogue entries.
