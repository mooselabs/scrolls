# Scrolls

A local Rust CLI for versioned knowledge: evidence, knowledge, decisions,
questions, doctrine, entities, and their relationships. Data lives in a private
Dolt 2.2.0 database at `.scrolls/dolt`. No daemon, model, or remote is required.

This release does **not** change the orchestrator's documentation authority.
The original Git documentation remains authoritative.

## Install

Qualified platform: Linux x86_64. Rust 1.85+ and Git are required to build;
Python 3 is used only by the demonstration and full migration qualification.

```fish
./scripts/install-dolt.sh
cargo install --locked --path .
set -gx SCROLLS_ACTOR patrick
scrolls --dolt "$PWD/.tools/dolt" init
```

Run from this checkout, or pass an absolute `--dolt` path and `--store` path.
The installer verifies the pinned archive digest and never replaces a shared
Dolt binary. `--role agent` is the default. Roles are cooperative local policy,
not authenticated identities; anyone with filesystem access can bypass them.

## Quickstart (fish)

`jq` makes it convenient to use IDs returned by the versioned JSON interface.
Every write requires an actor. Pass a stable `--op-id` when retrying automation.

```fish
set evidence (scrolls --json --op-id evidence-demo new evidence \
    --title 'Rollback readback' --body 'The interrupted transaction left no rows.' | jq -r .data.id)
set proposal (scrolls --json --op-id knowledge-demo new knowledge \
    --title 'Atomic local writes' --body 'Writes and their receipts commit together.' \
    --supports $evidence)
set record (echo $proposal | jq -r .data.id)
set branch (echo $proposal | jq -r .data.branch)
scrolls --proposal $branch status $record current --expect 1
scrolls review $branch
scrolls --role human review accept $branch
scrolls show $record
scrolls history $record
scrolls render --out ./knowledge-export
```

The export destination must be new. Bodies accept `--body`, `--body-file PATH`,
`--body-file -`, or piped stdin. `edit` opens `$VISUAL` / `$EDITOR` on a terminal;
`--editor` explicitly selects that behavior. `show` returns each record's
numeric `revision`, which is required by `edit`, `status`, and graph mutations.

For a pinned import:

```fish
set source /path/to/liskov-agent-orchestrator
set revision (git -C $source rev-parse HEAD)
scrolls --json import markdown "$source/docs" --rev $revision
scrolls doctor
scrolls show ADR-0236
scrolls doctrine --for repos/liskov-rs/src/example.sql
scrolls render --format liskov --out ./liskov-docs-export
```

Import requires an empty store. It reads Git objects, excludes untracked and
uncommitted bytes, and preserves original metadata and assets. Repeating the
same source commit is safe; changed-source reimport requires a new store.
Imported backlog, epic, milestone, question, and untyped archive records are
immutable. Newly authored local questions are independent of the legacy inbox.

## Verification

```fish
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release --all-targets
python3 scripts/demo.py --out /tmp/scrolls-demo
python3 scripts/verify-corpus.py /path/to/orchestrator --rev COMMIT --out /tmp/scrolls-qualification
```

Real-Dolt tests fail if the private binary is missing. CI runs the same Rust
checks and the demonstration. Full corpus qualification is an explicit local
check because the orchestrator corpus is private; its pinned policy pack must
already be present in the source repository's verified cache.

See the [command reference](docs/commands.md), [migration report](docs/migration-report.md),
and [design baseline and implementation boundary](docs/design.md).
