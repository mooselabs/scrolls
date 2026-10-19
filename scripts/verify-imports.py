#!/usr/bin/env python3
"""Qualify one pinned PEP snapshot through local and disposable shared stores."""
import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import socket
import shutil
import subprocess
import time
import uuid

PIN = "6068058b896441388bbf22f2607ab681193f6a3d"
ROOT = Path(__file__).resolve().parents[1]
DOLT = ROOT / ".tools/dolt"


def run(args, *, cwd=None, env=None):
    args = list(map(str, args))
    if args[0] == "git":
        args.insert(1, "--no-replace-objects")
        env = {k: v for k, v in (os.environ if env is None else env).items() if not k.startswith("GIT_")}
    return subprocess.run(args, cwd=cwd, env=env, capture_output=True, check=True, timeout=600).stdout


@contextlib.contextmanager
def shared_server(out):
    """Never accept an external endpoint: this fixture owns its entire service."""
    database = "capture_" + uuid.uuid4().hex
    directory = out / "server" / database
    directory.mkdir(parents=True)
    env = {k: v for k, v in os.environ.items() if not k.startswith("DOLT_")}
    env.update(DOLT_ROOT_PATH=str(out / "server-home"), DOLT_DISABLE_EVENT_FLUSH="1")
    # Dolt appends its update notice to stdout; the first line is its version.
    assert run([DOLT, "version"], env=env).decode().splitlines()[:1] == ["dolt version 2.2.0"], "Dolt 2.2.0 required"
    run([DOLT, "init", "--name", "Qualification", "--email", "fixture@localhost"], cwd=directory, env=env)
    initial = json.loads(run([DOLT, "sql", "-r", "json", "-q", "SELECT DOLT_HASHOF('HEAD') AS revision"], cwd=directory, env=env))["rows"][0]["revision"]
    # Account state belongs to this disposable directory, outside versioned rows.
    run([DOLT, "sql", "-q", "CREATE USER fixture IDENTIFIED BY 'test-only'; GRANT ALL ON *.* TO fixture"], cwd=directory, env=env)
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    connection = out / "connection.json"
    connection.write_text(json.dumps({"host": "127.0.0.1", "port": port, "database": database, "user": "fixture", "password": "test-only"}))
    connection.chmod(0o600)
    with (out / "server.log").open("wb") as log:
        server = subprocess.Popen([str(DOLT), "sql-server", "--host", "127.0.0.1", "--port", str(port)], cwd=directory, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 15
            while True:
                assert server.poll() is None, "disposable SQL server exited; inspect server.log"
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                        break
                except OSError:
                    if time.monotonic() >= deadline:
                        raise RuntimeError("disposable SQL server did not become ready")
                    time.sleep(0.05)
            # Unique database name plus expected initial revision prevents a port
            # allocation race from making this fixture initialize another store.
            yield connection, initial
        finally:
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=10)


def qualify(repo, binary, out, connection=None, expected_initial=None):
    timings = {}
    base = [str(binary), "--json", "--actor", "qualification", "--role", "human", "--dolt", str(DOLT)]
    if connection is not None:
        base += ["--connection", str(connection)]

    def call(*args, reader=False):
        client = out / ("reader" if reader and connection is not None else "store")
        value = json.loads(run(base + ["--store", str(client)] + list(args)))
        assert value["ok"], value
        return value["data"]

    start = time.monotonic()
    if connection is None:
        call("init")
    else:
        plan = call("init", "--dry-run")
        assert plan["base_revision"] == expected_initial
        call("init", "--op-id", "initialization", "--expect-base", plan["base_revision"], "--expect-schema", plan["ontology_digest"])
    timings["initialize"] = time.monotonic() - start
    plan = out / "peps.plan.json"
    start = time.monotonic()
    planned = call("import", "plan", "git", repo, "--root", "peps", "--rev", PIN, "--source", "python.peps", "--profile", "pep", "--out", plan)
    timings["plan"] = time.monotonic() - start
    coverage = planned["coverage"]
    for key, value in {"documents": 744, "files": 820, "header_references": 113, "edges": 46, "unmapped_references": 40, "unresolved_references": 0}.items():
        assert coverage[key] == value, (key, coverage[key])
    start = time.monotonic()
    call("import", "apply", plan, "--op-id", "peps-initial", "--dry-run")
    timings["validate"] = time.monotonic() - start
    start = time.monotonic()
    result = call("import", "apply", plan, "--op-id", "peps-initial")
    timings["apply"] = time.monotonic() - start
    start = time.monotonic()
    assert result == call("import", "apply", plan, "--op-id", "peps-initial", reader=True)
    timings["replay"] = time.monotonic() - start
    record = call("show", "PEP-0008", "--provenance", reader=True)
    assert record["record"]["kind"] == "evidence"
    assert record["provenance"]["membership"]["metadata"]["pep"] == "8"
    assert call("lint", reader=True)["errors"] == []
    doctor = call("doctor", reader=True)
    assert doctor["import_health"]["files"] == 820
    export = out / "export"
    start = time.monotonic()
    call("render", "--out", export, reader=True)
    timings["export"] = time.monotonic() - start
    entries = run(["git", "-C", repo, "ls-tree", "--full-tree", "-r", "-z", PIN + ":peps"]).split(b"\0")
    checks = []
    for entry in filter(None, entries):
        header, path = entry.split(b"\t", 1)
        mode, kind, oid = header.decode().split()
        assert kind == "blob"
        raw = run(["git", "-C", repo, "cat-file", "blob", oid])
        file = export / "sources/python.peps" / path.decode()
        assert file.read_bytes() == raw, path
        assert bool(file.stat().st_mode & 0o111) == (mode == "100755"), path
        checks.append([path.decode(), mode, oid, hashlib.sha256(raw).hexdigest()])
    assert len(checks) == coverage["files"], "oracle must check every included source file"
    report = {"backend": "local" if connection is None else "shared", "source_commit": PIN,
              "tested_commit": run(["git", "-C", ROOT, "rev-parse", "HEAD"]).decode().strip(),
              "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "coverage": coverage,
              "candidate_digest": result["candidate_digest"],
              "source_inventory_digest": hashlib.sha256(json.dumps(checks, separators=(",", ":")).encode()).hexdigest(),
              "byte_mismatches": 0, "mode_mismatches": 0, "timings_seconds": timings,
              "shared_query_bytes": doctor.get("shared_query_bytes"), "source_bytes": doctor["import_health"]["bytes"]}
    (out / "backend-report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2), flush=True)
    return report


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("repository", type=Path)
    parser.add_argument("--binary", type=Path, default=Path("target/release/scrolls"))
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    binary_source = args.binary.resolve(strict=True)
    binary = args.out.resolve() / "scrolls-tested"
    shutil.copyfile(binary_source, binary)
    binary.chmod(0o700)
    repo = args.repository.resolve(strict=True)
    local = qualify(repo, binary, args.out)
    shared_out = args.out / "shared"
    shared_out.mkdir()
    with shared_server(shared_out) as (connection, initial):
        shared = qualify(repo, binary, shared_out, connection, initial)
    assert local["source_inventory_digest"] == shared["source_inventory_digest"]
    assert local["candidate_digest"] == shared["candidate_digest"]
    report = dict(local, ok=True, backends={"local": local, "shared": shared})
    (args.out / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"ok": True, "backends": ["local", "shared"], "source_inventory_digest": local["source_inventory_digest"]}), flush=True)


if __name__ == "__main__":
    main()
