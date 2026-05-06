# MemPalace Improvement Roadmap

Goal: elevate every dimension from current state to A+. TDD-driven, incremental,
each phase independently shippable.

## Status Dashboard

| Dimension        | Current | Target | Phase that fixes it |
|------------------|---------|--------|---------------------|
| Search quality   | B+      | A+     | Phase 3             |
| Architecture     | A-      | A+     | Phase 2             |
| Test coverage    | F       | A+     | Phase 1             |
| Code quality     | B       | A+     | Phase 2             |
| Feature completeness | C  | A+     | Phases 4–6         |
| Import pipeline  | B+      | A+     | Phases 4–5         |
| Error handling   | B-      | A+     | Phase 1             |

---

## Phase 1 — Foundation: Test Harness & Error Safety

**Prerequisite for all other work.** Nobody writes code against untestable
interfaces.

### 1.1 Test infrastructure

- [ ] Add `tempfile = "3"` to `[dev-dependencies]` in Cargo.toml (done)
- [ ] Create a `test_helpers` module (or inline in `#[cfg(test)]` blocks):
  ```rust
  // Pattern to follow in every test file:
  #[cfg(test)]
  mod tests {
      use tempfile::TempDir;
      use super::*;

      fn test_db() -> (TempDir, Database) {
          let dir = TempDir::new().unwrap();
          let db = Database::open(dir.path().to_str().unwrap()).unwrap();
          (dir, db)
      }
  }
  ```
- [x] Write the first batch of tests in `db.rs`:
  - [x] `test_open_creates_tables` — open empty DB, verify drawers/drawers_fts/triples/vec_drawers/vec_embedded exist
  - [x] `test_add_drawer_basic` — add one drawer, verify it's in drawers table and FTS index
  - [x] `test_add_drawer_idempotent_same_content` — two identical inserts produce same id (INSERT OR IGNORE)
  - [x] `test_add_drawer_different_content_different_id` — different content → different id
  - [x] `test_get_drawer_count` — count matches inserts
  - [x] `test_delete_drawer` — delete removes from drawers, FTS, vec_drawers, vec_embedded
  - [x] `test_delete_nonexistent_drawer` — returns DrawerNotFound
  - [x] `test_upsert_drawer_insert` — upsert creates when not present
  - [x] `test_upsert_drawer_replace` — upsert overwrites when present (content, wing, room)
  - [x] `test_fts_search_basic` — insert text, search finds it
  - [x] `test_fts_search_no_match` — search returns empty when nothing matches
  - [x] `test_fts_search_wing_filter` — filter by wing restricts results
  - [x] `test_fts_search_room_filter` — filter by room restricts results
  - [x] `test_fts_search_limit` — limit parameter respected
  - [x] `test_sanitize_fts_query_multi_word` — "hello world" → "hello OR world"
  - [x] `test_sanitize_fts_query_single_token` — "hello" → "hello" (no wrapping)
  - [x] `test_sanitize_fts_query_already_has_syntax` — pass-through when `"`, `*`, `(`, etc.
  - [x] `test_bulk_replace_basic` — replace string across multiple drawers
  - [x] `test_bulk_replace_no_match` — returns 0 when find string absent

- [x] Write first batch of tests in `embed.rs`:
  - [x] `test_embed_returns_1536_bytes` — 384 f32 * 4 = 1536 bytes
  - [x] `test_embed_deterministic` — same input → same output
  - [x] `test_embed_empty_string` — empty input doesn't panic (returns Ok or None)
  - [x] `test_embed_l2_normalized` — output vector has unit L2 norm
  - [x] `test_embed_unicode` — CJK input works

- [x] Write first batch of tests in `knowledge_graph.rs`:
  - [x] `test_add_triple` — add fact, verify in DB
  - [x] `test_add_triple_idempotent` — adding same triple twice returns same ID
  - [x] `test_query_entity_outgoing` — find facts by subject
  - [x] `test_query_entity_incoming` — find facts by object
  - [x] `test_query_entity_both` — default direction finds both
  - [x] `test_query_as_of` — time filter excludes facts outside window
  - [x] `test_invalidate` — sets valid_until
  - [x] `test_timeline_entity` — ordered chronologically
  - [x] `test_timeline_all` — full timeline with LIMIT 100
  - [x] `test_stats` — returns entity count, triple count, predicates

