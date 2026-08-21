#!/usr/bin/env python3
"""
LoCoMo retrieval harness for Rust MemPalace.

Indexes each conversation turn as one drawer (wing=conversation_id,
room=session_N, content=turn text). For each QA pair, runs mempalace_search
and scores R@5 / R@10 by whether the gold answer string appears in a hit.

Data:
  Default: a tiny synthetic set so the script runs end-to-end without a
  download. Official LoCoMo JSON (list of {conversation, qa}) via --data.

Download (not vendored):
  https://github.com/snap-research/locomo  (or the paper's release JSON)

Usage:
  python bench/locomo_rust.py
  python bench/locomo_rust.py --data /path/to/locomo.json --limit 20
  MEMPALACE_BIN=./target/release/mempalace-mcp python bench/locomo_rust.py

Deterministic: questions are scored in sorted order of (conversation_id, question).
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def rust_bin() -> str:
    env = os.environ.get("MEMPALACE_BIN")
    if env:
        return env
    candidate = ROOT / "target" / "release" / "mempalace-mcp"
    if candidate.is_file():
        return str(candidate)
    which = shutil.which("mempalace-mcp")
    if which:
        return which
    home = Path.home() / "bin" / "mempalace-mcp"
    return str(home)


INIT_MSG = (
    json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "locomo", "version": "1.0"},
            },
        }
    )
    + "\n"
)

INITIALIZED_MSG = (
    json.dumps(
        {
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {},
        }
    )
    + "\n"
)


def send_recv(proc, msg: str) -> dict:
    proc.stdin.write(msg.encode())
    proc.stdin.flush()
    while True:
        if proc.poll() is not None:
            raise RuntimeError(f"Process exited with code {proc.returncode}")
        line = proc.stdout.readline()
        if not line:
            raise RuntimeError("EOF from process stdout")
        line = line.decode(errors="replace").strip()
        if not line:
            continue
        try:
            return json.loads(line)
        except json.JSONDecodeError:
            continue


def tool_msg(req_id: int, name: str, args: dict) -> str:
    return (
        json.dumps(
            {
                "jsonrpc": "2.0",
                "id": req_id,
                "method": "tools/call",
                "params": {"name": name, "arguments": args},
            }
        )
        + "\n"
    )


class RustMCP:
    def __init__(self, palace_dir: str, binary: str):
        env = {**os.environ, "MEMPALACE_PALACE_PATH": palace_dir}
        self.proc = subprocess.Popen(
            [binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            env=env,
        )
        resp = send_recv(self.proc, INIT_MSG)
        if "error" in resp:
            raise RuntimeError(f"initialize failed: {resp['error']}")
        self.proc.stdin.write(INITIALIZED_MSG.encode())
        self.proc.stdin.flush()
        self._req_id = 10

    def call(self, name: str, args: dict) -> dict:
        self._req_id += 1
        return send_recv(self.proc, tool_msg(self._req_id, name, args))

    def add_drawer(self, wing: str, room: str, content: str):
        self.call(
            "mempalace_add_drawer",
            {"wing": wing, "room": room, "content": content},
        )

    def search_contents(self, query: str, limit: int) -> list[str]:
        resp = self.call("mempalace_search", {"query": query, "limit": limit})
        try:
            text = resp["result"]["content"][0]["text"]
            data = json.loads(text)
        except Exception:
            return []
        hits = data if isinstance(data, list) else data.get("results", [])
        return [h.get("content", "") for h in hits]

    def stop(self):
        try:
            self.proc.stdin.close()
            self.proc.terminate()
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()


SYNTHETIC = [
    {
        "conversation_id": "syn_alice",
        "turns": [
            {"session": "s1", "text": "Alice lives in Berlin and commutes by train."},
            {"session": "s1", "text": "Bob prefers dark mode in vim."},
            {"session": "s2", "text": "Alice adopted a cat named Miso last spring."},
        ],
        "qa": [
            {"question": "Where does Alice live?", "answer": "Berlin"},
            {"question": "What is Alice's cat named?", "answer": "Miso"},
        ],
    }
]


def iter_official_locomo(raw) -> list[dict]:
    """Best-effort parse of LoCoMo release JSON (list of samples)."""
    items = []
    if isinstance(raw, dict) and "data" in raw:
        raw = raw["data"]
    if not isinstance(raw, list):
        return items
    for i, sample in enumerate(raw):
        conv = sample.get("conversation") or sample
        cid = str(sample.get("sample_id") or sample.get("conversation_id") or f"locomo_{i}")
        turns = []
        if isinstance(conv, dict):
            for key, val in sorted(conv.items()):
                if not str(key).startswith("session"):
                    continue
                session_turns = val
                if isinstance(val, dict):
                    session_turns = val.get("turns") or val.get("conversation") or []
                if not isinstance(session_turns, list):
                    continue
                for t in session_turns:
                    text = t.get("text") or t.get("content") or ""
                    if text:
                        turns.append({"session": str(key), "text": text})
        qa = []
        for q in sample.get("qa") or []:
            ans = q.get("answer") or q.get("final_answer") or ""
            if isinstance(ans, list):
                ans = " ".join(str(a) for a in ans)
            question = q.get("question") or ""
            if question and ans:
                qa.append({"question": question, "answer": str(ans)})
        if turns and qa:
            items.append({"conversation_id": cid, "turns": turns, "qa": qa})
    return items


def load_items(path: str | None) -> list[dict]:
    if not path:
        return SYNTHETIC
    with open(path) as f:
        raw = json.load(f)
    parsed = iter_official_locomo(raw)
    return parsed or SYNTHETIC


def answer_in_hits(answer: str, contents: list[str]) -> bool:
    needle = answer.strip().lower()
    if not needle:
        return False
    return any(needle in (c or "").lower() for c in contents)


def run(data_path: str | None, limit: int | None, verbose: bool) -> dict:
    binary = rust_bin()
    items = load_items(data_path)
    tasks = []
    for item in items:
        for qa in item["qa"]:
            tasks.append((item, qa))
    tasks.sort(key=lambda t: (t[0]["conversation_id"], t[1]["question"]))
    if limit:
        tasks = tasks[:limit]

    hits5 = 0
    hits10 = 0
    scored = 0
    t0 = time.perf_counter()
    print(f"LoCoMo harness  bin={binary}  questions={len(tasks)}  synthetic={not data_path}")
    for item, qa in tasks:
        scored += 1
        palace = tempfile.mkdtemp(prefix="locomo_palace_")
        try:
            mcp = RustMCP(palace, binary)
            for idx, turn in enumerate(item["turns"]):
                mcp.add_drawer(
                    wing=item["conversation_id"],
                    room=f"{turn['session']}_{idx}",
                    content=turn["text"],
                )
            c5 = mcp.search_contents(qa["question"], 5)
            c10 = mcp.search_contents(qa["question"], 10)
            mcp.stop()
        except Exception as e:
            if verbose:
                print(f"  error: {e}")
            c5, c10 = [], []
        finally:
            shutil.rmtree(palace, ignore_errors=True)
        h5 = answer_in_hits(qa["answer"], c5)
        h10 = answer_in_hits(qa["answer"], c10)
        hits5 += int(h5)
        hits10 += int(h10)
        if verbose:
            print(f"  [{scored}] r5={h5} r10={h10}  q={qa['question'][:60]}")

    elapsed = time.perf_counter() - t0
    summary = {
        "n_scored": scored,
        "r5": hits5 / scored if scored else 0.0,
        "r10": hits10 / scored if scored else 0.0,
        "hits5": hits5,
        "hits10": hits10,
        "seconds": elapsed,
        "synthetic": not bool(data_path),
    }
    print(
        json.dumps(
            {
                **summary,
                "r5_pct": round(summary["r5"] * 100, 2),
                "r10_pct": round(summary["r10"] * 100, 2),
            },
            indent=2,
        )
    )
    return summary


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--data", help="Path to official LoCoMo JSON (optional)")
    p.add_argument("--limit", type=int, default=None)
    p.add_argument("--verbose", action="store_true")
    args = p.parse_args()
    run(args.data, args.limit, args.verbose)


if __name__ == "__main__":
    sys.exit(main() or 0)
