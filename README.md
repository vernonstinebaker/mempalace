# MemPalace MCP Server

A single-binary MCP (Model Context Protocol) server providing persistent, semantic memory for AI assistants. Built in Rust with zero external runtime dependencies.

## Features

- **Semantic search** — ONNX-based `all-MiniLM-L6-v2` embeddings (384d) via pure-Rust `tract`
- **Hybrid retrieval** — vector cosine search (sqlite-vec) + FTS5 BM25 fused via Reciprocal Rank Fusion (RRF)
- **Structured memory** — drawers organized into wings (projects) and rooms (aspects)
- **Knowledge graph** — typed subject/predicate/object triples with temporal validity
- **Agent diaries** — per-agent AAAK-compressed diary entries
- **Graph traversal** — cross-wing tunnel discovery and room-to-room walks
- **Single SQLite file** — everything in one `palace.db`, WAL mode, no servers

## Benchmark

Evaluated on [LongMemEval](https://github.com/xiaowu0162/LongMemEval) (500-question episodic memory benchmark):

| Mode | R@5 |
|---|---|
| Rust MemPalace — user-turns-only | **94.0%** (442/470 scored) |
| Rust MemPalace — all turns | 91.7% (431/470) |

By category (user-turns-only):

| Category | R@5 |
|---|---|
| knowledge-update | 98.6% |
| single-session-assistant | 96.4% |
| multi-session | 95.0% |
| single-session-preference | 90.0% |
| single-session-user | 90.6% |
| temporal-reasoning | 92.1% |

## Building

```sh
cargo build --release
# Binary: target/release/mempalace-mcp
```

Requires Rust 1.75+. All dependencies are vendored/bundled — no system SQLite, no Python, no Node.js.

The ONNX model and tokenizer are embedded in the binary at compile time via `include_bytes!`.

## Installation

```sh
# Copy or symlink the binary somewhere on your PATH
ln -s /path/to/target/release/mempalace-mcp ~/bin/mempalace-mcp

# Create the palace directory
mkdir -p ~/mempalace
```

## Configuration

Set one environment variable:

```sh
MEMPALACE_PALACE_PATH=/path/to/palace/directory
```

The binary appends `/palace.db` to this path. The directory is created automatically if it doesn't exist.

## MCP Client Setup

### OpenCode (`~/.config/opencode/config.json`)

```json
{
  "mcp": {
    "mempalace": {
      "type": "local",
      "command": "/path/to/mempalace-mcp",
      "args": [],
      "env": {
        "MEMPALACE_PALACE_PATH": "/path/to/palace/directory"
      }
    }
  }
}
```

### OpenCat (configure via the MCP Servers UI)

- **Type:** stdio
- **Command:** `/path/to/mempalace-mcp`
- **Env:** `MEMPALACE_PALACE_PATH=/path/to/palace/directory`

## MCP Tools

| Tool | Description |
|---|---|
| `mempalace_add_drawer` | Store verbatim content |
| `mempalace_search` | Hybrid semantic + keyword search |
| `mempalace_check_duplicate` | Vector-based dedup before filing |
| `mempalace_delete_drawer` | Remove a drawer by ID |
| `mempalace_list_wings` | All wings with drawer counts |
| `mempalace_list_rooms` | Rooms within a wing |
| `mempalace_get_taxonomy` | Full wing→room→count tree |
| `mempalace_kg_add` | Add a knowledge graph triple |
| `mempalace_kg_query` | Query an entity's relationships |
| `mempalace_kg_invalidate` | Mark a triple as expired |
| `mempalace_kg_timeline` | Chronological fact history |
| `mempalace_kg_stats` | KG overview |
| `mempalace_traverse` | Walk the palace graph |
| `mempalace_find_tunnels` | Find rooms bridging two wings |
| `mempalace_graph_stats` | Graph overview |
| `mempalace_diary_write` | Write an agent diary entry |
| `mempalace_diary_read` | Read recent diary entries |
| `mempalace_mempalace_status` | Palace overview |
| `mempalace_get_aaak_spec` | AAAK compression spec |
| `mempalace_backfill_embeddings` | Embed any unindexed drawers |

## Architecture

```
src/
  main.rs      — startup, model/tokenizer loading, stdio MCP dispatch
  mcp.rs       — tool handlers (JSON-RPC over stdio)
  db.rs        — SQLite operations: drawers, FTS5, vec0, KG, graph
  embed.rs     — ONNX inference via tract, attention-mask mean pooling
bench/
  longmemeval_rust_useronly.py  — R@5 benchmark (user turns only)
  longmemeval_rust.py           — R@5 benchmark (all turns)
  embed_parity.py               — verify embedding parity with ChromaDB
  compare.py                    — compare two benchmark result files
```

## Retrieval: How Hybrid Search Works

`search()` runs two retrieval passes and fuses results with **Reciprocal Rank Fusion (RRF, k=60)**:

1. **Vector pass** — embeds the query, runs sqlite-vec KNN (k = limit×8), returns candidates ordered by cosine distance
2. **BM25 pass** — sanitizes query to FTS5 OR syntax, runs `drawers_fts MATCH`, returns candidates ordered by BM25 rank
3. **RRF fusion** — scores each document as Σ 1/(60 + rank_i + 1) across both lists, takes top-k

If sqlite-vec is unavailable (extension not loaded), falls back to pure FTS5.

## License

MIT