- [x] Write first batch of tests in `import_sessions.rs`:
  - [x] `test_slugify_replaces_spaces_with_dashes` — "Hello World" → "hello-world"
  - [x] `test_slugify_collapses_multiple_dashes` — "a--b" → "a-b"
  - [x] `test_slugify_max_64_chars` — long titles truncated
  - [x] `test_slugify_empty_string` — returns "session"
  - [x] `test_slugify_special_chars` — "Session: Memory?" → "session-memory"

- [x] Write first batch of tests in `indexer.rs`:
  - [x] `test_slugify_path` — "src/db.rs" → "src-db-rs"
  - [x] `test_active_extensions_default` — includes "rs", "go", "py", "md" etc.
  - [x] `test_active_extensions_env_override` — MEMPALACE_EXTENSIONS="go,py" overrides

- [x] Make `cargo test --release` pass with all the above.

### 1.2 Fix silent data loss

- [x] In `import_sessions.rs:35`: change `filter_map(|r| r.ok())` to propagate errors
  via `collect::<rusqlite::Result<Vec<_>>>()?`. If a row fails to parse, the whole
  import should fail (or at minimum collect errors and report them).

- [x] In `import_sessions.rs:113-120`: replace nested `.ok().and_then()` chain with
  proper error handling. Log a warning when JSON parsing fails for a specific part,
  but continue processing remaining parts.

- [x] Bug fix: `SKIP_DIRS` had mixed-case entries (`"Pods"`, `"DerivedData"`) that
  would never match the lowercased comparison in the indexer filter. Fixed to lowercase.

### 1.3 Structured logging

- [x] Create `src/log.rs` with a `log!` macro supporting levels: `info`, `warn`, `error`, `debug`.
- [x] Add `RUST_LOG` env var check: if `RUST_LOG=debug`, enable debug-level output.
- [x] Replace all `eprintln!()` calls in `src/main.rs` with `log!("info", ...)` / `log!("error", ...)`.
- [x] Replace all `eprintln!("WARN: ...")` in `src/import_sessions.rs` with `log!("warn", ...)`.
- [x] Replace `eprintln!("WARN: ...")` in `src/indexer.rs` with `log!("warn", ...)`.
- [x] Replace `eprintln!("[embed] ...")` in `src/embed.rs` with `log!("info", ...)` / `log!("warn", ...)`.
- [x] Replace `eprintln!("  [backfill] ...")` in `src/db.rs` with `log!("info", ...)` / `log!("warn", ...)`.

**Phase 1 completion check:**
```
cargo test --release   # all tests green
cargo clippy -- -D warnings  # clean
cargo fmt -- --check    # clean
```

---

## Phase 2 — Eliminate Duplication & Move Logic to db.rs

### 2.1 Abstract the (wing, room) query pattern

The pattern `match (wing, room) { (Some(w), Some(r)) => ..., (Some(w), None) => ..., ... }`
appears in 6 places across `db.rs`. Unify it.

- [ ] Create a query builder helper in `db.rs`:
  ```rust
  struct SearchQuery {
      base_sql: String,      // "SELECT ... FROM ... WHERE embedding MATCH ?1 AND k = {fetch}"
      params: Vec<Box<dyn rusqlite::types::ToSql>>,
  }

  impl SearchQuery {
      fn new(base_sql_template: &str) -> Self;
      fn filter_wing(&mut self, wing: &str);
      fn filter_room(&mut self, room: &str);
      fn with_limit(&mut self, limit: usize);
      fn execute_vector(&self, conn: &Connection, vec_bytes: &[u8]) -> Result<Vec<...>>;
      fn execute_fts(&self, conn: &Connection, query: &str) -> Result<Vec<...>>;
  }
  ```
- [ ] Refactor `fts_search`, `fts_search_raw`, `vector_search_raw`, and
  `search_hybrid` to use this helper. Delete the old duplicated arms.
- [ ] Remove `#[allow(dead_code)]` from `vector_search`. Either use it in the
  search path or delete it.
- [ ] Verify: `cargo test --release` still green, no behavior change.

### 2.2 Move graph tool logic from mcp.rs to db.rs

Currently `tool_traverse`, `tool_find_tunnels`, and `tool_graph_stats` live
entirely in `mcp.rs` with inline SQL — violating the MCP Tool Guidelines
("Implement core logic in `db.rs`").

