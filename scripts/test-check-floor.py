#!/usr/bin/env python3
"""Exercise the floor's public command in disposable tracked trees."""

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
CHECK = ROOT / "scripts/check-floor.py"
ATTRIBUTE = "#[allow(dead_code)]\n"


class FloorTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        self.git("init", "--quiet", "--initial-branch=main")
        self.write_baseline()
        self.commit()

    def git(self, *args):
        return subprocess.run(
            ["git", "-c", "user.name=Floor Test", "-c", "user.email=floor@example.invalid",
             "-c", "commit.gpgsign=false", *args],
            cwd=self.root, env=self.env, capture_output=True, text=True, check=True,
        ).stdout.strip()

    def write(self, path, text):
        destination = self.root / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(text)
        self.git("add", "--", path)

    def write_baseline(self, markers=None, held=None):
        self.write("scripts/floor-baseline.json", json.dumps({
            "markers": [] if markers is None else markers,
            "held": {} if held is None else held,
        }))

    def row(self, path="test.rs", text=ATTRIBUTE.strip(), count=1, kind="rust-attribute"):
        return {"path": path, "kind": kind, "text": text, "count": count}

    def commit(self):
        self.git("commit", "--quiet", "--allow-empty", "-m", "fixture")

    def check(self, status=0, finding=None):
        result = subprocess.run(["python3", str(CHECK)], cwd=self.root, env=self.env,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, status, result.stdout + result.stderr)
        if finding is not None:
            self.assertIn(finding, result.stdout)
        return result.stdout

    def test_matching_baseline_passes(self):
        self.write("test.rs", ATTRIBUTE)
        self.write_baseline([self.row()], {"large.rs": 2001})
        self.write("large.rs", "\n" * 2001)
        self.check(finding="floor: 1 anonymous markers held, 1 files held")

    def test_new_allow_fails(self):
        self.write("test.rs", ATTRIBUTE)
        self.check(1, "test.rs:1: new anonymous rust-attribute")

    def test_allow_reason_passes(self):
        self.write("test.rs", '#[allow(dead_code, reason = "platform entry")]\n')
        self.check()

    def test_expect_reason_passes(self):
        self.write("test.rs", '#[expect(dead_code, reason = "platform entry")]\n')
        self.check()

    def test_allow_id_above_passes(self):
        self.write("test.rs", "// BKLG-20261108-aot6\n" + ATTRIBUTE)
        self.check()

    def test_allow_ordinary_comment_fails(self):
        self.write("test.rs", ATTRIBUTE.rstrip() + " // an ordinary comment\n")
        self.check(1, "test.rs:1:")

    def test_multiline_cfg_attr_fails(self):
        self.write("test.rs", "#![cfg_attr(\n    not(test),\n    allow(dead_code)\n)]\n")
        self.check(1, "test.rs:1:")

    def test_multiline_cfg_attr_reason_passes(self):
        self.write("test.rs", '#![cfg_attr(\n not(test),\n allow(dead_code, reason = "platform entry")\n)]\n')
        self.check()

    def test_new_deferral_fails(self):
        self.write("test.rs", "fn main() {}\n// TODO later\n")
        self.check(1, "test.rs:2: new anonymous deferral")

    def test_deferral_id_passes(self):
        self.write("test.rs", "// TODO later BKLG-20261108-aot6\n")
        self.check()

    def test_rust_strings_pass(self):
        self.write("test.rs", '''let a = "TODO"; let b = r#"// TODO"#;
let c = b"#[allow(dead_code)]"; let d = br##"/* FIXME */"##;
let e = '\\''; let f = "\\\" // TODO";
''')
        self.check()

    def test_shell_quoted_template_passes(self):
        self.write("test.sh", 'scratch=$(mktemp -d "XXX")\necho "# TODO later"\n')
        self.check()

    def test_manifest_allow_fails(self):
        self.write("Cargo.toml", '[lints.clippy]\nneedless_return = "allow"\n')
        self.check(1, "Cargo.toml:2: new anonymous manifest-lint")

    def test_manifest_inline_allow_fails(self):
        self.write("Cargo.toml", '[workspace.lints.rust]\nunused = { level = "allow", priority = -1 }\n')
        self.check(1, "Cargo.toml:2:")

    def test_manifest_id_passes(self):
        self.write("Cargo.toml", '[lints.clippy]\n# Q-20261108-aot6\nneedless_return = "allow"\n')
        self.check()

    def test_manifest_strings_are_not_tables(self):
        self.write("Cargo.toml", '''[package.metadata]
example = """
[lints.clippy]
needless_return = "allow"
"""
''')
        self.check()

    def test_shellcheck_fails(self):
        self.write("test.sh", "# shellcheck disable=SC2064\n")
        self.check(1, "test.sh:1: new anonymous comment-suppression")

    def test_shellcheck_reason_passes(self):
        self.write("test.sh", "# shellcheck disable=SC2064 # the trap expands now\n")
        self.check()

    def test_python_noqa_fails(self):
        self.write("test.py", "import os  # noqa\n")
        self.check(1, "test.py:1: new anonymous comment-suppression")

    def test_python_reason_passes(self):
        self.write("test.py", "import os  # noqa # imported for registration\n")
        self.check()

    def test_python_string_passes(self):
        self.write("test.py", 's = "# noqa"\nt = """\n# TODO later\n"""\n')
        self.check()

    def test_eslint_fails(self):
        self.write("test.js", "// eslint-disable-next-line no-console\n")
        self.check(1, "test.js:1:")

    def test_eslint_reason_passes(self):
        self.write("test.js", "// eslint-disable-next-line no-console -- reason\n")
        self.check()

    def test_markdown_is_not_scanned(self):
        self.write("guide.md", "#[rustfmt::skip]\n#[ignore]\n//nolint\n// TODO\n")
        self.check()

    def test_fixture_excluded(self):
        self.write("tests/fixtures/test.rs", "// TODO later\n" * 2001)
        self.check()

    def test_removed_marker_stale(self):
        self.write("test.rs", "fn main() {}\n")
        self.write_baseline([self.row()])
        self.check(1, "test.rs:1: stale row: lower or remove it")

    def test_duplicate_count_stale(self):
        self.write("test.rs", ATTRIBUTE)
        self.write_baseline([self.row(count=2)])
        self.check(1, "test.rs:1: stale row:")

    def test_duplicate_markers_use_count(self):
        self.write("test.rs", ATTRIBUTE * 2)
        self.write_baseline([self.row()])
        self.check(1, "test.rs:2:")

    def test_duplicate_rows_rejected(self):
        self.write_baseline([self.row(), self.row()])
        self.check(1, "scripts/floor-baseline.json:1: cannot check floor:")

    def test_marker_line_move_passes(self):
        self.write("test.rs", "\n\n" + ATTRIBUTE)
        self.write_baseline([self.row()])
        self.check()

    def test_marker_text_change_fails(self):
        self.write("test.rs", "#[allow(unused)]\n")
        self.write_baseline([self.row()])
        self.check(1, "test.rs:1: new anonymous")

    def test_new_large_source_fails(self):
        self.write("test.rs", "\n" * 2001)
        self.check(1, "test.rs:1: new file over 2,000 lines")

    def test_large_data_passes(self):
        self.write("test.json", "\n" * 2001)
        self.check()

    def test_held_growth_fails(self):
        self.write("test.rs", "\n" * 2002)
        self.write_baseline(held={"test.rs": 2001})
        self.check(1, "test.rs:1: held file grew")

    def test_held_shrink_requires_lower_hold(self):
        self.write("test.rs", "\n" * 2001)
        self.write_baseline(held={"test.rs": 2002})
        self.check(1, "test.rs:1: lower its hold to 2001")

    def test_lowered_hold_passes(self):
        self.write("test.rs", "\n" * 2002)
        self.write_baseline(held={"test.rs": 2002})
        self.commit()
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.write("test.rs", "\n" * 2001)
        self.write_baseline(held={"test.rs": 2001})
        self.check()

    def test_small_file_hold_removed(self):
        self.write("test.rs", "\n" * 2000)
        self.write_baseline(held={"test.rs": 2001})
        self.check(1, "test.rs:1: remove its hold")

    def test_missing_file_hold_removed(self):
        self.write_baseline(held={"test.rs": 2001})
        self.check(1, "test.rs:1: remove its hold")

    def test_grown_baseline_marker_fails(self):
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.write("test.rs", ATTRIBUTE)
        self.write_baseline([self.row()])
        self.check(1, "scripts/floor-baseline.json:1: baseline marker count grew")

    def test_grown_baseline_count_fails(self):
        self.write("test.rs", ATTRIBUTE)
        self.write_baseline([self.row()])
        self.commit()
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.write("test.rs", ATTRIBUTE * 2)
        self.write_baseline([self.row(count=2)])
        self.check(1, "scripts/floor-baseline.json:1: baseline marker count grew")

    def test_grown_baseline_hold_fails(self):
        self.write("test.rs", "\n" * 2001)
        self.write_baseline(held={"test.rs": 2001})
        self.commit()
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.write("test.rs", "\n" * 2002)
        self.write_baseline(held={"test.rs": 2002})
        self.check(1, "scripts/floor-baseline.json:1: baseline hold grew")

    def test_new_baseline_hold_fails(self):
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.write("test.rs", "\n" * 2001)
        self.write_baseline(held={"test.rs": 2001})
        self.check(1, "scripts/floor-baseline.json:1: baseline hold grew")

    def test_no_origin_reports_unchecked(self):
        self.check(finding="baseline growth not checked (merge base unavailable)")

    def test_no_base_baseline_reports_unchecked(self):
        self.git("rm", "scripts/floor-baseline.json")
        self.commit()
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.write_baseline()
        self.check(finding="baseline growth not checked (merge base has no baseline)")

    def test_language_suppressions(self):
        cases = {"test.py": "# type: ignore\n# pylint: disable=unused-import\n# pragma: no cover\n",
                 "test.ts": "// @ts-ignore\n// @ts-expect-error\n//nolint\n",
                 "test.rs": "#[rustfmt::skip]\n#[ignore]\n#[expect(dead_code)]\n"}
        for path, content in cases.items():
            self.write(path, content)
        output = self.check(1)
        for path in cases:
            for line in range(1, 4):
                self.assertIn(f"{path}:{line}:", output)

    def test_reason_forms_and_literals(self):
        self.write("test.rs", '#[ignore = "needs a service"]\n#[allow(dead_code, reason = r#"platform"#)]\n')
        self.write("test.ts", '// @ts-ignore unsupported type\nconst x = `// TODO\n#[ignore]`;\n')
        self.check()

    def test_fake_or_empty_reason_fails(self):
        self.write("test.rs", '#[allow(dead_code)] // reason = "fake"\n#[allow(unused, reason = " ")]\n')
        output = self.check(1, "test.rs:1:")
        self.assertIn("test.rs:2:", output)

    def test_doc_and_block_deferrals(self):
        self.write("test.rs", "/// TODO later\n//! FIXME later\n/* HACK later */\n")
        output = self.check(1)
        for line in range(1, 4):
            self.assertIn(f"test.rs:{line}:", output)

    def test_language_specific_forms_only(self):
        self.write("test.py", 's = "#[ignore]"\n# eslint-disable\n')
        self.write("test.rs", '// noqa\n')
        self.write("test.css", "/* TODO later */\n")
        self.check()

    def test_repository_wiring(self):
        agents = (ROOT / "AGENTS.md").read_text()
        self.assertIn("## Floor (ADR-0284)", agents)
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        job = re.split(r"^  [\w-]+:\s*$", ci.split("  rust-and-dolt:\n", 1)[1],
                       maxsplit=1, flags=re.MULTILINE)[0]
        commands = ["cargo fmt --check", "python3 scripts/test-check-floor.py",
                    "python3 scripts/check-floor.py", "cargo clippy --locked --all-targets -- -D warnings"]
        positions = [job.index(f"- run: {command}") for command in commands]
        self.assertEqual(positions, sorted(positions))
        for command in commands[1:3]:
            self.assertIn(command, agents)
        self.assertIn("@AGENTS.md", (ROOT / "CLAUDE.md").read_text())


if __name__ == "__main__":
    unittest.main(verbosity=2)
