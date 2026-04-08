#!/usr/bin/env python3
"""
MemPalace MCP Benchmark: Python (ChromaDB) vs Rust (SQLite + sqlite-vec)
========================================================================
Measures:
  1. Startup time  — time from process spawn to first valid MCP response
  2. Per-tool latency — median + p95 over N_REPS repetitions
  3. Search quality  — top-5 results for shared queries (manual inspection)

Usage:
  python bench/compare.py [--reps N] [--no-search-quality]
"""

import argparse
import json
import os
import subprocess
import sys
import time
from statistics import median, quantiles

# ---------------------------------------------------------------------------
# Server configurations
# ---------------------------------------------------------------------------

PYTHON_CMD = [
    "/Volumes/EnvoyUltra/Programming/mempalace/.venv/bin/python",
    "-m",
    "mempalace.mcp_server",
]
PYTHON_ENV = {
    **os.environ,
    "MEMPALACE_PALACE_PATH": "/Volumes/EnvoyUltra/Programming/mempalace/palace",
}

RUST_CMD = ["/Users/vds/bin/mempalace-mcp"]
RUST_ENV = {
    **os.environ,
    "MEMPALACE_PALACE_PATH": "/Volumes/EnvoyUltra/Programming/mempalace",
}

N_REPS = 5  # repetitions per tool call (overridden by --reps)

SEARCH_QUERIES = [
    "Rust performance optimization",
    "SQLite vector search embeddings",
    "family kids children",
    "memory palace ChromaDB migration",
    "agent diary entries AAAK",
]

# ---------------------------------------------------------------------------
# MCP client helpers
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
                "clientInfo": {"name": "bench", "version": "1.0"},
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


def send_recv(proc, msg: str) -> tuple[dict, float]:
    """Send a JSON-RPC message and return (parsed_response, elapsed_seconds).
    Skips any non-JSON lines (e.g. log output on stdout) and raises if the
    process has died or no response is received within a reasonable time.
    """
    t0 = time.perf_counter()
    proc.stdin.write(msg.encode())
    proc.stdin.flush()
    while True:
        if proc.poll() is not None:
            raise RuntimeError(f"Process exited with code {proc.returncode}")
        line = proc.stdout.readline()
        if not line:
            raise RuntimeError("EOF from process stdout — process likely crashed")
        line = line.decode(errors="replace").strip()
        if not line:
            continue
        try:
            parsed = json.loads(line)
        except json.JSONDecodeError:
            # Log line leaked to stdout — skip it
            continue
        elapsed = time.perf_counter() - t0
        return parsed, elapsed


def tool_call_msg(req_id: int, name: str, args: dict) -> str:
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


def tools_list_msg(req_id: int) -> str:
    return (
        json.dumps(
            {"jsonrpc": "2.0", "id": req_id, "method": "tools/list", "params": {}}
        )
        + "\n"
    )


# ---------------------------------------------------------------------------
# Benchmark runner
# ---------------------------------------------------------------------------


