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
binary_digest = hashlib.sha256(a.binary.resolve().read_bytes()).hexdigest()
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
print("Pinned corpus imported into the private store", flush=True)
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
manifest = json.loads((out / "rendered/docs/SCROLLS-MANIFEST.json").read_text())
graph = json.loads((out / "rendered/docs/scrolls/graph.json").read_text())
imported_entities = {f"scrolls/entity/{rid}.md" for rid, record in graph["records"].items() if record["path"] is None}
sidecars = {"scrolls/graph.json", "scrolls/README.md"}
assert set(actual) - set(files) == imported_entities | sidecars
assert all(actual.get(path) == digest for path, digest in files.items()), "path/byte roundtrip mismatch"
assert set(manifest["records"]) == set(graph["records"])
assert all((out / "rendered/docs" / path).is_file() for path in manifest["records"].values())
print(f"Pristine export preserves {len(files)} source paths", flush=True)
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
# Author every native kind after the imported edit, with stable IDs and both
# manifest and non-manifest entities. These writes target only this private store.
def data(*args):
    return cli(*args)["data"]


def create(kind, title, body="Local qualification record.", **options):
    args = ["new", kind, "--title", title, "--body", body]
    for key, value in options.items():
        args.extend(["--" + key.replace("_", "-"), value])
    result = data(*args)
    if result["branch"] != "main":
        data("--role", "human", "review", "accept", result["branch"])
    return result["id"]


native = {}
native["entity"] = create("entity", "Qualification service")
repo_id = next(rid for rid, r in graph["records"].items() if r["fields"].get("type") == "repo")
native["evidence"] = create("evidence", "Native evidence", about=repo_id)
for kind in ["knowledge", "primer", "decision"]:
    # Separate evidence preserves the provenance relation's existing two-record policy.
    evidence = create("evidence", "Support for " + kind)
    native["support_" + kind] = evidence
    native[kind] = create(kind, "Native " + kind, body=f"[Evidence]({evidence}#details)", supports=evidence, about=native["entity"])
for kind in ["doctrine", "collection", "note"]:
    native[kind] = create(kind, "Native " + kind, about=native["entity"])
q = data("ask", "Local qualification question", "--about", native["entity"], "--context", f"[Knowledge]({native['knowledge']})")
assert q["branch"] == "main"
native["question"] = q["id"]
answer = data("--role", "human", "answer", q["id"], "Accepted local answer", "--expect", "1")
native["answer"] = answer["id"]
data("--role", "human", "accept", answer["id"], "--expect", "1", "--expect-question", "2")
# Add a supplemental assertion owned by an imported record; preserve its bytes.
reference = data("link", record["id"], "related", native["entity"], "--expect", "2")
assert reference["branch"] == "main"
shutil.copytree(out / "source", out / "native", symlinks=True)
shutil.rmtree(out / "native/docs")
data("render", "--format", "liskov", "--out", out / "native/docs")
native_manifest = json.loads((out / "native/docs/SCROLLS-MANIFEST.json").read_text())
native_graph = json.loads((out / "native/docs/scrolls/graph.json").read_text())
assert set(native_manifest["records"]) == set(native_graph["records"])
expected_additions = imported_entities | sidecars | {native_manifest["records"][rid] for rid in native.values()}
native_paths = {p.relative_to(out / "native/docs").as_posix() for p in (out / "native/docs").rglob("*") if p.is_file() and p.name not in ["RENDERED", "SCROLLS-MANIFEST.json"]}
assert native_paths - set(files) == expected_additions, "unexpected native paths"
assert set(files) <= native_paths
allowed_changed = {selected, "raw/README.md", "knowledge/README.md", "work-notes/README.md"}
native_changed = {p for p in files if hashlib.sha256((out / "native/docs" / p).read_bytes()).hexdigest() != files[p]}
assert native_changed <= allowed_changed, native_changed
assert all(hashlib.sha256((out / "native/docs" / p).read_bytes()).hexdigest() == files[p] for p in files if p.startswith(("backlog/", "questions/")))
index_native = run([root / "target/release/examples/check_indexes", out / "native/docs"], text=True)
supplement = (out / "native/docs/scrolls/README.md").read_text()
for rid, r in native_graph["records"].items():
    if r["path"] is None:
        assert f"[{rid}]" in supplement
    if r["kind"] == "decision" and r["original"] is not None:
        alias = next((alias for alias in r["aliases"] if alias.startswith("ADR-")), None)
        if alias:
            assert r["metadata"]["number"] == alias[4:]