- [ ] Move `tool_traverse` logic to `db.rs` as `pub fn traverse(start_room, max_hops) -> Result<Value>`
- [ ] Move `tool_find_tunnels` logic to `db.rs` as `pub fn find_tunnels(wing_a, wing_b) -> Result<Value>`
- [ ] Move `tool_graph_stats` logic to `db.rs` as `pub fn get_graph_stats() -> Result<Value>`
- [ ] In `mcp.rs`, each tool handler becomes a 2-line delegation:
  ```rust
  "mempalace_traverse" => {
      let result = self.db.traverse(start_room, max_hops)?;
      Ok(serde_json::to_string(&result)?)
  }
  ```
- [ ] Write tests for the moved functions in `db.rs`:
  - [ ] `test_traverse_existing_room` — returns that room at hop 0
  - [ ] `test_traverse_missing_room` — returns error with suggestions
  - [ ] `test_traverse_max_hops` — respects hop limit
  - [ ] `test_find_tunnels_between_two_wings` — returns rooms spanning both
  - [ ] `test_find_tunnels_no_wings` — returns all multi-wing rooms
  - [ ] `test_graph_stats` — returns expected keys (total_rooms, total_edges, etc.)

**Phase 2 completion check:**
```
cargo test --release   # all tests green, including new graph tool tests
cargo clippy -- -D warnings  # clean
# Verify: no SQL left in mcp.rs outside of the TOOLS_JSON string
rg "SELECT|INSERT|DELETE|UPDATE" src/mcp.rs --count
# Should be 0 (except possibly in TOOLS_JSON description strings — those are fine)
```

---

## Phase 3 — Search Recency

This is the phase that directly addresses the user's original complaint:
"most recent memory" should return recent memories.

### 3.1 Add `filed_at` to search results

- [ ] Modify the search result structs and JSON outputs to include a `filed_at`
  field on every result:
  - `fts_search`: add `d.filed_at` to SELECT, include in result JSON
  - `vector_search_raw`: add `d.filed_at` to SELECT, include in return tuple
  - `search_hybrid`: include `filed_at` in metadata HashMap, include in result JSON

### 3.2 Add recency decay to hybrid RRF scoring

- [ ] Add a `--boost-recent` option (default: enabled in MCP, disabled in CLI for
  backwards compat). When enabled, modify `search_hybrid` scoring:
  ```rust
  // After RRF score computed, apply time decay:
  let age_seconds = (now - filed_at).as_secs_f64();
  let recency_boost = 1.0 / (1.0 + age_seconds / half_life_seconds);
  final_score = rrf_score * (1.0 + recency_weight * recency_boost);
  ```
  - `half_life_seconds`: 86400.0 (24 hours) — configurable via env var `MEMPALACE_RECENCY_HALF_LIFE` or tool parameter
  - `recency_weight`: 0.3 — configurable via env var `MEMPALACE_RECENCY_WEIGHT` or tool parameter
- [ ] Add `sort_by` parameter to the `mempalace_search` tool schema:
  ```json
  "sort_by": {
    "type": "string",
    "description": "Sort mode: 'relevance' (default), 'recency', or 'hybrid'",
    "enum": ["relevance", "recency", "hybrid"]
  }
  ```
- [ ] When `sort_by = "recency"`, just run `ORDER BY filed_at DESC` (pure chronological).
- [ ] When `sort_by = "hybrid"`, apply the recency decay above.
- [ ] When `sort_by = "relevance"`, current behavior (no time weighting).

### 3.3 Tests for recency

- [ ] `test_search_sort_by_recency` — insert 5 semantically identical drawers
  with staggered `filed_at`, search, verify newest first.
- [ ] `test_search_sort_by_relevance` — insert 5 drawers where one is highly
  relevant but old, verify relevance order preserved.
- [ ] `test_search_hybrid_sort` — insert one exact-match drawer that's 30 days
  old and one vaguely-relevant drawer filed 1 hour ago. With `hybrid` mode,
  the recent one should surface near the top (not necessarily #1, but higher
  than it would be in pure relevance mode).
- [ ] `test_recency_decay_no_effect_when_weight_zero` — with recency_weight=0,
  results identical to relevance mode.

### 3.4 Wire into MCP

- [ ] Update `TOOLS_JSON` constant in `mcp.rs` for `mempalace_search`:
  add `sort_by` and `recency_weight` and `recency_half_life` parameters.
