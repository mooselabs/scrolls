# Scrolls

Scrolls is a Rust CLI for versioned knowledge graphs for humans and agents,
backed by a private, pinned Dolt binary. Markdown bodies remain readable while
typed records and relationships make evidence and decisions queryable.

## Floor (ADR-0284)

The language check that fails the gate is
`cargo clippy --locked --all-targets -- -D warnings`. CI's `rust-and-dolt` job
and the factory gate (`gascity-packs/proof/assets/scripts/gates/scrolls.sh`)
run it. This Rust repository adds no linter for its Python helpers.

Every deliberate debt marker carries a ledger id (`BKLG-YYYYMMDD-xxxx`,
`EPIC-YYYYMMDD-xxxx` or `Q-YYYYMMDD-xxxx`) on its first line or the line above,
or a permanent, justified suppression carries a reason saying why it is not debt.
Markers include comments beginning TODO, FIXME, XXX or HACK; Rust `allow` and
`expect` (including `cfg_attr`), `rustfmt::skip` and `ignore`; Cargo lint levels
set to `allow`; Python `noqa`, `type: ignore`, `pylint: disable` and
`pragma: no cover`; shell `shellcheck disable=`; JavaScript/TypeScript
`eslint-disable`, `@ts-ignore`, `@ts-expect-error`; and `//nolint`.
Size-exception, raise or allow-list rows and deliberate compatibility re-exports
or legacy paths are also markers; the script does not scan these last two kinds,
and the baseline's size holds carry no id or reason, pending an owner decision.

Reasons use Rust `reason = "…"` inside the attribute or `#[ignore = "…"]`;
Python/shell suppressions use a further `#` followed by text on the same line;
ESLint uses ` -- reason`; TypeScript uses text after `@ts-ignore` or
`@ts-expect-error`. An ordinary comment after a Rust attribute is not a reason.
Deferral comments require an id. New Rust suppressions use
`#[expect(lint, reason = "…")]`; target-specific suppressions stay `allow`
with a reason because CI's stable toolchain is unpinned and gated `expect` was
verified only on Rust 1.98.1. Use Clippy configuration for permanent test-code
rules such as `allow-unwrap-in-tests` and `allow-expect-in-tests`.

`python3 scripts/check-floor.py` runs the floor checks. CI and the factory gate
run `python3 scripts/test-check-floor.py`, then `python3 scripts/check-floor.py`,
between `cargo fmt --check` and Clippy. Existing anonymous markers are counted by
path, kind and normalized first-line text in `scripts/floor-baseline.json`.
Do not convert them in bulk. A touched marker gains an id or reason, or is
removed; lower or remove its baseline row in the same change. The baseline never
grows. The check compares against the merge-base baseline when available and
reports when history is unavailable; review must still refuse growth.

The module design target is 1,000 lines. New hand-written source files may not
exceed 2,000 lines; existing files over 2,000 may not grow. The baseline holds
those files: lower a hold when its file shrinks, and remove it at 2,000 lines or
on deletion. Lines are newline bytes, as in `wc -l`. Source extensions are
`.rs`, `.py`, `.sh`, `.js`, `.cjs`, `.mjs`, `.ts`, `.css`, `.html` and `.c`;
other extensions (data and documentation) are outside the size check. Both
checks exclude any `fixtures/` directory component and the baseline JSON itself.
Markers are scanned only in `.rs`, `.py`, `.sh`, `.js`, `.cjs`, `.mjs`, `.ts`
using their comment syntax, and Cargo lint tables in files named `Cargo.toml`.
Strings are not markers. Markdown, JSON, YAML, other TOML, HTML, CSS and images
are not scanned for markers. Both floor scripts are scanned like other source.

## Validation

Prepare the pinned private dependencies and compatibility sources as described
in README's Development and verification section, then run:

```sh
./scripts/install-dolt.sh
./scripts/install-beads.sh
git clone https://github.com/mooselabs/scrolls.git .verification/scrolls-0.3.0
git -C .verification/scrolls-0.3.0 checkout --detach 3ae0a38dcc4da0a508e82ce4737d226e06d6f2b4
git clone https://github.com/mooselabs/scrolls.git .verification/scrolls-shared-previous
git -C .verification/scrolls-shared-previous checkout --detach 024d95242ea811520f2925d3e27ab45c933145a5
cargo fmt --check
python3 scripts/test-check-floor.py
python3 scripts/check-floor.py
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release --all-targets
python3 scripts/demo.py --binary target/release/scrolls --out /tmp/scrolls-demo
python3 scripts/verify-beads.py --binary target/release/scrolls
```

Use a fresh demonstration output path. Full corpus qualification remains a
separate local check against the private source; see README for its prerequisites.