class ServerBench:
    def __init__(self, label: str, cmd: list, env: dict):
        self.label = label
        self.cmd = cmd
        self.env = env
        self.proc = None

    def start(self) -> float:
        """Spawn the process and measure time to first valid response (startup)."""
        t0 = time.perf_counter()
        self.proc = subprocess.Popen(
            self.cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=self.env,
        )
        # Send initialize and measure until response
        resp, _ = send_recv(self.proc, INIT_MSG)
        startup = time.perf_counter() - t0
        # Send initialized notification (no response expected)
        self.proc.stdin.write(INITIALIZED_MSG.encode())
        self.proc.stdin.flush()
        if "error" in resp:
            raise RuntimeError(f"{self.label} initialize failed: {resp['error']}")
        return startup

    def stop(self):
        if self.proc:
            try:
                self.proc.stdin.close()
                self.proc.terminate()
                self.proc.wait(timeout=5)
            except Exception:
                self.proc.kill()
            self.proc = None

    def measure(self, name: str, args: dict, req_id: int = 99) -> float:
        """Call one tool once and return elapsed time (seconds)."""
        msg = tool_call_msg(req_id, name, args)
        resp, elapsed = send_recv(self.proc, msg)
        if "error" in resp:
            print(
                f"  [WARN] {self.label} {name} returned error: {resp['error']}",
                file=sys.stderr,
            )
        return elapsed

    def measure_n(self, name: str, args: dict, n: int) -> list[float]:
        """Call one tool N times and return list of elapsed times."""
        return [self.measure(name, args, req_id=100 + i) for i in range(n)]

    def search_results(self, query: str, req_id: int = 200) -> list[dict]:
        """Run a search and return a normalised result list for quality inspection.

        Handles two response shapes:
          Python: {"results": [{"text": ..., "similarity": ..., "wing": ..., "room": ...}, ...]}
          Rust:   [{"content": ..., "rank": ..., "wing": ..., "room": ...}, ...]
        """
        msg = tool_call_msg(req_id, "mempalace_search", {"query": query, "limit": 5})
        resp, _ = send_recv(self.proc, msg)
        try:
            text = resp["result"]["content"][0]["text"]
            data = json.loads(text)
        except Exception:
            return []

        # Normalise to list of {"snippet": str, "score": float, "wing": str, "room": str}
        if isinstance(data, list):
            # Rust format
            raw = data
            return [
                {
                    "snippet": str(h.get("content", ""))[:60].replace("\n", " "),
                    "score": round(h.get("rank", 0.0), 4),
                    "wing": h.get("wing", ""),
                    "room": h.get("room", ""),
                }
                for h in raw
            ]
        elif isinstance(data, dict):
            # Python format
            raw = data.get("results", [])
            return [
                {
                    "snippet": str(
                        h.get("text", h.get("content", h.get("document", "")))
                    )[:60].replace("\n", " "),
                    "score": round(
                        float(
                            h.get("similarity", h.get("score", h.get("distance", 0.0)))
                        ),
                        4,
                    ),
                    "wing": h.get("wing", ""),
                    "room": h.get("room", ""),
                }
                for h in raw
            ]
        return []


# ---------------------------------------------------------------------------
# Formatting helpers
# ---------------------------------------------------------------------------


def fmt_ms(secs: float) -> str:
    return f"{secs * 1000:.1f} ms"


def pct95(data: list[float]) -> float:
    if len(data) < 2:
        return data[0] if data else 0.0
    return quantiles(data, n=100)[94]


