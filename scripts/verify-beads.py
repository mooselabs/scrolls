#!/usr/bin/env python3
"""Qualify public Scrolls/Beads interfaces using disposable local databases."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument("--binary", type=Path, default=Path("target/debug/scrolls"))
parser.add_argument("--bd", type=Path, default=Path(".tools/bd"))
parser.add_argument("--dolt", type=Path, default=Path(".tools/dolt"))
args = parser.parse_args()
binary, bd, dolt = (p.resolve(strict=True) for p in (args.binary, args.bd, args.dolt))

with tempfile.TemporaryDirectory(prefix="scrolls-beads-qualification-") as tmp:
    root = Path(tmp)
    home = root / "home"
    home.mkdir()
    repo = root / "work"
    repo.mkdir()
    env = {k: v for k, v in os.environ.items() if not k.startswith(("BEADS_", "BD_", "DOLT_", "GIT_", "GC_"))}
    env.update(HOME=str(home), XDG_CONFIG_HOME=str(home / "config"), XDG_DATA_HOME=str(home / "data"), NO_COLOR="1")

    def run(cmd, *, cwd=repo, expected=0):
        result = subprocess.run([str(x) for x in cmd], cwd=cwd, env=env, input="", text=True, capture_output=True, timeout=120)
        assert result.returncode == expected, (cmd, result.returncode, result.stdout, result.stderr)
        return result.stdout

    def beads(*cmd):
        return json.loads(run([bd, "--json", "--sandbox", *cmd]))

    def scrolls(*cmd, store="knowledge"):
        value = json.loads(run([binary, "--json", "--actor", "fixture", "--role", "human", "--store", root / store, "--dolt", dolt, *cmd]))
        assert value["ok"], value
        return value["data"]

    run(["git", "init", "-q"])
    run(["git", "config", "user.name", "Scrolls Fixture"])
    run(["git", "config", "user.email", "fixture@localhost"])
    run([bd, "init", "--prefix", "fixture", "--skip-hooks", "--skip-agents", "--non-interactive"])
    bead = beads("create", "Implement the decision", "--description", "Disposable integration fixture")
    issue_id = bead["id"]
    beads("update", issue_id, "--metadata", '{"other_integration":{"keep":true}}')
    # Closed beads must remain discoverable for backlinks and cleanup.
    beads("close", issue_id)
    scrolls("init")
    scrolls("beads", "bind", "work", "--directory", repo, "--bd", bd)
    reference = "work:" + issue_id
    evidence = scrolls("new", "evidence", "--title", "Readback", "--body", "Observed outcome")
    knowledge = scrolls("new", "knowledge", "--title", "Decision rationale", "--body", "Current understanding", "--supports", evidence["id"])
    branch = knowledge["branch"]
    scrolls("--proposal", branch, "status", knowledge["id"], "current", "--expect", "1")
    scrolls("--proposal", branch, "link", knowledge["id"], "implemented_by", reference, "--expect", "2")
    assert scrolls("beads", "refs", reference)["documents"] == []
    scrolls("review", "accept", branch)
    packet = scrolls("context", "--bead", reference)
    assert any(x["id"] == knowledge["id"] and x["body"] == "Current understanding" for x in packet["items"])
    refreshed = scrolls("beads", "refresh", "--all")
    assert refreshed["results"][0]["status"] == "closed"
    assert refreshed["results"][0]["observed_at"]
    preview = scrolls("--dry-run", "beads", "publish", "--all")
    assert preview["results"][0]["action"] == "update"
    before = beads("show", issue_id)[0]
    assert not any(k.startswith("scrolls/") for k in before["metadata"])
    first = scrolls("beads", "publish", "--all")
    assert first["results"][0]["verified"]
    after = beads("show", issue_id)[0]
    assert after["status"] == "closed"
    assert after["metadata"]["other_integration"] == {"keep": True}
    key = next(k for k in after["metadata"] if k.startswith("scrolls/"))
    uri = after["metadata"][key]["documents"][0]["uri"]
    assert scrolls("show", uri)["record"]["id"] == knowledge["id"]
    assert "scrolls://" in run([bd, "show", issue_id])
    assert scrolls("beads", "publish", "--all")["results"][0]["action"] == "unchanged"
    # A second independent Scrolls store gets its own metadata namespace.
    scrolls("init", store="second")
    scrolls("beads", "bind", "work", "--directory", repo, "--bd", bd, store="second")
    second = scrolls("new", "evidence", "--title", "Another readback", "--body", "facts", store="second")
    scrolls("link", second["id"], "produced_by", reference, "--expect", "1", store="second")
    scrolls("beads", "publish", "--all", store="second")
    assert len([k for k in beads("show", issue_id)[0]["metadata"] if k.startswith("scrolls/")]) == 2
    unlinked = scrolls("unlink", knowledge["id"], "implemented_by", reference, "--expect", "3")
    scrolls("review", "accept", unlinked["branch"])
    # Simulate lost publication receipts; discover stale projections in Beads.
    sidecar = root / "knowledge" / "beads.json"
    local = json.loads(sidecar.read_text())
    local["publications"] = {}
    local["operations"] = {}
    sidecar.write_text(json.dumps(local))
    assert scrolls("beads", "publish", "--all")["results"][0]["action"] == "remove"
    final = beads("show", issue_id)[0]
    assert key not in final["metadata"]
    assert final["metadata"]["other_integration"] == {"keep": True}
    assert len([k for k in final["metadata"] if k.startswith("scrolls/")]) == 1
    assert final["status"] == "closed"
    print(json.dumps({"ok": True, "beads": run([bd, "version"]).strip(), "checks": ["review isolation", "offline context", "explicit refresh", "publish dry run", "human Beads backlinks", "repeat publish", "multiple Scrolls stores", "closed issue cleanup after receipt loss", "unrelated metadata and work state preserved"]}, indent=2))
