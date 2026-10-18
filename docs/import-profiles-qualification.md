# Scrolls 0.3 import-profile qualification

Qualified implementation: `4cd27bd51bcac1e287197481d8cc206a8a23de99`.
Package 0.3.0 uses store format 4, ontology/profile/plan format 1 and JSON envelope 2.
All checks below ran against one frozen binary and unchanged source/test inputs.
Later qualification-document commits do not change the tested implementation.
No production store or documentation authority was migrated.

## Checks and provenance

Formatting, warnings-denied Clippy, all **56 Rust/Dolt tests**, the all-target
release build, generic demo, disposable Beads 1.3.0 qualification, pinned PEP
qualification and full pinned Liskov qualification pass. The toolchain was Rust
1.98.1 on Linux x86_64 with private Dolt 2.2.0.

- Binary SHA-256: `46aec1b9d0b370b8a4891fc5979ac5370e89f24760bbb329d8da1f5a9ecc9286`.
- Source/test snapshot SHA-256: `0dd8531150dc031fe85f1e651aeb07565fd1c91bb64806666f1ffb2c1dad4928`.
- PEP profile SHA-256: `b4383a34c5cde664a674941c661dff378acee9a90b9c3fe1d6ef29b7988823b6`.
- Handbook profile SHA-256: `34611afcc372b6a959d86ef021eaaa611ce48a849aa760a8dfb6b3f4b5dfaab8`.

Logs, source hashes, immutable binaries and reports are retained locally under
`.verification/import-profiles-030/`. Private corpus bytes are not published.

## Pinned Python corpus

Python PEP commit `6068058b896441388bbf22f2607ab681193f6a3d` produces:

| Result | Count |
| --- | ---: |
| Records | 744 |
| Original files | 820 |
| Declared header references | 113 |
| Unique supersession edges | 46 |
| Unmapped Requires occurrences | 40 |
| Unresolved header targets | 0 |
| Original byte mismatches | 0 |
| Git executable-mode mismatches | 0 |

Candidate digest: `212e3abc8f397c9220350c431755b9ae287320fe2bfa5c21e11b0f7a7db49136`.
Raw path/mode/blob/content inventory digest:
`08ecd0e429bbc9e44d451790a62b96b85e82b5b89b114a9455d07d1aa523ac92`.

`scripts/verify-imports.py` exercises planning, dry-run, apply, exact receipt retry,
provenance, lint and native export through the public CLI. CI runs the same check
against this immutable public commit. Bodies remain literal RST; reference coverage
is declared metadata only. Body links and RST roles are not extracted.

## Liskov compatibility

The pinned source is `77f9200961d6b344fdf9ae89d3b612271d04a98d`.
All 9,242 original paths (8,199 Markdown and 1,043 assets) remain byte-identical.
All six independently generated indexes match, 3,407 frozen backlog/question
files remain preserved, and 13 native records plus the imported-store demo
succeed. Source, pristine, edited and native validators return identical exit
codes `[1, 1, 1, 1]`, with **zero additional diagnostics**. This preserves the
source's existing diagnostic debt; it does not claim a clean historical corpus.

## Migration of the real corpus

A qualified format-3 Liskov database was copied using Dolt `backup sync-url` and
`backup restore`, under the source store lock. The original store was retained.
The restored copy, including authored records and reviewed startup pins, migrated
to format 4 and replayed its migration receipt successfully.

- 8,250 records, 44,079 edges and 9,242 original-file rows are unchanged.
- All 33 old receipts, 35 prior commits and 8 branches are preserved; only main advances.
- Store UUID and the configured ontology, including pins, are unchanged.
- All 9,297 native export paths remain present. Content bytes match except
  the expected revision/version/manifest files; graph records, edges and schema match.
- Historical branches remain readable. The actual 0.2 binary refuses a write to
  format-4 main after a historical read, and main's revision stays unchanged.

The migration adds the three provenance tables, its receipt/recovery metadata,
and missing legacy Beads registry entries. Existing registry entries are preserved.
The raw table hashes and complete readback are in `full-migration-v2/report.json`.

## Regression boundaries

Tests cover strict YAML, malformed/duplicate headers, continuations, CRLF, Unicode,
templates, lookup/type errors, numeric overflow and missing/null/false/zero;
clone relocation and stable-key renames; overlapping/empty document selections;
raw Git reads despite export attributes; ignored uncommitted files; symlinks;
qualified aliases and Beads precedence; and duplicate/reversed reference assertions.
Forbidden relation mappings fail even when reference values or targets are absent.

Real Dolt checks cover byte-identical persistent state after planning/dry-run;
lock-bound recovery checks; dirty/journal refusal; stale main and attribution;
corrupted plans; source namespace races; protected proposal isolation; rejected
and accepted retries; schema pinning; immutable captured properties and edges;
local support links; and rollback of graph, originals, membership, manifests and
receipts. A synthetic file-key collision preserves the previous tuple and commits
no imported graph or successful receipt.

Migration recovery covers partial DDL, cleanup after fast-forward, and refusal to
discard unexpected rows or working state. Dolt DDL survives SQL rollback, and
reset leaves untracked tables. Recovery drops only verified empty migration-owned
tables on its temporary branch and never resets main.

The handbook profile and a custom research ontology use the same mapping engine.
The research schema disallows additional fields; unmapped raw metadata remains in
engine provenance. Changed-source refresh, deletion reconciliation, formatted RST
and section-level retrieval remain outside 0.3.

## Delivery

The 0.2 foundation landed through [PR #2](https://github.com/mooselabs/scrolls/pull/2)
at `3c53f60660cba5ae19f2e9a5fd77f701d589b154`; its
[main CI run](https://github.com/mooselabs/scrolls/actions/runs/39151589612) passed.
Its qualified implementation was `3ea5b25685bb24f78d38eb74ac72bee96be32a68`.
The 0.3 change is delivered through [PR #3](https://github.com/mooselabs/scrolls/pull/3),
which records the final reviewed head, merge revision and CI checks.