for label, rid in native.items():
    path = native_manifest["records"][rid]
    assert rid in path
    if label in ["knowledge", "primer"]:
        assert path == f"knowledge/{rid}.md"
    elif label in ["entity", "decision", "doctrine", "collection", "question", "answer"]:
        assert path == f"scrolls/{label}/{rid}.md"
    elif label == "note":
        date = native_graph["records"][rid]["created"][:10]
        assert path == f"work-notes/sessions/{date}/{date}-{rid}.md"
    else:
        date = native_graph["records"][rid]["created"][:10]
        assert path == f"raw/{date}-{rid}.md"
assert "metadata" not in native_graph["records"][native["decision"]]
assert native_graph["records"][native["question"]]["status"] == "answered"
assert native_graph["records"][native["answer"]]["status"] == "accepted"
# Independently compare with the pinned Liskov renderer even when an unrelated
# baseline validator error prevents its own freshness stage from running.
pinned_index_checks = {}
for tree in ["source", "rendered", "edited", "native"]:
    dest = out / tree
    pinned_index_checks[tree] = run(
        ["python3", "-c", r"""
import os, pathlib, sys
sys.path.insert(0, str(pathlib.Path('scripts').resolve()))
import docs_model as dm
root = pathlib.Path('docs').resolve()
def exempt(path):
    parts = pathlib.Path(path).relative_to(root).parts
    return ('templates' in parts[:-1] or path.endswith('.template.md')
            or (parts[0] == 'raw' and 'imported' in parts[:-1]))
corpus = dm.Corpus.load(str(root), is_exempt=exempt, ledger=False)
for readme, (subdir, dates) in dm.GENERATED_INDEXES.items():
    expected = dm.render_directory_index(corpus, readme, subdir, dates)
    actual = (root / readme).read_text()
    begin = actual.index(dm.INDEX_BEGIN)
    end = actual.index(dm.INDEX_END) + len(dm.INDEX_END)
    assert actual[begin:end] == expected, readme
print(f'{len(dm.GENERATED_INDEXES)} indexes match pinned Liskov renderer')
"""],
        cwd=dest,
        env={**{k: v for k, v in os.environ.items() if not k.startswith("GIT_")},
             "PYTHONDONTWRITEBYTECODE": "1",
             "LISKOV_WORK_HELPER": str(out / "disabled-beads-helper"),
             "LISKOV_WORK_WORKSPACE": str(out / "disabled-beads-workspace")},
        text=True,
    ).strip()
print("Native paths, graph, frozen history and pinned indexes verified", flush=True)
logs = []
codes = []
for tree in ["source", "rendered", "edited", "native"]:
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
    all(log == logs[0] for log in logs) and all(code == codes[0] for code in codes)
), "additional validator diagnostics"
print("Pinned validator: no additional diagnostics", flush=True)
report = {
    "source_commit": sha,
    "scrolls_commit": run(["git", "-C", root, "rev-parse", "HEAD"], text=True).strip(),
    "scrolls_binary_sha256": binary_digest,
    "scrolls_worktree_status": run(["git", "-C", root, "status", "--porcelain"], text=True).strip(),
    "native_records_authored": len(native),
    "native_additional_paths": sorted(expected_additions),
    "native_changed_paths": sorted(native_changed),
    "native_index_verification": index_native.strip(),
    "pinned_index_verification": pinned_index_checks,
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
assert hashlib.sha256(a.binary.resolve().read_bytes()).hexdigest() == binary_digest, "binary changed during qualification"
(out / "report.json").write_text(json.dumps(report, indent=2) + "\n")
print(
    json.dumps(
        {k: v for k, v in report.items() if k not in ["render", "import"]}, indent=2
    )
)