- [ ] Update `search()` handler in `mcp.rs` to pass new parameters through.
- [ ] Add the `filed_at` field to the JSON response shape.
- [ ] Update `TOOLS_JSON` descriptions to mention recency-aware search.

**Phase 3 completion check:**
```
cargo test --release   # all tests green, including recency tests
# Manual verification: search "most recent memory" returns May 2026 sessions
# before April 2026 sessions when sort_by=hybrid or sort_by=recency.
```

---

## Phase 4 — Session Import Quality

### 4.1 Enrich imported session content

- [ ] Add a `filed_at` override to `upsert_drawer`: accept an optional timestamp
  so imported sessions preserve their original `time_updated` rather than getting
  `datetime('now')`. Pass `time_updated` from opencode.db through.

- [ ] Modify `collect_assistant_text` to also capture:
  - [ ] Session title and directory (already done)
  - [ ] Session timestamp in ISO format (new)
  - [ ] First user message (provides initial context)
  - [ ] Tool call names executed (e.g., "Used tools: explore, bash, read")
  - [ ] The last assistant response (already done via tail)
  - [ ] Total message/part count as a summary line
  - Target: ~3000 chars total (up from 2000, still safe for embedding)

- [ ] Write tests:
  - [ ] `test_collect_assistant_text_has_timestamp` — output starts with session date
  - [ ] `test_collect_assistant_text_has_tool_names` — includes "Used tools: ..." line
  - [ ] `test_collect_assistant_text_respects_max_chars` — doesn't exceed limit
  - [ ] `test_collect_assistant_text_empty_session` — returns title + timestamp only
  - [ ] `test_import_sessions_preserves_timestamp` — filed_at matches session time_updated

### 4.2 Make session content limits configurable

- [ ] Add `MEMPALACE_SESSION_MAX_CHARS` env var (default 3000).
- [ ] Add `MEMPALACE_FILE_MAX_CHARS` env var (default 4000).
- [ ] Read from env in `collect_assistant_text` and `indexer.rs`.

### 4.3 Add `mempalace_import_sessions` MCP tool

- [ ] Add tool definition to `TOOLS_JSON`:
  ```json
  {
    "name": "mempalace_import_sessions",
    "description": "Import sessions from an opencode.db into the palace. Re-runs index-sessions logic from within an MCP session.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "oc_db_path": {
          "type": "string",
          "description": "Path to opencode.db (default: ~/.local/share/opencode/opencode.db)"
        }
      }
    }
  }
  ```
- [ ] Add `handle_tool_call` arm that delegates to `import_sessions::import_sessions`.
- [ ] Return `{"success": true, "imported": N, "total_sessions": M}`.

- [ ] Test:
  - [ ] `test_mcp_import_sessions_empty_db` — returns 0 imported
  - [ ] `test_mcp_import_sessions_valid_db` — creates drawers for each session

### 4.4 Auto-import hook in mcp.rs

- [ ] After processing `initialize` or on first tool call in a new session,
  trigger a background import (tokio::spawn or equivalent).
  - Check a sentinel `last_auto_import` timestamp — only run if > 5 minutes
    since last auto-import.
  - This ensures every MCP session sees near-current data without manual CLI.
- [ ] Test: start MCP server, make a search call, verify sessions from the
  current hour are present in results.

**Phase 4 completion check:**
```
cargo test --release   # all tests green
# Manual: create new session in opencode, call mempalace_import_sessions,
# then mempalace_search "new session" — should find the session just created.
```

---

## Phase 5 — Incremental Session Sync

### 5.1 Track import state

- [ ] Add a `sync_state` table to the schema:
  ```sql
  CREATE TABLE IF NOT EXISTS sync_state (
      source TEXT PRIMARY KEY,           -- e.g. "opencode_sessions"
      last_time_updated INTEGER NOT NULL -- millis timestamp of last imported session
  );
  ```
- [ ] Modify `import_sessions` to:
  - [ ] Read `last_time_updated` from `sync_state` for the given source.
  - [ ] Query `FROM session WHERE time_updated > ?1` instead of all sessions.
  - [ ] After import, write the `MAX(time_updated)` of imported sessions back
    to `sync_state`.
  - [ ] Add a `--full` flag to force full re-import (ignore sync_state).
