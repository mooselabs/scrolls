# Revision-bound proposal acceptance qualification

Date: 2026-10-18. This change binds proposal acceptance to the proposal and main
revisions returned by inspection. It does not change documentation authority,
rejection policy, or local answer acceptance. Roles remain cooperative.

## Contract

`review BRANCH` returns `main_revision`, `proposal_revision`, and a deterministic
versioned `review_token`. Human output prints both revisions and the token after
the diff. Accept with `--role human review accept BRANCH --review-token TOKEN`.
Any advance of main or the proposal requires a fresh review, even when the main
write is unrelated. Dry runs enforce the same binding. There is no bypass.

The existing store lock covers inspection and acceptance. For new acceptances,
the token check runs before a temporary merge branch or recovery journal is
created. Existing conflict detection, structural validation, transactional
receipts, and guarded main advancement remain in place.

Receipt lookup precedes the new check. Identical completed operations replay
across later main advances; changing the token with the same operation ID is an
argument mismatch. Absent tokens are omitted from command serialization, so
historical successful tokenless receipts still replay. New tokenless acceptance
is refused. Neither schema-1 nor schema-2 stores require a migration.

## Local validation

The release binary SHA-256 is
`09c13b88ee38fc7af1268642e225db53544890b1c8a47b2307574932cabe2917`.
Validation ran in the dedicated `agent/reviewed-revisions` worktree using private
Dolt 2.2.0 and disposable stores. Beads qualification used checksum-verified
Beads 1.3.0 and disposable workspaces.

| Gate | Result |
| --- | --- |
| `cargo fmt --check` | Pass |
| `cargo clippy --locked --all-targets -- -D warnings` | Pass |
| `cargo test --locked --all-targets` | 13 unit, 16 real-Dolt, and 1 Beads adapter test pass |
| `cargo build --locked --release --all-targets` | Pass |
| `scripts/demo.py` against the release binary | Pass; all 3 acceptances retain the inspected token and revisions |
| `scripts/verify-beads.py` against the release binary | Pass; work state and unrelated metadata preserved |
| README fish examples and Python script syntax | Pass |
| Pinned full-corpus qualification, including imported-store demo | Pass; zero additional validator diagnostics |

Focused real-Dolt coverage exercises unchanged repeated inspection; body,
metadata, and relationship changes; unrelated main advances; malformed,
unsupported-version, wrong-branch, and missing tokens; dry-run state preservation;
and fresh review after a harmless main advance. Failure snapshots compare every
branch head and operation receipt and check for a journal before opening the
store, so automatic recovery cannot conceal a leftover journal.

Two concurrent acceptance processes using the same reviewed main produce exactly
one success and one stale failure. Further coverage checks human-role enforcement,
rejection, completed-operation replay, changed-token retry refusal, historical
receipt compatibility, both supported store schemas, and interrupted acceptance
before and after main advancement. Existing conflict, structural-validation,
and invalid-doctrine tests now use fresh tokens and still enforce their checks.

## Pinned corpus

The source is the committed Liskov corpus at
`77f9200961d6b344fdf9ae89d3b612271d04a98d`, matching the previous
[correctness qualification](correctness-qualification.md). The pristine export
preserves all 9,242 original paths byte-for-byte.

The completed runner passed with 8,199 Markdown files and 1,043 assets,
zero byte mismatches, 3,407 frozen backlog/question files preserved, and
13 native records authored. All six directory indexes matched the pinned
renderer in each of the four snapshots. The demonstration also passed against
the imported store, using the preserved token for every proposal acceptance.

Source, pristine export, edited export, and native-record export produced identical
validator output and exit code 1: the pre-existing `ADR-0238` reference to missing
`EPIC-20261015-k7wd`, plus the explicit unavailable-ledger notice. There were
**zero additional validator diagnostics**. This is a no-regression baseline,
not a claim that the historical corpus validates without errors.

Reproduce from the Scrolls worktree with a fresh output directory:

```fish
python3 scripts/verify-corpus.py /path/to/liskov-agent-orchestrator \
    --rev 77f9200961d6b344fdf9ae89d3b612271d04a98d \
    --binary target/release/scrolls --out /tmp/scrolls-reviewed-revisions-corpus
```

The corpus runner uses the exact pinned policy-pack cache and compares validator
output across source, pristine export, edited export, and native-record export.
It also verifies that the release binary digest stays unchanged during the run.
