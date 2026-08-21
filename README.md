# MemPalace MCP Server

A single-binary, zero-dependency MCP (Model Context Protocol) server providing persistent, semantic memory for AI assistants. Written in Rust with embedded ONNX model and bundled SQLite extensions.

## 🎯 Project Background

Inspired by [milla-jovovich/mempalace](https://github.com/milla-jovovich/mempalace), this Rust reimplementation addresses the original Python version's architectural limitations:

- **Single binary deployment** – no Python runtime, no virtualenv, no system dependencies
- **Embedded ML model** – ONNX `all-MiniLM-L6-v2` (384-dim) baked in via `include_bytes!`
- **Bundled SQLite extensions** – FTS5 for BM25 keyword search, sqlite-vec for vector similarity
- **Zero-configuration** – creates palace directory and database automatically
- **Deterministic IDs** – content-addressed drawer IDs for deduplication
- **Agent diary compression** – AAAK format for efficient session logging

This implementation is **v3.1.0** (49 MCP tools). User-turns LongMemEval R@5 is **96.81%** (455/470) after the Phase 15 ranking work (sanitizer, keyword/phrase boosts, recency/`authored_at` tie-break) plus Phase 25 contradiction *annotation* (does not reorder). We do not special-case LongMemEval question IDs. LoCoMo: `python bench/locomo_rust.py` (synthetic baseline n=2 R@5=100%; pass `--data` for the official JSON).

## 🔑 Key Design Decisions

### 1. **Single-Binary Philosophy**
All dependencies are vendored or bundled:
- `rusqlite` with `bundled` feature → no system SQLite3 needed
- `sqlite-vec` → vector extension compiled in
- `tract-onnx` → pure-Rust ONNX inference (no Python/torch)
- `tokenizers` → HuggingFace BERT tokenizer
- Model + tokenizer bytes embedded at compile time (~4.5MB each)

Result: one `mempalace-mcp` executable (~16MB stripped) that runs anywhere Rust 1.75+ supports.

### 2. **Hybrid Search Architecture**
Instead of choosing between vector or keyword search, we use both:
- **Vector pass**: embed query → sqlite-vec KNN (limit×8 candidates) → cosine similarity
- **BM25 pass**: FTS5 OR-tokenized query → rank-scored candidates
- **Fusion**: Reciprocal Rank Fusion (RRF, k=60) combines ranked lists
- **Fallback**: if vec0 extension fails to load, pure FTS5 search

This mirrors the rigor of academic hybrid retrieval while staying within the single-binary constraint.

### 3. **Write-Optimized Storage Model**
- WAL-mode SQLite for concurrent readers/writers
- `drawers_fts` virtual table with sync triggers keeps FTS5 current
- Shadow table `vec_embedded` tracks which drawers have vectors (since vec0 lacks reliable point-lookups)
- Deterministic `drawer_id = md5(content + wing + room)` enables deduplication
- Separate tables for triples (knowledge graph) and diary entries

### 4. **MCP-First Tool Design**
All 49 tools follow MCP conventions:
- Consistent JSON-RPC over stdio transport
- Rich inputSchema validation
- Standardized success/error response shapes
- Agent diary and knowledge graph tools match the original spec
- New tools (`update_drawer`, `bulk_replace`) added for maintenance workflows

### 5. **Search with Recency Awareness**
  `mempalace_search` supports three sort modes:
  - `relevance` (default): hybrid vector + FTS5 with Reciprocal Rank Fusion
  - `recency`: newest first, FTS5-filtered
  - `hybrid`: RRF fusion with exponential time decay boost
  - Pagination via `offset`, date range via `filed_after`/`filed_before`, exact `source_file` filter, optional `max_distance`

### 6. **Incremental Session Sync**
  `mempalace_import_sessions` tracks last imported timestamp via `sync_state`
  table — subsequent runs only import new/changed sessions. Use `full: true`
  to force re-import. Sessions include tool names, first message, timestamp,
  and message/part counts.

### 7. **Export & Backup**
  - `mempalace_export`: JSON Lines export by wing/room
  - `mempalace_export_kg`: full knowledge graph as JSON
  - `mempalace_backup`: copy palace.db with timestamp
  - `mempalace_restore`: restore from backup

## 🚀 Enhancements Over Original

### ✅ Import from Existing Palace
```sh
mempalace import-palace /path/to/old/palace.db
```
Migrates drawers and triples from any compatible MemPalace SQLite database (original Python or Rust version).

### ✅ Import Agent Sessions (OpenCode, Codex, Grok Build, Zcode)
```sh
mempalace index-sessions                    # auto: import from every store found
mempalace index-sessions --source codex     # only one source
mempalace index-sessions --db /custom/path/to/opencode.db   # path override (opencode/zcode)
```
Sources and their default locations (env-overridable via `MEMPALACE_OPENCODE_DB`,
`MEMPALACE_CODEX_HOME`, `MEMPALACE_GROK_HOME`, `MEMPALACE_ZCODE_DB`):

| Source | Store | Wing / stable ID |
|--------|-------|------------------|
| OpenCode | `~/.local/share/opencode/opencode.db` | `opencode` / `oc_session_{id}` |
| Codex | `~/.codex/sessions/**/*.jsonl` + `session_index.jsonl` | `codex` / `codex_{id}` |
| Grok Build | `~/.grok/sessions/<cwd>/<uuid>/` | `grok` / `grok_{id}` |
| Zcode | `~/.zcode/cli/db/db.sqlite` | `zcode` / `zc_{id}` |

Behavior:
- Auto mode silently skips stores that don't exist; explicitly naming a missing
  store returns `SourceNotFound`.
- Incremental by default (`sync_state` per source); `--full` re-imports everything.
- System/developer prompt boilerplate is never imported.
- Foreign databases are opened read-only.
- Each session becomes one drawer: room = slugified title, content = title +
  date + directory + first user message + assistant text (head/tail bounded).
- Automatic deduplication on re-run (byte-identical content is skipped).

### ✅ Bulk Import with Filtering
```sh
mempalace index /path/to/source  # indexes all files
mempalace index /path/to/source --include "*.rs,*.txt,*.md"  # whitelist
mempalace index /path/to/source --exclude "*.log,*.tmp"      # blacklist
```
Indexes plain text files into the palace:
- `wing` = parent directory name (configurable via future flag)
- `room` = file stem (slugified)
- `content` = full file contents
- `source_file` = relative path from root
- `added_by` = "indexer"
- Skips binaries via null-byte detection
- Respects `.gitignore` patterns automatically

### ⚙️ Maintenance Tools
```sh
# Update a drawer in-place (preserves ID)
mempalace update_drawer --id drawer_xyz --content "new text"

# Find/replace across all drawers (or one wing)
mempalace bulk_replace --find "OldName" --replace "NewName" --wing project_alpha

# Backfill missing embeddings (after schema changes)
mempalace reindex
```
These transform MemPalace from write-only to a fully maintainable memory system.

### 📊 Observability
- `mempalace_status` → palace overview (drawer count, wing/room distribution)
- `mempalace_get_taxonomy` → wing → room → drawer count tree
- `mempalace_kg_stats` → knowledge graph health
- `mempalace_graph_stats` → palace graph connectivity
- `mempalace_diary_read` → per-agent AAAK journal
- `mempalace_profile` / `mempalace_context` → derived KG profile + session start bundle
- `mempalace_memory` / `mempalace_recall` → coarse save/forget and search+profile

## 🏗️ Architecture Overview

```
src/
├── main.rs      – Binary entry point: loads ONNX model/tokenizer, dispatches MCP
├── mcp.rs       – MCP tool handlers (49 tools, JSON-RPC over stdio)
├── db.rs        – Core SQLite operations: drawers, FTS5, vec0, KG, graph, bulk ops
├── embed.rs     – ONNX inference via tract, attention-mask mean pooling
├── import_sessions.rs – OpenCode session import logic
├── import_palace.rs   – Cross-version palace importer
├── indexer.rs     – File system indexer with whitelist/blacklist support
├── hallways.rs    – Structural entity co-occurrence graph (no NLP)
├── lock.rs        – Process writer flock
├── profile.rs     – Derived profile + one-call context
├── validate.rs    – Query/date/name sanitizers
├── wal.rs         – JSONL write-ahead audit log
└── knowledge_graph.rs – Triple store with temporal validity and supersede

bench/
  longmemeval_rust_useronly.py – 500-question episodic memory benchmark (user turns)
  locomo_rust.py               – LoCoMo retrieval harness (synthetic default; --data for official JSON)
  longmemeval_rust.py          – Same benchmark (all turns: user + assistant)
  embed_parity.py              – Verify embedding equivalence with ChromaDB
  compare.py                   – Diff two benchmark JSON outputs
```

## 🔍 Retrieval Deep Dive

`search(query, limit, wing, room, embedder)` executes:

1. **Vector Path** (if embedder provided):
   - Query → ONNX embedding (384-float32 vector)
   - sqlite-vec KNN search: `SELECT * FROM vec_drawers WHERE embedding MATCH ? LIMIT limit×8`
   - Join to drawers table, apply wing/room filters
   - Return top `limit` by cosine similarity (1 - distance/2)

2. **BM25 Path**:
   - Sanitize query: quote phrases, split whitespace → OR-joined tokens
   - FTS5 search: `SELECT * FROM drawers_fts WHERE drawers_fts MATCH ? [AND wing=? AND room=?] ORDER BY rank LIMIT limit`
   - Join to drawers table to retrieve content

3. **Reciprocal Rank Fusion**:
   - Score(doc) = Σ 1/(60 + rankᵢ + 1) over all lists where doc appears
   - Re-score all unique docs from both passes
   - Return top `limit` by fused score

If vec0 fails to load (rare, indicates corrupted install), automatically falls back to BM25-only.

## 📈 Benchmark Results

Evaluated on [LongMemEval](https://github.com/xiaowu0162/LongMemEval) (500-question episodic memory benchmark):

| Mode | R@5 | Hits/Scored |
|------|-----|-------------|
| Rust MemPalace — user-turns-only | **96.81%** | 455/470 |
| Rust MemPalace — all turns | 91.70% | 431/470 (pre-Phase-15; not re-run) |

By category (user-turns-only, 2026-08-21, v3.1.0):

| Category | R@5 |
|----------|-----|
| knowledge-update | 100.00% (72/72) |
| multi-session | 97.52% (118/121) |
| temporal-reasoning | 95.28% (121/127) |
| single-session-assistant | 94.64% (53/56) |
| single-session-user | 100.00% (64/64) |
| single-session-preference | 90.00% (27/30) |

**Notes**:
- User-turns-only indexes only user turns (no assistant self-talk) for a fair comparison with Python raw mode
- Dataset: HuggingFace `xiaowu0162/longmemeval-cleaned` (`longmemeval_s_cleaned.json`); not vendored in this repo
- Beats the claimed Python raw 96.6% without question-ID ranking hacks; Python "hybrid_v4" was confirmed rigged (3 hardcoded IDs)
- Pre-Phase-15 floor was 94.04% (442/470); remaining 15 misses are structural (multi-hop, temporal implication, or missing facts), not ranking-order bugs
- Contradiction annotation (Phase 25) does not reorder hits; kill-switch `MEMPALACE_CONTRADICTION_ANNOTATION=0`

## 🔧 Installation & Usage

### Binary Installation
```sh
# Build from source (requires Rust 1.75+)
git clone https://github.com/vernonstinebaker/mempalace.git
cd mempalace
unset CARGO_TARGET_DIR   # IDEs/sandboxes may redirect cargo output
make install             # cargo build --release && cp to ~/bin/mempalace-mcp
mempalace-mcp --info     # expect: MemPalace v3.1.0 (Rust)

# Alternative: copy into a directory already on PATH
# sudo cp target/release/mempalace-mcp /usr/local/bin/
```

Prefer `make install` over a symlink: some clients hold `~/bin/mempalace-mcp` open, and a symlink to a stale `target/release` (or a Cursor `CARGO_TARGET_DIR` cache) will keep serving an old binary. Restart any MCP client after install so it execs the new file. Native target is `aarch64-macos` (vdsmini); do not copy this Mach-O to RISC-V or Android.

### First Run
```sh
# Creates ~/mempalace/palace.db automatically
MEMPALACE_PALACE_PATH=~/mempalace mempalace-mcp --info
```

### MCP Server Mode (default)
```sh
# stdio transport (used by OpenCode/OpenCat)
MEMPALACE_PALACE_PATH=~/mempalace mempalace-mcp
```

### Maintenance Commands
```sh
# Import existing palace (original Python or Rust version)
mempalace import-palace /path/to/old/palace.db

# Search from the terminal (same hybrid retrieval as the MCP server)
mempalace search "login bug" --wing codex --limit 10
mempalace search "read-only databases" --room backend --source /tmp/notes.md

# Import agent sessions (auto-discovers OpenCode/Codex/Grok/Zcode)
mempalace index-sessions

# Index source code with whitelist
mempalace index ~/projects --include "*.rs,*.cpp,*.py,*.js,*.ts"

# Bulk replace (useful after renaming projects/people)
mempalace bulk_replace --find "OldProject" --replace "NewProject" --wing project_alpha

# Re-embed any drawers missing vectors
mempalace reindex
```

## 📜 License

MIT License

Copyright (c) 2026 MemPalace Contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.

## 🙏 Inspiration & Credits

- Core memory model, AAAK format, wing/room/drawer metaphor, knowledge graph → [milla-jovovich/mempalace](https://github.com/milla-jovovich/mempalace)
- Embedding model → sentence-transformers/all-MiniLM-L6-v2 (ONNX conversion)
- Hybrid retrieval design → academic literature on RRF (Cormack et al. 2009, Cormack & Clarke 2010)
- MCP specification → [modelcontextprotocol.io](https://modelcontextprotocol.io)

## 🧪 Testing

```sh
cargo test --release          # 281 tests as of v3.1.0 / PLAN phases 15–27
cargo fmt -- --check
cargo clippy --release -- -D warnings
python bench/locomo_rust.py   # synthetic LoCoMo harness (pass --data for official JSON)
```
- Rust/sqlite-vec integration → [asg017/sqlite-vec](https://github.com/asg017/sqlite-vec)
- Pure-Rust ONNX → [github.com/pelotom/ tract](https://github.com/pelotom/tract)
- Benchmark methodology → [LongMemEval](https://github.com/xiaowu0162/LongMemEval)
- Implementation → Developed with AI assistance using OpenCode and various LLMs for code generation, cross-checking, and refinement

## 🤝 Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for details on:
- Reporting bugs
- Suggesting features
- Submitting pull requests
- Code style and testing guidelines

We welcome contributions from the community!
