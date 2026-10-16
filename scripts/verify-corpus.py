#!/usr/bin/env python3
"""Pinned Git import/export and independent validator qualification; never opens live Beads."""

import argparse, hashlib, io, json, os, pathlib, shutil, subprocess, tarfile

p = argparse.ArgumentParser()
p.add_argument("repository", type=pathlib.Path)
p.add_argument("--rev", required=True)
p.add_argument("--out", type=pathlib.Path, required=True)
p.add_argument(
    "--binary", type=pathlib.Path, default=pathlib.Path("target/release/scrolls")
)
p.add_argument("--existing-store", type=pathlib.Path)
a = p.parse_args()
root = pathlib.Path(__file__).resolve().parents[1]
repo = a.repository.resolve()
out = a.out.resolve()
out.mkdir(parents=True, exist_ok=False)


def run(args, **kw):
    return subprocess.run(
        [str(s) for s in args], check=True, capture_output=True, **kw
    ).stdout


sha = run(["git", "-C", repo, "rev-parse", a.rev + "^{commit}"], text=True).strip()
archive = run(["git", "-C", repo, "archive", sha])
for tree in ["source", "rendered"]:
    dest = out / tree
    dest.mkdir()
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        tar.extractall(dest, filter="data")
    # Install a verified copy of the exact pinned policy pack without a network or ledger call.
    pin = json.loads((dest / "gascity/proof-pack.lock.json").read_text())
    source = repo / ".cache/proof-pack" / pin["sha256"] / "proof"
    entries = []
    for path in sorted(source.rglob("*")):
        if "__pycache__" in path.parts:
            continue
        assert not path.is_symlink()
        if path.is_file():
            entries.append(
                [
                    path.relative_to(source).as_posix(),
                    bool(path.stat().st_mode & 0o111),
                    hashlib.sha256(path.read_bytes()).hexdigest(),
                ]
            )
    assert (
        hashlib.sha256(json.dumps(entries, separators=(",", ":")).encode()).hexdigest()
        == pin["sha256"]
    )
    target = dest / ".cache/proof-pack" / pin["sha256"] / "proof"
    shutil.copytree(source, target, ignore=shutil.ignore_patterns("__pycache__"))
    (dest / ".cache/proof-pack/current").symlink_to(pin["sha256"])
    link = dest / "gascity/packs/proof"
    link.parent.mkdir(exist_ok=True, parents=True)
    if not link.is_symlink():
        link.symlink_to("../../.cache/proof-pack/current/proof")
store = (a.existing_store or out / "store").resolve()
base = [
    a.binary.resolve(),
    "--json",
    "--actor",
    "qualification",
    "--dolt",
    root / ".tools/dolt",
    "--store",
    store,
]


def cli(*args):
    return json.loads(run(base + list(args)))


cli("init")
imported = cli("import", "markdown", repo / "docs", "--rev", sha)
shutil.rmtree(out / "rendered/docs")
rendered = cli("render", "--format", "liskov", "--out", out / "rendered/docs")
files = {
    p.relative_to(out / "source/docs")
    .as_posix(): hashlib.sha256(p.read_bytes())
    .hexdigest()
    for p in (out / "source/docs").rglob("*")
    if p.is_file()
}
actual = {
    p.relative_to(out / "rendered/docs")
    .as_posix(): hashlib.sha256(p.read_bytes())
    .hexdigest()
    for p in (out / "rendered/docs").rglob("*")
    if p.is_file() and p.name not in ["RENDERED", "SCROLLS-MANIFEST.json"]
}
assert files == actual, "path/byte roundtrip mismatch"
index_result = run(
    [root / "target/release/examples/check_indexes", out / "rendered/docs"], text=True
)
# Exercise a representable imported edit and validate its refreshed indexes.
selected = next(
    p
    for p in sorted(files)
    if p.startswith("raw/")
    and p.count("/") == 1
    and p.endswith(".md")
    and b"doc_type: raw" in (out / "source/docs" / p).read_bytes()
)
record = cli("show", selected)["data"]["record"]
cli(
    "edit",
    record["id"],
    "--expect",
    str(record["revision"]),
    "--title",
    record["title"] + " - Scrolls adapter qualification",
    "--body",
    record["body"],
)
shutil.copytree(out / "source", out / "edited", symlinks=True)
shutil.rmtree(out / "edited/docs")
cli("render", "--format", "liskov", "--out", out / "edited/docs")
run([root / "target/release/examples/check_indexes", out / "edited/docs"], text=True)
edited_hashes = {
    p: hashlib.sha256((out / "edited/docs" / p).read_bytes()).hexdigest() for p in files
}
changed = [p for p in files if files[p] != edited_hashes[p]]
assert selected in changed and set(changed) <= {
    selected,
    "raw/README.md",
    "work-notes/README.md",
    "questions/README.md",
    "knowledge/README.md",
    "decisions/README.md",
    "business/README.md",
}, changed
assert all(
    files[p] == edited_hashes[p]
    for p in files
    if p.startswith(("backlog/", "questions/"))
)
logs = []
codes = []
for tree in ["source", "rendered", "edited"]:
    dest = out / tree
    env = {
        **os.environ,
        "LISKOV_WORK_HELPER": str(out / "disabled-beads-helper"),
        "LISKOV_WORK_WORKSPACE": str(out / "disabled-beads-workspace"),
        "PYTHONDONTWRITEBYTECODE": "1",
    }
    env = {k: v for k, v in env.items() if not k.startswith("GIT_")}
    result = subprocess.run(
        ["python3", "scripts/validate_docs.py"],
        cwd=dest,
        env=env,
        capture_output=True,
        text=True,
    )
    log = (result.stdout + result.stderr).replace(str(dest), "<EXPORT>")
    (out / (tree + "-validator.log")).write_text(log)
    logs.append(log)
    codes.append(result.returncode)
assert (
    logs[0] == logs[1] == logs[2] and codes[0] == codes[1] == codes[2]
), "additional validator diagnostics"
report = {
    "source_commit": sha,
    "paths_compared": len(files),
    "markdown": sum(p.endswith(".md") for p in files),
    "assets": sum(not p.endswith(".md") for p in files),
    "byte_mismatches": 0,
    "validator_exit_codes": codes,
    "additional_diagnostics": 0,
    "representable_edit": selected,
    "changed_edit_paths": changed,
    "index_verification": index_result.strip(),
    "frozen_files_verified": sum(
        p.startswith(("backlog/", "questions/")) for p in files
    ),
    "render": rendered,
    "import": imported,
}
run(
    [
        "python3",
        root / "scripts/demo.py",
        "--binary",
        a.binary.resolve(),
        "--store",
        store,
        "--out",
        out / "demo",
    ],
    text=True,
)
(out / "report.json").write_text(json.dumps(report, indent=2) + "\n")
print(
    json.dumps(
        {k: v for k, v in report.items() if k not in ["render", "import"]}, indent=2
    )
)
