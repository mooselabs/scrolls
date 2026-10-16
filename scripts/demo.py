#!/usr/bin/env python3
"""Executable local workflow demonstration, also run in CI."""

import argparse, json, pathlib, subprocess

p = argparse.ArgumentParser()
p.add_argument(
    "--binary", type=pathlib.Path, default=pathlib.Path("target/release/scrolls")
)
p.add_argument("--out", type=pathlib.Path, required=True)
p.add_argument("--store", type=pathlib.Path)
a = p.parse_args()
a.out.mkdir(parents=True, exist_ok=False)
root = pathlib.Path(__file__).resolve().parents[1]
base = [
    str(a.binary.resolve()),
    "--json",
    "--actor",
    "alice",
    "--role",
    "human",
    "--dolt",
    str(root / ".tools/dolt"),
    "--store",
    str(a.store or a.out / "store"),
]
transcript = []


def call(*args):
    result = subprocess.run(
        base + list(args), capture_output=True, text=True, check=True
    )
    v = json.loads(result.stdout)
    transcript.append({"args": args, "result": v})
    return v["data"]


call("init")
entity = call(
    "new",
    "entity",
    "--title",
    "Storage",
    "--field",
    "type=service",
    "--body",
    "The local storage subsystem.",
)
call("review", "accept", entity["branch"])
e = call(
    "new",
    "evidence",
    "--title",
    "Rollback measurement",
    "--body",
    "A killed transaction left zero partial rows.",
    "--about",
    entity["id"],
)
assert e["branch"] == "main"
assert call("show", entity["id"])["record"]["revision"] == 1
k = call(
    "new",
    "knowledge",
    "--title",
    "Atomic local writes",
    "--body",
    "Explicit transactions preserve operation receipts with the write.",
    "--supports",
    e["id"],
    "--about",
    entity["id"],
)
call("--proposal", k["branch"], "status", k["id"], "current", "--expect", "1")
call("review", k["branch"])
call("review", "accept", k["branch"])
packet = call("context", entity["id"], "--budget", "16000")
assert any(r["id"] == k["id"] for r in packet["items"])
q = call(
    "ask",
    "Should we keep the transaction boundary?",
    "--about",
    entity["id"],
    "--owner",
    "alice",
    "--context",
    "Reviewing the rollback result.",
    "--if-yes",
    "Keep it.",
    "--if-no",
    "Investigate alternatives.",
)
answer = call(
    "answer",
    q["id"],
    "Yes; the measured rollback is the required behavior.",
    "--expect",
    "1",
)
call("accept", answer["id"], "--expect", "1", "--expect-question", "2")
call("history", k["id"])
packet = call("prime")
question = next(r for r in packet["items"] if r["id"] == q["id"])
assert question["accepted_answer_id"] == answer["id"]
assert any(r["id"] == answer["id"] for r in packet["items"])
doctrine = call("new", "doctrine", "--title", "Mandatory demo rule", "--body", "Read the complete rule.\n" * 1000, "--field", "glob=demo/**")
call("--proposal", doctrine["branch"], "status", doctrine["id"], "active", "--expect", "1")
call("review", "accept", doctrine["branch"])
packet = call("prime", "--for", "demo/file.rs")
assert packet["over_budget_bytes"] > 0
assert any(r["id"] == doctrine["id"] for r in packet["items"])
assert packet["used_bytes"] == len(json.dumps(packet["items"], ensure_ascii=False, separators=(",", ":")).encode())
call("render", "--format", "liskov", "--out", str(a.out / "rendered"))
(a.out / "transcript.json").write_text(json.dumps(transcript, indent=2) + "\n")
print(
    f"Demonstrated evidence, proposal, review, context, local question, history and render: {a.out}"
)
