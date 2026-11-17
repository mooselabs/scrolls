#!/usr/bin/env python3
"""Check tracked source for anonymous debt and growing size holds (ADR-0284)."""

import collections
import json
from pathlib import Path
import re
import subprocess
import sys
import tomllib


BASELINE = "scripts/floor-baseline.json"
SOURCE = {".rs", ".py", ".sh", ".js", ".cjs", ".mjs", ".ts", ".css", ".html", ".c"}
COMMENT_SOURCE = {".rs", ".py", ".sh", ".js", ".cjs", ".mjs", ".ts"}
ID = re.compile(r"\b(?:BKLG|EPIC|Q)-[0-9]{8}-[a-z0-9]{4}\b")
DEFERRAL = re.compile(r"(?:TODO|FIXME|XXX|HACK)\b")
SUPPRESSIONS = {
    ".py": r"\b(?:noqa|type:\s*ignore|pylint:\s*disable|pragma:\s*no\s+cover)\b",
    ".sh": r"\bshellcheck\s+disable\s*=",
    ".rs": r"^nolint\b",
}
JS_SUPPRESSION = r"\beslint-disable[\w-]*\b|@ts-(?:ignore|expect-error)\b|^nolint\b"
RUST_RAW = re.compile(r'(?:br|r)(#+)?"')
RUST_CHAR = re.compile(r"'(?:\\[^\n]+?|[^'\\\n])'")


def git(*args):
    return subprocess.run(["git", *args], capture_output=True, check=True).stdout


def lexical(text, language):
    """Return code, comments and literals with offsets; masking preserves newlines."""
    code = list(text)
    comments, literals = [], []
    i = 0
    while i < len(text):
        start = i
        comment = None
        if language in {".py", ".sh", ".toml"} and text[i] == "#":
            comment = "#"
        elif language not in {".py", ".sh", ".toml"}:
            if text.startswith("//", i):
                comment = "//"
            elif text.startswith("/*", i):
                comment = "/*"
        if comment is not None:
            if comment == "/*":
                i += 2
                depth = 1
                while i < len(text) and depth:
                    if language == ".rs" and text.startswith("/*", i):
                        depth += 1
                        i += 2
                    elif text.startswith("*/", i):
                        depth -= 1
                        i += 2
                    else:
                        i += 1
            else:
                end = text.find("\n", i)
                i = len(text) if end == -1 else end
            opener = 2 if comment == "/*" else len(comment)
            if language == ".rs" and text[start:start + 3] in {"///", "//!"}:
                opener = 3
            body = text[start + opener:i - 2] if comment == "/*" else text[start + opener:i]
            comments.append((start, body))
        else:
            raw = RUST_RAW.match(text, i) if language == ".rs" else None
            quote = text[i]
            if raw is not None:
                delimiter = '"' + (raw[1] or "")
                content = raw.end()
                end = text.find(delimiter, content)
                i = len(text) if end == -1 else end + len(delimiter)
                literals.append((start, text[content:end if end != -1 else len(text)]))
            elif quote in "\"'`" and (quote != "`" or language in {".js", ".cjs", ".mjs", ".ts", ".sh"}):
                # A Rust lifetime is code, whereas a quoted character is a literal.
                if language == ".rs" and quote == "'" and RUST_CHAR.match(text, i) is None:
                    i += 1
                    continue
                delimiter = quote
                if language in {".py", ".toml"} and text.startswith(quote * 3, i):
                    delimiter *= 3
                content = i + len(delimiter)
                i = content
                while i < len(text) and not text.startswith(delimiter, i):
                    if text[i] == "\\" and not (language in {".sh", ".toml"} and quote == "'"):
                        i += 2
                    else:
                        i += 1
                literals.append((start, text[content:i]))
                i = min(len(text), i + len(delimiter))
            else:
                if language == ".sh" and quote == "\\":
                    i += 2
                else:
                    i += 1
                continue
        code[start:i] = ["\n" if c == "\n" else " " for c in text[start:i]]
    return "".join(code), comments, literals


