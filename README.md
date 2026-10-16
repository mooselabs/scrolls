# Scrolls 0.2.0

A local Rust CLI for versioned document graphs with a configurable YAML ontology.
Use the bundled knowledge model or define your own kinds, fields, relationships,
lifecycles and review policies. Data lives in private Dolt 2.2.0 at
`.scrolls/dolt`. No daemon, model or remote is required.

## Install and start

Qualified platform: Linux x86_64. Rust 1.85+ and Git are required to build.
Python 3 runs the demonstration and corpus qualification.

```fish
./scripts/install-dolt.sh
cargo install --locked --path .
set -gx SCROLLS_ACTOR patrick
scrolls --dolt "$PWD/.tools/dolt" init
scrolls prime
scrolls new evidence --title 'Observed result' --body 'Measured behavior.'
scrolls search 'measured behavior'
```

For a custom ontology, initialize a fresh store with
`scrolls init --schema examples/handbook.yaml`. The
[handbook](examples/handbook.yaml) and [research](examples/research.yaml) examples
exercise the same engine as the [bundled schema](schemas/knowledge.yaml).

## Three responsibilities

- **Orientation:** `prime` delivers the configured schema guide, authored
  instructions and explicit startup pins. Its byte budget is soft and reported.
- **Retrieval:** `show`, `context`, `list`, `search`, `query` and direction-aware
  `walk`. Immediate references include historical records; follow bodies explicitly.
- **Authoring:** `new`, `edit`, `status`, `link`, `unlink`, atomic `batch`, and
  proposal review. Write policy and ownership guards come from configuration.

`schema show`, `schema export`, `schema validate FILE`, and
`schema apply FILE --expect REV` manage configuration. Applying a schema always
requires proposal review and revalidation against current data at acceptance.

See the [command reference](docs/commands.md), [0.2 migration guide](docs/migration-0.2.md),
[design](docs/design.md), and [selection examples](examples/queries/).
Document-specific commands and authoring shortcuts have been replaced by generic
operations. Retrieval profiles, generated summaries, embeddings and automatic
context expansion are outside this release.

## Safety and compatibility

Dolt access is serialized by a store lock. Batches use one initial revision
snapshot, increment each affected existing record once, and commit the graph
and one operation receipt atomically. `--dry-run` previews; `--op-id` makes retries
idempotent. Roles are cooperative attribution, not authenticated identities.
Guarded lifecycle changes use pre-change ownership and are checked again when a
human accepts a proposal. Frozen imported history remains immutable.

Store format 3, ontology format 1 and CLI JSON envelope 2 accompany this release.
Resolve pending proposals before `migrate`. Migration preserves identity,
revisions, receipts, source files and history, installing empty startup pins.
Native exports include the complete graph and active configuration. The pinned
Git Liskov adapter retains explicit compatibility checks and corpus qualification.
No live ledger migration or documentation-authority cutover is part of this release.

Beads connections are explicit: bind local work stores, refresh cached metadata,
and publish backlinks when requested. Ordinary reads remain offline. Beads
context contains cached metadata and directly linked document references.
The external Beads backlink payload remains version 1.

## Verify

```fish
./scripts/install-dolt.sh
./scripts/install-beads.sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release --all-targets
python3 scripts/demo.py --binary target/release/scrolls --out /tmp/scrolls-demo
python3 scripts/verify-beads.py --binary target/release/scrolls
```

The pinned corpus check uses a disposable destination and does not open the live
Beads ledger:

```fish
python3 scripts/verify-corpus.py /path/to/orchestrator --rev COMMIT --out /tmp/scrolls-corpus --binary target/release/scrolls
```

The exact source commit must have its pinned policy pack available locally. Prior
0.1 qualification is retained in [migration-report.md](docs/migration-report.md);
[0.2 qualification](docs/ontology-0.2-qualification.md) records the new release checks.
