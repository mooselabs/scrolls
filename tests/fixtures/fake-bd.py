#!/usr/bin/env python3
"""Deterministic boundary fixture; no production Beads state is accessed."""
import json
import pathlib
import sys
import time

root = pathlib.Path.cwd()
state_path = root / "fake.json"
state = json.loads(state_path.read_text())
args = sys.argv[1:]
with (root / "calls.jsonl").open("a") as calls:
    calls.write(json.dumps(args) + "\n")
if "version" in args:
    print(json.dumps({"version": "1.3.0"}))
elif "where" in args:
    print(json.dumps({"path": str(root / ".beads")}))
elif "list" in args:
    if state.get("timeout"):
        time.sleep(5)
    if state.get("malformed"):
        print("not JSON")
        sys.exit(0)
    if state.get("offline"):
        sys.exit("fixture unavailable")
    rows = list(state["issues"].values())
    if "--id" in args:
        target = args[args.index("--id") + 1]
        rows = [r for r in rows if r["id"] == target]
    if "--has-metadata-key" in args:
        key = args[args.index("--has-metadata-key") + 1]
        rows = [r for r in rows if key in r.get("metadata", {})]
    print(json.dumps(rows))
elif "update" in args:
    assert "--readonly" not in args
    issue = state["issues"][args[args.index("update") + 1]]
    # Simulate an unrelated writer between the caller's read and atomic mutation.
    issue.setdefault("metadata", {})["concurrent"] = {"preserved": True}
    if "--metadata" in args:
        issue["metadata"].update(json.loads(args[args.index("--metadata") + 1]))
    if "--unset-metadata" in args:
        issue["metadata"].pop(args[args.index("--unset-metadata") + 1], None)
    state_path.write_text(json.dumps(state))
    if state.get("interrupt_after_write"):
        sys.exit("simulated interruption after external commit")
    print(json.dumps([issue]))
else:
    sys.exit("unexpected fake command: " + repr(args))