- [ ] Test:
  - [ ] `test_incremental_import_first_run` — imports all sessions, records timestamp
  - [ ] `test_incremental_import_no_new_sessions` — second run imports 0
  - [ ] `test_incremental_import_with_new_session` — add a session to test DB,
    second run imports exactly 1
  - [ ] `test_incremental_import_full_flag` — `--full` imports all regardless
  - [ ] `test_incremental_import_preserves_tracking_after_failure` — interrupted
    import doesn't advance timestamp

### 5.2 Wire to MCP

- [ ] Update `mempalace_import_sessions` tool to accept `full` and `limit`
  parameters.
- [ ] The auto-import hook from Phase 4.4 should use incremental mode.

**Phase 5 completion check:**
```
cargo test --release   # all tests green
# Sequential import runs: 1st reports N imported, 2nd reports 0, 
# add session, 3rd reports 1.
```

---

## Phase 6 — Feature Completeness

### 6.1 Pagination for search

- [ ] Add `offset` parameter to `mempalace_search` (default 0).
- [ ] Add total result count to response (for "showing page 3 of 10" UX).
- [ ] Update `TOOLS_JSON` schema.
- [ ] Test:
  - [ ] `test_search_pagination` — offset=0 gives first page, offset=5 gives next
  - [ ] `test_search_pagination_beyond_bounds` — offset > total returns empty

### 6.2 Date range filter for search

- [ ] Add `filed_after` and `filed_before` ISO datetime parameters.
- [ ] Add `WHERE d.filed_at >= ? AND d.filed_at <= ?` clauses when provided.
- [ ] Test:
  - [ ] `test_search_date_range_inclusive` — boundaries included
  - [ ] `test_search_date_range_empty` — range with no matches returns empty

### 6.3 Export functionality

- [ ] Add `mempalace_export` tool:
  ```json
  {
    "name": "mempalace_export",
    "description": "Export drawers as JSON Lines. Filter by wing and/or room.",
    "inputSchema": {
      "properties": {
        "wing": { "type": "string", "description": "Filter by wing (optional)" },
        "room": { "type": "string", "description": "Filter by room (optional)" },
        "format": { "type": "string", "description": "'jsonl' or 'aaak'", "default": "jsonl" }
      }
    }
  }
  ```
- [ ] Add `mempalace_export_kg` tool — exports knowledge graph as JSON.
- [ ] Test:
  - [ ] `test_export_jsonl` — valid JSON lines, one per drawer
  - [ ] `test_export_filtered` — wing filter reduces output
  - [ ] `test_export_kg` — valid JSON with triples array

### 6.4 `mempalace_list_recent` tool

- [ ] Add tool for purely time-ordered recent content:
  ```json
  {
    "name": "mempalace_list_recent",
    "description": "List recently filed content, ordered by filed_at descending. Use this when you need to know what's new.",
    "inputSchema": {
      "properties": {
        "limit": { "type": "integer", "description": "Max results (default 20)" },
        "wing": { "type": "string", "description": "Filter by wing (optional)" },
        "since": { "type": "string", "description": "Only entries filed after this ISO datetime" }
      }
    }
  }
  ```
- [ ] Test:
  - [ ] `test_list_recent_order` — newest first
  - [ ] `test_list_recent_since` — filters by date

### 6.5 Soft-delete for drawers

- [ ] Add `deleted_at DATETIME` column to drawers table.
- [ ] Modify all search queries to add `AND deleted_at IS NULL`.
- [ ] `delete_drawer` sets `deleted_at = datetime('now')` instead of DELETE.
- [ ] Add `mempalace_restore_drawer` tool.
- [ ] Add `mempalace_purge_deleted` tool (hard-delete soft-deleted drawers).
- [ ] Test:
  - [ ] `test_soft_delete_hides_from_search`
  - [ ] `test_restore_makes_searchable_again`
  - [ ] `test_purge_removes_permanently`

### 6.6 Backup/restore

- [ ] Add `mempalace_backup` tool: copies `palace.db` to specified path
  (or `~/backups/mempalace/<timestamp>.db`).
- [ ] Add `mempalace_restore` tool: replaces current DB with a backup.
- [ ] Test:
  - [ ] `test_backup_creates_file` — file exists at expected path
  - [ ] `test_backup_is_complete` — drawer count matches source

**Phase 6 completion check:**
```
cargo test --release   # all tests green, ~50 new tests across all new tools
cargo clippy -- -D warnings  # clean
cargo fmt -- --check    # clean
```