def print_row(label: str, times: list[float]):
    med = median(times)
    p95 = pct95(times)
    print(f"  {label:<35}  median={fmt_ms(med):>10}  p95={fmt_ms(p95):>10}")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def run_bench(n_reps: int, show_search_quality: bool):
    py = ServerBench("Python", PYTHON_CMD, PYTHON_ENV)
    rs = ServerBench("Rust  ", RUST_CMD, RUST_ENV)

    print("=" * 70)
    print("  MemPalace MCP Benchmark")
    print(f"  Repetitions per tool: {n_reps}")
    print("=" * 70)

    # ── Startup ──────────────────────────────────────────────────────────────
    print("\n[1] STARTUP TIME (spawn → first response)\n")

    py_startups = []
    rs_startups = []
    for i in range(n_reps):
        print(f"  run {i + 1}/{n_reps}...", end=" ", flush=True)
        t = py.start()
        py_startups.append(t)
        py.stop()
        print(f"Python={fmt_ms(t)}", end="  ", flush=True)
        t = rs.start()
        rs_startups.append(t)
        rs.stop()
        print(f"Rust={fmt_ms(t)}")

    print()
    print(
        f"  {'Python':<10}  median={fmt_ms(median(py_startups)):>10}  p95={fmt_ms(pct95(py_startups)):>10}"
    )
    print(
        f"  {'Rust':<10}  median={fmt_ms(median(rs_startups)):>10}  p95={fmt_ms(pct95(rs_startups)):>10}"
    )
    speedup = median(py_startups) / median(rs_startups)
    print(f"\n  Rust is {speedup:.1f}x faster to start\n")

    # ── Per-tool latency ─────────────────────────────────────────────────────
    print("[2] PER-TOOL LATENCY\n")

    TOOL_CASES = [
        ("tools/list", {}, False),
        ("mempalace_status", {}, True),
        ("mempalace_list_wings", {}, True),
        ("mempalace_get_taxonomy", {}, True),
        ("mempalace_search", {"query": "SQLite performance"}, True),
        ("mempalace_search", {"query": "family children"}, True),
        ("mempalace_kg_query", {"entity": "Alice"}, True),
        ("mempalace_kg_stats", {}, True),
        ("mempalace_diary_read", {"agent_name": "opencode", "last_n": 5}, True),
    ]

    py.start()
    rs.start()

    results_table = []

    for tool_name, args, is_tool_call in TOOL_CASES:
        if is_tool_call:
            py_times = py.measure_n(tool_name, args, n_reps)
            rs_times = rs.measure_n(tool_name, args, n_reps)
        else:
            # tools/list via direct method
            py_times = []
            rs_times = []
            for i in range(n_reps):
                msg = tools_list_msg(300 + i)
                _, e = send_recv(py.proc, msg)
                py_times.append(e)
                _, e = send_recv(rs.proc, msg)
                rs_times.append(e)

        py_med = median(py_times)
        rs_med = median(rs_times)
        speedup = py_med / rs_med if rs_med > 0 else float("inf")
        results_table.append((tool_name, py_med, rs_med, speedup))

    py.stop()
    rs.stop()

    # Print table
    print(f"  {'Tool':<35}  {'Python':>12}  {'Rust':>12}  {'Speedup':>8}")
    print(f"  {'-' * 35}  {'-' * 12}  {'-' * 12}  {'-' * 8}")
    for tool_name, py_med, rs_med, speedup in results_table:
        winner = "←" if rs_med < py_med else "→"
        print(
            f"  {tool_name:<35}  {fmt_ms(py_med):>12}  {fmt_ms(rs_med):>12}  {speedup:>6.1f}x {winner}"
        )

    overall_py = sum(r[1] for r in results_table)
    overall_rs = sum(r[2] for r in results_table)
    print(
        f"\n  {'TOTAL (sum of medians)':<35}  {fmt_ms(overall_py):>12}  {fmt_ms(overall_rs):>12}  {overall_py / overall_rs:>6.1f}x"
    )

    # ── Search quality ────────────────────────────────────────────────────────
    if show_search_quality:
        print("\n[3] SEARCH QUALITY (top-5 results per query)\n")
        print("  NOTE: Python uses old ChromaDB palace (~19k drawers)")
        print("        Rust   uses current SQLite palace (~23k drawers)\n")

        py.start()
        rs.start()

        for q in SEARCH_QUERIES:
            print(f'  Query: "{q}"')
            py_res = py.search_results(q)
            rs_res = rs.search_results(q)

            print(f"  {'Python results':<52}  {'Rust results'}")
            print(f"  {'-' * 52}  {'-' * 52}")
            max_len = max(len(py_res), len(rs_res))
            for i in range(max_len):
                py_hit = py_res[i] if i < len(py_res) else {}
                rs_hit = rs_res[i] if i < len(rs_res) else {}
                py_score = py_hit.get("score", "?")
                rs_score = rs_hit.get("score", "?")
                py_snip = py_hit.get("snippet", "")
                rs_snip = rs_hit.get("snippet", "")
                print(
                    f"  [{i + 1}] {str(py_score):<7} {py_snip:<44}  [{i + 1}] {str(rs_score):<7} {rs_snip}"
                )
            print()

        py.stop()
        rs.stop()

    print("=" * 70)
    print("  Done.")
    print("=" * 70)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="MemPalace MCP benchmark")
    parser.add_argument("--reps", type=int, default=N_REPS, help="Repetitions per tool")
    parser.add_argument(
        "--no-search-quality", action="store_true", help="Skip search quality section"
    )
    args = parser.parse_args()
    run_bench(args.reps, not args.no_search_quality)
