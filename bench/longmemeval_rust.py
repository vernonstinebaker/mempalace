#!/usr/bin/env python3
"""
LongMemEval benchmark runner for the Rust MemPalace MCP server.

For each of the 500 questions in longmemeval_s_cleaned.json:
  1. Spawn a fresh Rust MCP process with an ephemeral SQLite palace in /tmp
  2. Add all haystack sessions as drawers (wing=session_id, room=session)
  3. Query with mempalace_search (limit=5)
  4. Check if the ground-truth session_id appears in the top-5 results
  5. Kill the process and delete the temp palace

Reports R@5 (recall at 5) across all question types.

Usage:
  python bench/longmemeval_rust.py [--data /path/to/longmemeval_s_cleaned.json]
                                   [--limit N]   # only run first N questions
                                   [--verbose]
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from collections import defaultdict

RUST_BIN = "/Users/vds/bin/mempalace-mcp"

# ---------------------------------------------------------------------------
# MCP helpers (reused from compare.py)
# ---------------------------------------------------------------------------

INIT_MSG = (
    json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "longmemeval", "version": "1.0"},
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


# ---------------------------------------------------------------------------
# Ephemeral Rust MCP process
# ---------------------------------------------------------------------------


class RustMCP:
    def __init__(self, palace_dir: str):
        self.palace_dir = palace_dir
        env = {**os.environ, "MEMPALACE_PALACE_PATH": palace_dir}
        self.proc = subprocess.Popen(
            [RUST_BIN],
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
        resp = send_recv(self.proc, tool_msg(self._req_id, name, args))
        return resp

    def add_drawer(self, wing: str, room: str, content: str):
        self.call(
            "mempalace_add_drawer", {"wing": wing, "room": room, "content": content}
        )

    def search(self, query: str, limit: int = 5) -> list[str]:
        """Return list of wing names from top-5 results."""
        resp = self.call("mempalace_search", {"query": query, "limit": limit})
        try:
            text = resp["result"]["content"][0]["text"]
            data = json.loads(text)
        except Exception:
            return []
        if isinstance(data, list):
            return [h.get("wing", "") for h in data]
        return []

    def stop(self):
        try:
            self.proc.stdin.close()
            self.proc.terminate()
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()


# ---------------------------------------------------------------------------
# Dataset helpers
# ---------------------------------------------------------------------------


def load_dataset(path: str) -> list[dict]:
    with open(path) as f:
        return json.load(f)


def get_haystack_sessions(item: dict) -> list[tuple[str, str]]:
    """
    Returns [(session_id, session_text), ...] for all haystack sessions.

    Two formats are supported:
      - longmemeval_s/m: haystack_sessions is a list of dicts with
        'session_id' and 'conversation' keys.
      - longmemeval_oracle: haystack_sessions is a list of conversation
        lists (turns), with session IDs in a parallel 'haystack_session_ids' list.
    """
    sessions = []
    raw_sessions = item.get("haystack_sessions", [])
    session_ids = item.get("haystack_session_ids", [])

    for i, sess in enumerate(raw_sessions):
        if isinstance(sess, dict):
            # longmemeval_s / _m format
            sid = sess.get("session_id", f"session_{i}")
            turns = sess.get("conversation", [])
        else:
            # longmemeval_oracle format: sess is a list of turns
            sid = session_ids[i] if i < len(session_ids) else f"session_{i}"
            turns = sess

        parts = []
        for turn in turns:
            role = turn.get("role", "")
            content = turn.get("content", "")
            parts.append(f"{role}: {content}")
        text = "\n".join(parts)
        sessions.append((sid, text))
    return sessions


def get_answer_session_id(item: dict) -> list[str]:
    """The ground-truth session_id(s) that contain the answer."""
    # New format uses answer_session_ids (list); old format uses answer_session_id (str)
    ids = item.get("answer_session_ids", None)
    if ids is not None:
        return ids
    single = item.get("answer_session_id", "")
    return [single] if single else []


def get_question(item: dict) -> str:
    return item.get("question", "")


def get_question_type(item: dict) -> str:
    return item.get("question_type", "unknown")


# ---------------------------------------------------------------------------
# Main benchmark
# ---------------------------------------------------------------------------


def run_benchmark(data_path: str, limit: int | None, verbose: bool):
    dataset = load_dataset(data_path)
    if limit:
        dataset = dataset[:limit]

    total = len(dataset)
    hits = 0
    type_hits = defaultdict(int)
    type_total = defaultdict(int)

    print(f"Running LongMemEval R@5 on {total} questions against Rust MCP...")
    print(f"Binary: {RUST_BIN}")
    print()

    t_start = time.perf_counter()
    scored = 0

    for i, item in enumerate(dataset):
        q = get_question(item)
        answer_sids = get_answer_session_id(item)
        qtype = get_question_type(item)
        sessions = get_haystack_sessions(item)

        # Skip abstention questions (no ground-truth retrieval target)
        if not answer_sids or item["question_id"].endswith("_abs"):
            continue

        scored += 1

        # Create a fresh temp palace dir for this question
        palace_dir = tempfile.mkdtemp(prefix="lme_palace_")
        try:
            mcp = RustMCP(palace_dir)

            # Add all haystack sessions
            for sid, text in sessions:
                # Use session_id as wing, "session" as room
                mcp.add_drawer(wing=sid, room="session", content=text)

            # Query
            top_wings = mcp.search(q, limit=5)
            # Hit if ANY answer session appears in top-5
            hit = any(sid in top_wings for sid in answer_sids)

            mcp.stop()
        except Exception as e:
            if verbose:
                print(f"  [ERROR] q{i}: {e}")
            hit = False
        finally:
            shutil.rmtree(palace_dir, ignore_errors=True)

        if hit:
            hits += 1
            type_hits[qtype] += 1
        type_total[qtype] += 1

        if verbose or scored % 50 == 0:
            elapsed = time.perf_counter() - t_start
            rate = scored / elapsed
            remaining = total - i - 1
            eta = remaining / rate if rate > 0 else 0
            print(
                f"  [{scored:>3}/{total}] hit={hit}  running_r5={hits / scored:.3f}"
                f"  {rate:.1f} q/s  ETA {eta:.0f}s"
                f"{'  q=' + q[:50] if verbose else ''}"
            )

    elapsed = time.perf_counter() - t_start
    r5 = hits / scored if scored > 0 else 0.0

    print()
    print("=" * 60)
    print(f"  LongMemEval R@5 — Rust MemPalace (fixed pooling)")
    print("=" * 60)
    print(f"  Overall R@5: {r5:.4f}  ({hits}/{scored} scored, {total} total)")
    print()
    print(f"  By question type:")
    for qtype in sorted(type_total):
        t = type_total[qtype]
        h = type_hits[qtype]
        print(f"    {qtype:<35}  {h}/{t}  ({h / t:.3f})")
    print()
    print(f"  Total time: {elapsed:.1f}s  ({scored / elapsed:.1f} q/s)")
    print("=" * 60)

    return r5


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--data",
        default="/tmp/longmemeval-data/longmemeval_s_cleaned.json",
        help="Path to longmemeval_s_cleaned.json",
    )
    parser.add_argument(
        "--limit", type=int, default=None, help="Only run first N questions"
    )
    parser.add_argument(
        "--verbose", action="store_true", help="Print every question result"
    )
    args = parser.parse_args()

    if not os.path.exists(args.data):
        print(f"ERROR: dataset not found at {args.data}", file=sys.stderr)
        sys.exit(1)

    run_benchmark(args.data, args.limit, args.verbose)