---

## Phase 7 — Performance & Polish

### 7.1 Performance benchmarks

- [ ] Create a benchmark module using Rust's built-in `#[bench]` harness
  (or criterion.rs dev-dependency if more detail needed):
  - [ ] `bench_add_drawer` — throughput for single inserts
  - [ ] `bench_add_drawer_bulk` — throughput for batch inserts
  - [ ] `bench_search_fts` — latency for keyword search
  - [ ] `bench_search_vector` — latency for vector search
  - [ ] `bench_search_hybrid` — latency for hybrid search
  - [ ] `bench_embed` — latency for single text embedding
  - [ ] `bench_import_sessions` — throughput for 100-session import
  - [ ] `bench_bulk_replace` — latency for replacing across 10k drawers
  - [ ] `bench_delete` — latency for delete

- [ ] Run benchmarks on a 100k-drawer palace and verify:
  - p99 search latency < 100ms
  - p99 add_drawer latency < 50ms

- [ ] Fix any regressions. If search p99 > 100ms:
  - Profile with Instruments (macOS) or perf (Linux).
  - Optimize the bottleneck (likely embedding computation or vec0 query).

### 7.2 Concurrent access test

- [ ] Write a test that spawns 10 threads, each doing 100 mixed search/insert/delete
  operations on the same DB. Verify no panics, no data corruption.
- [ ] If SQLITE_BUSY occurs, add retry logic with exponential backoff.

### 7.3 Input validation hardening

- [ ] Audit all `get_str`, `get_i64`, `get_f64` call sites. Ensure:
  - Limit values are clamped to 1..1000 (not arbitrary).
  - String inputs reject null bytes (`\0`).
  - Content length is validated before embedding (reject > 100k chars).
- [ ] Add tests for each validation failure:
  - [ ] `test_search_limit_zero_rejected`
  - [ ] `test_add_drawer_null_byte_rejected`
  - [ ] `test_add_drawer_content_too_large`

### 7.4 Documentation cleanup

- [ ] Update README.md with new feature descriptions (sort_by, recency, export, etc.).
- [ ] Add examples for each new tool to the README.
- [ ] Update the pre-release checklist in AGENTS.md to include the new test suite.

### 7.5 Semantic versioning

- [ ] Version bump: `3.0.0` → `3.1.0` (backward-compatible additions from Phases 2–6).
- [ ] If the soft-delete migration (Phase 6.5) requires schema change, bump to `4.0.0`
  and add an auto-migration in `create_tables`.

**Phase 7 completion check:**
```
cargo test --release   # all tests green (including concurrent, validation, benchmarks)
cargo bench            # produces benchmark report, all within targets
cargo clippy -- -D warnings  # clean
cargo fmt -- --check    # clean
# Binary size hasn't grown unexpectedly (compare to 3.0.0 baseline)
```

---

## How to Execute This Plan

### For a human

1. Start at Phase 1. Every phase is a PR. Each PR must pass `cargo test --release`,
   `cargo clippy -- -D warnings`, and `cargo fmt -- --check`.
2. After each PR merges, run the LongMemEval benchmark to verify no regression.
3. Tag releases at Phase 3, Phase 5, and Phase 7.

### For an agent (AI assistant)

When picking up this plan mid-execution:

1. **Determine current phase.** Check which tests exist (`rg "#\[test\]" src/`).
   Count completed tasks. Determine the next un-started task in the earliest
   incomplete phase.

2. **Follow the TDD cycle per task:**
   - Read the task description and its test spec.
   - Write the test FIRST, verify it fails.
   - Read the relevant source files to understand current code.
   - Write the minimum code to pass the test.
   - Run `cargo test --release` to confirm green.
   - Run `cargo clippy -- -D warnings` and `cargo fmt`.
   - Commit with message: `phase(N): <task description>`.

3. **Do not skip phases.** Each phase builds on the prior. Phase 2's refactoring
   requires Phase 1's test coverage. Phase 3's recency requires Phase 2's
   deduplication (or you'll write recency logic in 4 different search functions).

4. **If a test fails unexpectedly:** do NOT modify the test to make it pass.
   Instead, understand why the existing code doesn't meet the spec, and fix
   the code. The test is the authority.

5. **After each phase:** run the full test suite and report:
   ```
   tests passed: N / total: M
   clippy warnings: 0
   fmt: clean
   ```
