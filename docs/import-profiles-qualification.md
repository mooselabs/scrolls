# Scrolls 0.3 import-profile qualification

The release integrates the configurable ontology foundation from `b6215fe` with
revision-bound review on `e4faa65`, then adds profile imports. The package is
0.3.0, store format 4, ontology/profile/plan format 1 and JSON envelope 2.

The isolated `agent/source-profiles-030` implementation passes formatting,
warnings-denied Clippy, all 54 Rust/Dolt tests, the all-target release build,
the generic demo, disposable Beads qualification and the pinned PEP qualification.
The final Liskov repeat and CI/main integration are still in progress; their tested
revisions will be recorded before delivery. No production store or documentation
authority is changed.

The frozen Linux x86_64 binary, built with Rust 1.98.1 and private Dolt 2.2.0,
has SHA-256 `fa22b36ada01a076967b8c6c0cf8574a370d0a4768106acceb2ebec652570f66`.
Its source snapshot and logs are retained under `.verification/import-profiles-030/`.
The PEP candidate digest is
`4268852d1e382634d3df3d5654294766164b293e897b27644b38a0a9df207b25`;
the canonical PEP profile SHA-256 is
`b4383a34c5cde664a674941c661dff378acee9a90b9c3fe1d6ef29b7988823b6`.

## Pinned Python corpus

The initial implementation run against Python PEP commit
`6068058b896441388bbf22f2607ab681193f6a3d` produces 744 records from 820 files,
113 declared header occurrences, 46 unique supersession edges and 40 unmapped
Requires occurrences. All targets resolve. Native export has zero original byte
or executable-mode mismatches. The raw source inventory digest is
`08ecd0e429bbc9e44d451790a62b96b85e82b5b89b114a9455d07d1aa523ac92`.

`scripts/verify-imports.py` runs planning, dry-run, apply, receipt retry, provenance,
lint and byte/mode export checks through the public CLI. CI runs it against the
same immutable public commit. Bodies remain literal RST; only declared metadata
references are extracted.

## Regression coverage

Tests exercise strict YAML, malformed and duplicate metadata, continuations,
CRLF, Unicode, lookup/type checks, numeric overflow, missing/null/false/zero,
templates, clone relocation, stable-key renames, overlap, zero documents,
raw Git objects despite export attributes, ignored uncommitted files, symlinks,
alias ambiguity and precedence, deduplicated replacement references and ownership.

Real Dolt checks include byte-identical persistent state after planning/dry-run,
dirty/journal refusal, stale main and attribution, corrupted plans, atomic rollback
of graph/originals/membership/manifests/receipts, direct and proposed imports,
namespace races, rejected retries, acceptance after schema changes, captured-field
protection, local relationships, native export and explicit Liskov rejection.

Migration tests preserve existing records, UUID, receipts, custom ontology and
historical branches. Recovery exercises partial DDL before main advances, cleanup
after fast-forward, and refusal to discard unexpected working state. Dolt DDL
survives SQL rollback; reset also leaves untracked tables. Recovery removes only
verified migration-owned tables on its temporary branch and never resets main.

The handbook and custom research ontology exercise the same mapper. The research
schema disallows additional fields; captured unknown metadata stays in provenance.