def markers(path, text):
    """Yield anonymous identities and source locations, independent of Git/baselines."""
    language = path.suffix
    lines = text.splitlines()

    def marker(offset, kind, reason=False):
        line = text.count("\n", 0, offset) + 1
        nearby = "\n".join(lines[max(0, line - 2):line])
        if not reason and ID.search(nearby) is None:
            return (str(path), kind, " ".join(lines[line - 1].split())), line
        return None

    if language in COMMENT_SOURCE:
        code, comments, literals = lexical(text, language)
        for offset, body in comments:
            body = body.lstrip()
            if DEFERRAL.match(body):
                found = marker(offset, "deferral")
                if found is not None:
                    yield found
            pattern = SUPPRESSIONS.get(language, JS_SUPPRESSION)
            suppression = re.search(pattern, body)
            if suppression is not None:
                rest = body[suppression.end():]
                if language in {".py", ".sh"}:
                    reason = re.search(r"#\s*\S", rest) is not None
                elif suppression[0].startswith("@ts-"):
                    reason = bool(rest.strip())
                else:
                    reason = re.search(r"\s--\s*\S", rest) is not None
                found = marker(offset, "comment-suppression", reason)
                if found is not None:
                    yield found
        if language == ".rs":
            for attribute in re.finditer(r"#!?\s*\[", code):
                start, end, depth = attribute.start(), attribute.end(), 1
                while end < len(code) and depth:
                    depth += (code[end] == "[") - (code[end] == "]")
                    end += 1
                inner = code[attribute.end():end - 1].strip()
                if not (re.match(r"(?:allow|expect)\s*\(|rustfmt\s*::\s*skip\b|ignore\b", inner)
                        or (re.match(r"cfg_attr\s*\(", inner) and re.search(r"\b(?:allow|expect)\s*\(", inner))):
                    continue
                reason = False
                for field in re.finditer(r"\b(?:reason|ignore)\s*=", code[start:end]):
                    value_at = start + field.end()
                    reason = reason or any(value_at <= pos < end and not text[value_at:pos].strip() and value.strip()
                                           for pos, value in literals)
                found = marker(start, "rust-attribute", reason)
                if found is not None:
                    yield found
    elif path.name == "Cargo.toml":
        data = tomllib.loads(text)
        code, _, _ = lexical(text, ".toml")
        table = []
        offset = 0
        for raw, masked in zip(text.splitlines(keepends=True), code.splitlines(keepends=True)):
            if masked.lstrip().startswith("["):
                header = tomllib.loads(raw)
                table = []
                while isinstance(header, dict) and len(header) == 1:
                    key, header = next(iter(header.items()))
                    table.append(key)
            key = re.match(r'\s*([\w-]+|"[^"]+"|\x27[^\x27]+\x27)\s*=', raw)
            is_lints = table[:1] == ["lints"] or table[:2] == ["workspace", "lints"]
            if key is not None and "=" in masked and is_lints:
                value = data
                for part in table + [key[1].strip("\"'")]:
                    value = value[part]
                if value == "allow" or (isinstance(value, dict) and value.get("level") == "allow"):
                    found = marker(offset, "manifest-lint")
                    if found is not None:
                        yield found
            offset += len(raw)


def baseline(data):
    """Validate the small policy file before using its allowances."""
    rows, held = data["markers"], data["held"]
    counts = collections.Counter()
    for row in rows:
        identity = tuple(row[key] for key in ("path", "kind", "text"))
        if not all(isinstance(value, str) and value for value in identity):
            raise ValueError("marker identity must contain nonempty strings")
        if identity in counts or type(row["count"]) is not int or row["count"] < 1:
            raise ValueError("marker rows must be unique with positive integer counts")
        counts[identity] = row["count"]
    if not isinstance(held, dict) or any(not isinstance(p, str) or type(n) is not int or n <= 2000 for p, n in held.items()):
        raise ValueError("holds must name source files over 2,000 lines")
    return counts, held


def growth(counts, held, report):
    try:
        base = git("merge-base", "HEAD", "origin/main").decode().strip()
    except subprocess.CalledProcessError:
        print("floor: baseline growth not checked (merge base unavailable)")
        return
    try:
        old = git("show", f"{base}:{BASELINE}")
    except subprocess.CalledProcessError:
        print("floor: baseline growth not checked (merge base has no baseline)")
        return
    old_counts, old_held = baseline(json.loads(old))
    for identity, count in counts.items():
        if count > old_counts[identity]:
            report(BASELINE, 1, f"baseline marker count grew: {identity[0]} {identity[2]}")
    for path, size in held.items():
        if size > old_held.get(path, 0):
            report(BASELINE, 1, f"baseline hold grew: {path}")


def main():
    findings = []

    def report(path, line, message):
        findings.append(f"{path}:{line}: {message}")

    try:
        counts, held = baseline(json.loads(Path(BASELINE).read_text()))
        seen = collections.Counter()
        sizes = {}
        paths = git("ls-files", "-z").decode().split("\0")
        for name in filter(None, paths):
            path = Path(name)
            if "fixtures" in path.parts[:-1] or name == BASELINE:
                continue
            if path.suffix not in SOURCE and path.name != "Cargo.toml":
                continue
            content = path.read_bytes()
            if path.suffix in SOURCE:
                sizes[name] = content.count(b"\n")
            for identity, line in markers(path, content.decode()):
                seen[identity] += 1
                if seen[identity] > counts[identity]:
                    report(name, line, f"new anonymous {identity[1]}: {identity[2]}")
        for identity, count in counts.items():
            if seen[identity] < count:
                report(identity[0], 1, "stale row: lower or remove it")
        for path, size in sizes.items():
            if size > 2000 and path not in held:
                report(path, 1, "new file over 2,000 lines or file grew past 2,000 lines")
        for path, limit in held.items():
            size = sizes.get(path)
            if size is None or size <= 2000:
                report(path, 1, "remove its hold")
            elif size > limit:
                report(path, 1, "held file grew")
            elif size < limit:
                report(path, 1, f"lower its hold to {size}")
        growth(counts, held, report)
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        report(BASELINE, 1, f"cannot check floor: {error}")
    for finding in findings:
        print(finding)
    if findings:
        return 1
    print(f"floor: {sum(counts.values())} anonymous markers held, {len(held)} files held")
    return 0


if __name__ == "__main__":
    sys.exit(main())
