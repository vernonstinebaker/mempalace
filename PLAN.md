# MemPalace Rust — Active Plan

**This file is the single source of truth for remaining work.**
`ROADMAP.md` is a historical archive of Phases 1–14 (done). Do not start
new work from ROADMAP.md.

Last reconciled against Python MemPalace **v3.7.1** (2026-08-14) and this
repo’s tree on **2026-08-21**.

Competitive analysis vs. Supermemory lives in
[`docs/SUPERMEMORY_COMPARISON.md`](docs/SUPERMEMORY_COMPARISON.md); it is
the rationale behind Phases 22–25. Do not implement anything from its
“NOT adopting” list.

---

## Resume here — read this first

Update this table **before you start a task** and **again when you finish
it**. If a session dies mid-task, the next agent uses this block, the
Progress log, and git status — not chat history.

| Field | Value |
|-------|-------|
| Status | `done` |
| Active phase | *(none — 15–27 complete)* |
| Active task | *(done)* |
| Last completed task | 27.1 |
| WIP notes | Phase 27 (CLI `search` subcommand) complete. Bonus fix found via TDD: hyphenated/punctuated FTS tokens silently zeroed FTS-only results (`read-only` parsed as column subtraction) — now quoted by `sanitize_fts_query`. Next work requires a new phase decision. |
| Files currently dirty | — |
| Last verification | `cargo test --release` 281 passed · fmt/clippy clean (2026-08-21) |
| Blockers | none |

**Status values:** `not_started` · `in_progress` · `blocked` · `phase_complete` · `done`

**How to resume after a lost session**

1. Read this file from the top through the active task. Do not skim.
2. `git status` and `git log -8 --oneline`. If WIP notes name files, read them.
3. If the active checkbox is still `[ ]` but code exists, **do not rewrite
   from scratch**. Run the task’s named tests. If they fail, finish the task.
   If they pass, mark it `[x]`, update this table, append the Progress log.
4. If you cannot tell whether a task is done, treat it as **not done**.
   Re-run its tests. Never skip the failing-test-first step for unfinished work.
5. Never start Phase N+1 while Phase N has an unchecked required task.
6. After every completed task: `cargo test --release` green, `cargo fmt`,
   then update **Resume here** + the task checkbox + the Progress log.

---

## 0. Strategy filter

We adopt upstream ideas only when they fit **this** product:

- Single binary, no runtime packages, no Python, no Chroma/Milvus/Qdrant/pgvector
- Embedded MiniLM ONNX + sqlite-vec + FTS5 in one `palace.db`
- Explicit errors, transactions, TDD
- MCP-first, local-first, verbatim storage

### Adopt / adapt from Python 3.4–3.7

| Upstream | Why it fits |
|----------|-------------|
| Query sanitizer + short embed query | Stops prompt-dump contamination (#333); keeps p99 search honest |
| `source_file` search filter | Exact metadata filter; we already store the column |
| Keyword-overlap + quoted-phrase boost | Honest hybrid v1/v2/v4 techniques — **not** the 3 hardcoded LongMemEval IDs |
| Equal-score recency / `authored_at` tie-break | Python 3.6 chronology; we already have recency *sort* but not tie-break |
| `kg_supersede` + half-open validity windows | Atomic fact replacement; our `invalidate`+`add` races at the boundary |
| `checkpoint`, `delete_by_source`, `sync`, `mine` MCP | Same write path, fewer round-trips; mine wraps existing `indexer.rs` |
| Structural hallways (no LLM) | Co-occurrence graph from URLs/paths/identifiers (Python 3.6 extractor) |
| Tunnel `source_drawer_id` / `target_drawer_id` | Columns already exist; MCP does not expose them |
| `busy_timeout` + writer flock | Python 3.7 paid for multi-writer SQLite corruption; we should not |
| `list_drawers` `since`/`before` | We have this on search only |

### Adopt / adapt from Supermemory (see docs/SUPERMEMORY_COMPARISON.md)

| Idea | Why it fits |
|------|-------------|
| Deterministic auto-forgetting (`expires_at`) | Engine-enforced TTL, agent-declared at write; no LLM needed (Phase 22) |
| Derived user profile view | Static = open KG triples, dynamic = recent matching drawers; pure composition (Phase 23) |
| One-call context injection | Profile + recent + diary tail in one response; fewer round-trips (Phase 23) |
| Tiny verb-level tool layer | 3 coarse tools over the 37 granular ones; better small-model discoverability (Phase 24) |
| Multi-benchmark credibility | LoCoMo harness alongside LongMemEval guards against single-benchmark overfit (Phase 25) |
| Contradiction surfacing at search time | Annotate near-duplicate hits with differing dates instead of silent co-ranking (Phase 25) |

### Explicitly out of scope (do not implement)

- Chroma / Milvus / Qdrant / pgvector backends
- EmbeddingGemma, OpenAI-compat remote embedders, LLM rerank
- HTTP MCP transport, `mempalace serve`, Docker-as-primary, write daemon
- Agent logstream, artifacts, mesh peers, RFC 004 replication
- Office `extract` mode (MarkItDown / PDF / DOCX extras)
- Nostalgia-pattern / question-ID specific LongMemEval hacks
- Soft-delete (parked — schema-breaking, Python also hard-deletes)
- Cursor/Claude hook scripts (parked — useful later, not core retrieval)

If a later agent wants something from this list, add it to
[Parking lot](#parking-lot) with a rationale. Do not sneak it into an
in-progress phase.

---

## 1. Baseline (2026-08-21) — do not regress

| Item | Current |
|------|---------|
| Crate version | `3.0.0` (`Cargo.toml`) |
| MCP tools | 37 in `TOOLS_JSON` |
| Tests | 150 `#[test]` across `src/` |
| LongMemEval user-turns R@5 | **94.04%** (442/470) — `bench/longmemeval_rust_useronly.py` |
| LongMemEval all-turns R@5 | 91.70% (431/470) |
| Python raw (Chroma, no LLM) | 96.6% — the number we want to match or beat |
| Python honest hybrid (held-out 450) | 98.4% — aspirational, no LLM |
| Search default | Hybrid RRF (k=60), `sort_by=relevance\|recency\|hybrid` |
| Search extras we have | `offset`, `filed_after`, `filed_before` |
| Search extras we lack | sanitizer, `source_file`, `max_distance`, keyword/phrase boost, `authored_at` |
| KG | add/query/invalidate/timeline/stats + `valid_to`; **no supersede** |
| Indexer | CLI `index` only; extensions already include cs/php/swift/kt/java |
| Durability | WAL journal, **no** `busy_timeout`, **no** process flock |
| Docs drift | README still says “21 tools” / “107 tests” |

*Snapshot taken **before** Phases 15–25. Values above are historical
floors, not current state — current tool/test counts and scores live in
the Resume table and Progress log. (Docs drift was resolved in Phase 21:
README now says v3.1.0 / 49 tools.)*

**Hard gates (never ship a phase that breaks these)**

- `cargo test --release` — all existing tests still pass
- `cargo fmt -- --check`
- User-turns LongMemEval R@5 **must not fall below 94.04%** after any
  search-ranking change (Phase 15). Record the new score in the Progress log.
- No new `unwrap()` / `expect()` in non-test library code
- No new runtime/system dependencies
- Binary remains a single `mempalace-mcp` with the model baked in

---

## 2. TDD protocol (mandatory, every task)

Copy this loop. Do not “just implement.”

```
1. Write the failing test named in the task (or the exact names listed).
2. Run ONLY that test; confirm it fails for the right reason:
     cargo test --release <test_name> -- --exact --nocapture
3. Write the minimum production code to pass.
4. Re-run the named test(s) — green.
5. cargo test --release          # full suite
6. cargo fmt
7. cargo clippy -- -D warnings   # required from Phase 21; try to keep clean earlier
8. Mark the task [x]. Update Resume here. Append Progress log.
```

If a test is already green before you write production code, the test is
wrong — fix the test so it fails, then implement.

Tests live in `#[cfg(test)] mod tests` of the file being changed.
Use `tempfile::TempDir` for databases. Pattern:

```rust
fn test_db() -> (tempfile::TempDir, Database) {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().to_str().unwrap()).unwrap();
    (dir, db)
}
```

MCP tools: schema in `TOOLS_JSON` (keep alphabetized by existing convention:
current file is grouped, not strictly alpha — **append new tools next to
their family**, then add a `match` arm in `execute_tool`). Success JSON:
`{ "success": true, ... }`. Errors: `{ "success": false, "error": "Code: msg" }`
or `anyhow` mapped at the boundary. Never leak raw rusqlite errors.

Commit message style (only when the user asks to commit):

```
phase(N): short description

<why>
```

---

## Phase 15 — Search quality (close the 94% → 96%+ gap)

**Goal:** Beat or match Python raw R@5 on user-turns LongMemEval **without**
LLM rerank and **without** question-specific hacks.

**Target:** user-turns R@5 ≥ 96.0%. Stretch: ≥ 96.6%.
**Floor:** 94.04%.

**Files:** `src/validate.rs`, `src/db.rs` (`search`, `search_hybrid`,
`fts_search*`, `vector_search_raw`), `src/mcp.rs` (`mempalace_search`).

### 15.1 Query sanitizer

- [x] `test_sanitize_query_strips_system_prompt_boilerplate` in `validate.rs`
      — a query that contains `IMPORTANT — MemPalace Memory Protocol` plus a
      short question must return only the short question.
- [x] `test_sanitize_query_passthrough_short` — `"why graphql"` unchanged.
- [x] `test_sanitize_query_truncates_over_250` — 1000-char unique string
      truncated to 250 chars (Python’s `maxLength: 250`).
- [x] `test_sanitize_query_empty_after_strip_returns_original` — if stripping
      would leave empty, keep a clipped original so search still runs.

Implement `validate::sanitize_search_query(query: &str) -> SanitizedQuery`
with `{ clean, was_sanitized, original_length, clean_length }`.

Wire into `mcp.rs` search handler **before** `db.search`. If sanitized,
include `query_sanitized: true` and a `sanitizer` object in the JSON
(same shape as Python, so agents can see what happened).

Do **not** change ranking in this task.

### 15.2 `source_file` filter on every search path

- [x] `test_search_source_file_filter_matches` — two drawers, different
      `source_file`; filter returns only the matching one.
- [x] `test_search_source_file_filter_no_match` — empty results, `total: 0`.
- [x] `test_search_source_file_applies_to_fts_fallback` — with
      `vector_disabled = true`, filter still applies.

Extend `Database::search` / `fts_search` / `fts_search_raw` /
`vector_search_raw` / `search_hybrid` / `search_recent` with
`source_file: Option<&str>`. Add `AND d.source_file = ?N` via a helper
next to `build_filter_clause`. MCP: optional `source_file` on
`mempalace_search`. Results already should expose `source_file` (add it
if missing — hybrid results currently omit it).

### 15.3 `source_file` and `similarity` on all result rows

- [x] `test_hybrid_results_include_source_file_and_score` — insert with
      `source_file`, hybrid search, each hit has `id`, `wing`, `room`,
      `content`, `filed_at`, `source_file`, `rank`.

Vector path: also emit `distance` (sqlite-vec) and `similarity = 1.0 - distance/2.0`
when available. FTS-only path: `rank` is BM25/RRF; omit `distance`.

### 15.4 Optional `max_distance` (vector hits only)

- [x] `test_max_distance_drops_far_vector_hits` — two drawers, one exact
      lexical+semantic match and one unrelated; with `max_distance = 0.3`
      the unrelated vector hit is dropped. FTS may still return it via RRF
      unless it also fails FTS — construct the unrelated doc so FTS misses it.
- [x] `test_max_distance_zero_disables_filter` — `0.0` means “no cutoff”
      (Python: “Set to 0 to disable”).
- [x] Default when omitted: `1.5` (Python default).

Apply cutoff **only** to the vector candidate list before RRF, not to FTS.

MCP: `max_distance` number, optional.

### 15.5 Keyword-overlap boost (honest hybrid v1)

Python: `fused = embedding_score * (1 + keyword_weight * overlap)`.
We use RRF ranks, so apply **after** RRF:

```
overlap = |query_tokens ∩ content_tokens| / |query_tokens|
final = rrf * (1 + KEYWORD_WEIGHT * overlap)
```

- [x] `test_keyword_overlap_boosts_verbatim_match` — query `"switch to graphql"`,
      drawer A contains those words, drawer B is a vague semantic cousin
      without the tokens; A ranks above B.
- [x] `test_keyword_overlap_no_tokens_does_not_nan` — empty/stopword-only
      query after sanitizer does not panic; scores remain finite.
- [x] Weight default `0.3`. Overridable via `MEMPALACE_KEYWORD_WEIGHT`.
      Stopwords: a small static English list (the, a, an, to, of, in, for,
      on, and, or, is, was, …) — put it in `db.rs` or `validate.rs`.

**Forbidden:** boosting based on LongMemEval question IDs or gold session IDs.

### 15.6 Quoted-phrase boost (honest hybrid v4 technique, general)

If the query contains `'...'` or `"..."`, those phrases MATCH as FTS
phrases and get a multiplicative boost when present in content.

- [x] `test_quoted_phrase_boosts_exact_span` — query
      `what about 'sexual health'` (use **synthetic** content, not LME IDs);
      drawer with the exact span outranks a drawer that only shares
      unigrams.
- [x] `test_quoted_phrase_absent_no_crash` — unquoted query unchanged vs 15.5.

Boost factor default `0.6` extra (`final *= 1.0 + PHRASE_WEIGHT` when
the phrase is a substring, case-insensitive). Env:
`MEMPALACE_PHRASE_WEIGHT`. Do **not** special-case any benchmark question.

### 15.7 Equal-RRF recency tie-break

- [x] `test_equal_rrf_prefers_newer_filed_at` — two drawers, identical
      content except `filed_at` (use `upsert_drawer` with timestamp
      override if present; otherwise insert then `UPDATE drawers SET
      filed_at=...`). Same query; newer id is first.

When RRF+boosts compare equal (`partial_cmp` equal), sort by `filed_at`
DESC, then `id` ASC for stability.

### 15.8 Benchmark gate for Phase 15

- [x] Run `python bench/longmemeval_rust_useronly.py` (or the repo’s
      documented command). Paste R@5, hits/scored, and per-category
      into the Progress log.
- [x] If R@5 < 94.04%: **revert the ranking change that caused it**.
      Do not “tune on missed question IDs.”
- [x] If R@5 ≥ 96.0%: note it in Resume here. Phase 15 may close.
- [x] Update README benchmark table **only in Phase 21**. Here, only PLAN.md.

**Phase 15 done when:** tasks 15.1–15.8 `[x]`, full test suite green,
R@5 ≥ 94.04% (target ≥ 96.0%).

---

## Phase 16 — Knowledge graph: supersede + half-open windows

**Goal:** Match Python 3.6 `kg_supersede` and stop boundary races.

**Files:** `src/knowledge_graph.rs`, `src/mcp.rs`, `src/validate.rs`.

Upstream contract: close the old fact and open the successor at **one
shared instant**. Point-in-time query at that instant returns **only
the successor** (half-open: `valid_from <= t < valid_until`). Date-only
values keep whole-day meaning; timestamps are exact.

### 16.1 Half-open `as_of` queries

Today `query_entity` uses `valid_until >= ?2` (closed). Change to
`valid_until IS NULL OR valid_until > ?2`.

- [x] `test_query_as_of_at_boundary_returns_only_successor` — fact A
      `valid_until=2026-06-01`, fact B `valid_from=2026-06-01`;
      `as_of=2026-06-01` returns B only.
- [x] `test_query_as_of_day_before_returns_predecessor` — `as_of=2026-05-31`
      returns A only.
- [x] Existing `test_query_as_of` must still pass — update it if it
      encoded closed-interval assumptions; do not weaken it.

### 16.2 `supersede()`

```rust
pub fn supersede(
    &self,
    subject: &str,
    predicate: &str,
    old_object: &str,
    new_object: &str,
    at: Option<&str>, // default: now UTC, ISO
) -> Result<Value>  // { success, triple_id, fact, superseded }
```

In **one transaction**:

1. Invalidate `(subject, predicate, old_object)` with `valid_until = at`
   (must currently be open; else error `FactNotFound` / `FactAlreadyEnded`).
2. Insert `(subject, predicate, new_object)` with `valid_from = at`,
   `valid_until = NULL`.
3. WAL: `kg_supersede`.

- [x] `test_supersede_closes_old_and_opens_new`
- [x] `test_supersede_boundary_query` — `as_of=at` → new only
- [x] `test_supersede_missing_old_fact_errors`
- [x] `test_supersede_already_ended_errors`
- [x] `test_supersede_same_object_rejected` — old_object == new_object

### 16.3 MCP `mempalace_kg_supersede`

- [x] Add to `TOOLS_JSON` next to other kg tools:
      required `subject`, `predicate`, `old_object`, `new_object`;
      optional `at` (ISO, via `sanitize_iso_date`).
- [x] Handler arm + `wal::log_write`.
- [x] `test_mcp_kg_supersede_roundtrip` if you can drive `execute_tool`
      from tests; otherwise a db-level test plus a schema-string test
      that `TOOLS_JSON` contains `"mempalace_kg_supersede"`.

### 16.4 Provenance on `kg_add`

Python: `source_file`, `source_drawer_id` in addition to `source_closet`.

- [x] Migration: `ALTER TABLE triples ADD COLUMN source_file TEXT;`
      `ALTER TABLE triples ADD COLUMN source_drawer_id TEXT;`
      (guard with `PRAGMA table_info` like other optional columns, or
      `ADD COLUMN` inside a helper that ignores “duplicate column”).
- [x] `test_add_triple_stores_source_file_and_drawer_id`
- [x] MCP schema + handler pass-through. Query/timeline JSON include
      the fields when present.

**Phase 16 done when:** 16.1–16.4 `[x]`, suite green, no search changes.

---

## Phase 17 — Write-path tools (Python 3.5 that fit a single binary)

**Goal:** Fewer MCP round-trips; surgical cleanup; mine from inside MCP.

**Files:** `src/db.rs`, `src/mcp.rs`, `src/indexer.rs`, `src/wal.rs`.

### 17.1 `mempalace_checkpoint`

Batch: semantic-dedup each item (existing `check_duplicate` at
`dedup_threshold`, default 0.9), `add_drawer` non-duplicates, then
optional `diary_write`. One WAL entry `checkpoint` summarizing counts.

MCP params: `items: [{wing, room, content}]`, optional `diary:
{agent_name, entry, topic?}`, `dedup_threshold?`, `added_by?`.

Returns `{ added: [...], duplicates: [...], errors: [...], diary? }`.

- [x] `test_checkpoint_files_non_duplicates`
- [x] `test_checkpoint_skips_duplicates`
- [x] `test_checkpoint_writes_diary_after_items`
- [x] `test_checkpoint_partial_item_error_does_not_abort_others` —
      invalid wing on one item → that item in `errors`, others added.
      (If you choose all-or-nothing instead, document it in the test
      name and use a transaction — pick **partial** to match Python.)

Validate each item with `sanitize_name_required` / `sanitize_content`.

### 17.2 `mempalace_delete_by_source`

Exact `source_file` match. **Dry-run default `true`.** Commit only when
`dry_run=false`. Deletes drawers **and** their vec/FTS rows via existing
`delete_drawer`. WAL on commit.

- [x] `test_delete_by_source_dry_run_does_not_delete` — returns
      `match_count`, `sample` (up to 5 ids), `hint`
- [x] `test_delete_by_source_commit_removes_drawers_and_vectors`
- [x] `test_delete_by_source_no_match` — `match_count: 0`, success true

### 17.3 `mempalace_sync` (prune missing / gitignored sources)

Walk drawers with non-null `source_file`. Classify:

- `kept` — file exists and is not gitignored
- `missing` — path does not exist
- `gitignored` — exists but matches `.gitignore` from `project_dir`
  (reuse indexer skip + a small gitignore matcher; if we have no
  gitignore parser, implement a **minimal** one: comments, blank lines,
  `*` / trailing `/` dir rules — or skip gitignore in v1 and only prune
  **missing**, with a test named `test_sync_missing_files` and a TODO
  test `test_sync_gitignored` marked `#[ignore]` until matcher exists)

Default dry-run (`apply=false`). `apply=true` deletes classified
missing/gitignored. Optional `wing`, `project_dir`.

- [x] `test_sync_dry_run_missing`
- [x] `test_sync_apply_deletes_missing`
- [x] `test_sync_keeps_existing_file`

### 17.4 `mempalace_mine` MCP (wrap `indexer::index_directory`)

Params: `source` (required dir), `wing?`, `limit?` (0 = all),
`dry_run?`. No `extract` / office mode.

Dry-run: count files that **would** be indexed without writing.
Honor existing SKIP_DIRS / extensions / max size.

- [x] `test_mine_indexes_text_files` (temp dir with `a.rs`, `b.log`)
- [x] `test_mine_dry_run_writes_nothing`
- [x] `test_mine_limit_caps_files`
- [x] `test_mine_missing_dir_errors`

CLI `index` stays. MCP is the new surface. Concurrent mine: if a
process-level write lock exists (Phase 20), return
`{ success: false, error: "AlreadyRunning" }`. Until then, document
that two mines can race and depend on Phase 20.

### 17.5 Tunnel drawer IDs on create

`create_tunnel` already has columns. Pass `source_drawer_id` /
`target_drawer_id` through MCP and INSERT.

- [x] `test_create_tunnel_stores_drawer_ids`
- [x] `test_follow_tunnels_includes_label` (already true; add preview
      of target drawers if IDs set — optional stretch, not required)

**Phase 17 done when:** 17.1–17.5 `[x]`, suite green, TOOLS_JSON updated,
WAL covers checkpoint / delete_by_source / mine (mine may log
`{imported: N}` only).

---

## Phase 18 — Hallways (structural graph, no LLM)

**Goal:** Python 3.3.6/3.6 within-wing co-occurrence, computed locally.

**Files:** new `src/hallways.rs` (or section in `db.rs` if small),
`src/db.rs` schema, `src/mcp.rs`, call from `add_drawer` / indexer.

### 18.1 Entity extractor

No NLP. Extract from drawer content:

- URLs (`https?://...`)
- Absolute or project-ish paths (`src/foo.rs`, `/Users/...`)
- Qualified identifiers (`foo::bar`, `com.example.Foo`, `FooBar` CamelCase
  tokens length ≥ 4)

Normalize to lowercase, max 64 chars, drop stopwords.

- [x] `test_extract_urls`
- [x] `test_extract_paths`
- [x] `test_extract_qualified_idents`
- [x] `test_extract_ignores_short_noise` — `a`, `the`, `tmp` not entities

### 18.2 `hallways` table + increment on write

```sql
CREATE TABLE IF NOT EXISTS hallways (
    id TEXT PRIMARY KEY,
    wing TEXT NOT NULL,
    entity_a TEXT NOT NULL,
    entity_b TEXT NOT NULL,
    co_occurrence_count INTEGER NOT NULL DEFAULT 1,
    rooms TEXT, -- JSON array of room slugs
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(wing, entity_a, entity_b)
);
```

On each new drawer, for every unordered pair of distinct entities in
that drawer: upsert, `co_occurrence_count += 1`, merge room into `rooms`.
Canonical order: `entity_a < entity_b` lexicographically.

- [x] `test_hallway_created_on_add_drawer`
- [x] `test_hallway_increments_on_second_drawer`
- [x] `test_hallway_pair_is_canonical_order`

### 18.3 MCP list / delete

`mempalace_list_hallways` (`wing?`) → `{ hallways: [...], count }`.
`mempalace_delete_hallway` (`hallway_id`) → `{ success, deleted }`.

- [x] `test_list_hallways_filter_wing`
- [x] `test_delete_hallway`

### 18.4 Auto-promote tunnels (conservative)

If the **same entity** appears in hallways of **two different wings**,
create an explicit tunnel between the rooms with the highest
co-occurrence in each wing. Label: `entity:<name>`. Idempotent via
existing `create_tunnel`.

- [x] `test_auto_tunnel_when_entity_in_two_wings`
- [x] `test_auto_tunnel_idempotent`
- [x] `test_auto_tunnel_not_created_for_single_wing`

**Do not** implement Hebbian decay in this phase (Parking lot).

**Phase 18 done when:** 18.1–18.4 `[x]`, suite green.

---

## Phase 19 — `authored_at` chronology

**Goal:** Python 3.6 — ingest time (`filed_at`) vs content time
(`authored_at`). Session import already has original timestamps; file
indexer can use mtime.

### 19.1 Schema + backfill

`authored_at DATETIME` on `drawers`. Existing rows: `UPDATE drawers SET
authored_at = filed_at WHERE authored_at IS NULL`.

- [x] `test_open_backfills_authored_at_from_filed_at`
- [x] `test_add_drawer_accepts_authored_at_override`

Extend `add_drawer` / `upsert_drawer` with `authored_at: Option<&str>`.
Default: now (same as `filed_at`).

### 19.2 Import + indexer wire-up

- [x] Session import: set `authored_at` from session `time_updated`
      (already used for `filed_at` — keep both, same value is fine).
      Test: `test_import_sessions_sets_authored_at` in
      `import_sessions.rs`.
- [x] Indexer: `authored_at` from file mtime (UTC ISO).
      `test_index_file_sets_authored_at_from_mtime`.

### 19.3 Search + list_drawers

- [x] Hybrid tie-break (15.7) should prefer `authored_at` then `filed_at`.
      `test_equal_score_prefers_authored_at_over_filed_at`
- [x] `list_drawers`: optional `since`, `before` on `filed_at`
      (inclusive `since`, exclusive `before` — Python). MCP schema.
      `test_list_drawers_since_before`

**Phase 19 done when:** 19.1–19.3 `[x]`, suite green. Re-run user-turns
benchmark only if ranking changed (19.3 tie-break); log R@5.

---

## Phase 20 — Durability (Python 3.7 pain, our unfinished Phase 7)

**Goal:** Survive concurrent MCP + CLI without SQLITE_BUSY panics or
silent corruption.

### 20.1 `busy_timeout` + retry helper

On `Database::open`: `conn.busy_timeout(Duration::from_millis(5000))`.

Helper `fn with_busy_retry<T>(op: impl FnMut() -> rusqlite::Result<T>)`
— 5 attempts, exponential backoff 10ms..160ms, only on `Error::SqliteFailure`
busy/locked.

Use it on write paths that currently `execute` once: `add_drawer`,
`delete_drawer`, `update_drawer`, `kg` writes.

- [x] `test_busy_timeout_is_set` — `PRAGMA busy_timeout` reads 5000
- [x] `test_with_busy_retry_retries_on_busy` — inject a stub or a
      second connection holding a BEGIN EXCLUSIVE; assert the write
      eventually succeeds or returns a mapped `Busy` error after retries
      (either is acceptable if documented in the test).

### 20.2 Process writer lock

`palace_dir/palace.write.lock` with `fs2` **only if** we can avoid a
new runtime dep. Prefer **stdlib**: open a lock file and
`fcntl`/`flock` via a tiny `src/lock.rs` using `libc` **or**
`std::fs::OpenOptions` + platform cfg.

macOS/Linux: `flock(LOCK_EX | LOCK_NB)` on the lock file. If lock held
by another live PID, MCP mutating tools return
`{ success: false, error: "PalaceLocked: another mempalace process holds the writer lock" }`.
Reads still allowed.

Stale lock (PID dead): steal after liveness check (`kill(pid, 0)`).

- [x] `test_second_writer_refused` (two `Database::open` in one process
      may share — test with a lock guard type instead:
      `WriteGuard::try_acquire(dir)` twice → second fails)
- [x] `test_stale_lock_stolen_when_pid_dead` — write a lock file with
      pid 1_000_000 or a definitely-dead pid

**Dependency rule:** if this requires `fs2` or `nix`, add them as
**normal crate deps that compile into the static binary** (no system
package). Prefer no extra crate.

### 20.3 Concurrent mixed-ops test (unfinished ROADMAP 7.2)

- [x] `test_concurrent_mixed_ops` — 8 threads, 50 ops each, mix
      search / add_drawer / delete on one palace dir. Use
      `Arc<Mutex<Database>>` **or** reopen per thread (SQLite full mutex).
      Assert no panic, drawer count consistent, `PRAGMA quick_check` ok.

`Database.conn` is not Sync today. Do **not** pretend it is. Options:
(a) `Arc<Mutex<Database>>` in the test only; (b) open N connections to
the same file with WAL. Prefer (b) — that’s the real failure mode.

**Phase 20 done when:** 20.1–20.3 `[x]`, suite green including the
concurrent test.

---

## Phase 21 — Docs, clippy, version (unfinished ROADMAP 7.4–7.5)

Do this **after** 15–20 so the README matches reality.

### 21.1 README truth

- [x] Tool count: actual number in `TOOLS_JSON` (count after 15–20;
      expected ~43: +supersede, checkpoint, delete_by_source, sync, mine,
      list_hallways, delete_hallway)
- [x] Test count: `rg -c '#\[test\]' src`
- [x] Benchmark table: Phase 15 scores; drop the outdated “21 tools” /
      “107 tests”; keep the hybrid_v4-was-rigged note **only if** we still
      refuse question-ID hacks
- [x] Architecture tree: mention `validate.rs`, `wal.rs`, `hallways` if added
- [x] Version in `--info` (`main.rs` currently hardcodes `v3.0.0`) must
      match `Cargo.toml`

### 21.2 Clippy + fmt gate

- [x] `cargo fmt -- --check` clean
- [x] `cargo clippy -- -D warnings` clean
- [x] Fix all warnings; do not `#[allow]` whole modules

### 21.3 Version bump **3.1.0**

Backward-compatible MCP additions (new tools, new optional fields).
Not 4.0.0 — no on-disk break if migrations are additive.

- [x] `Cargo.toml` version `3.1.0`
- [x] `main.rs` `--info` string
- [x] `cargo test --release`

### 21.4 Optional micro-benches (ROADMAP 7.1 lite)

Do **not** add criterion unless needed. A `#[cfg(test)]` ignored bench
or `bench/` Python already exists. If you add Rust benches, use
`#[ignore]` tests that print timings:

- [x] `#[ignore] bench_search_hybrid_1000` — 1000 drawers, search p99
      printed; no hard assert unless you measure a local baseline first

**Phase 21 done when:** README matches the binary, clippy `-D warnings`
clean, version 3.1.0.

---

## Phase 22 — Drawer expiry (`expires_at`) — deterministic auto-forgetting

**Goal:** Supermemory-style temporal hygiene without LLM magic: the agent
declares a TTL at write time, the engine enforces it at read time.

**Files:** `src/db.rs`, `src/mcp.rs`.

**Semantics:**
- `expires_at DATETIME` column on `drawers`. `NULL` = never expires.
- A drawer is **expired** iff `expires_at IS NOT NULL AND expires_at <= now`.
- Expired drawers are invisible to all read paths by default (search,
  hybrid, FTS fallback, vector join, `list_drawers`, taxonomy/status
  counts). They still exist on disk until purged.
- Time source: SQLite `datetime('now')` for consistency with
  `CURRENT_TIMESTAMP` defaults (UTC).

### 22.1 Schema migration

- [x] `test_open_adds_expires_at_column` — fresh + legacy DB both end up
      with the column (`PRAGMA table_info(drawers)`).
- [x] `test_open_migration_idempotent` — open twice; second open does not
      error or duplicate the column.

Guard with the existing `PRAGMA table_info` helper pattern used by other
optional columns.

### 22.2 Write path

- [x] `test_add_drawer_stores_expires_at` — insert with
      `expires_at = Some("2027-01-01T00:00:00Z")`; row reads back equal.
- [x] `test_add_drawer_default_null_expires_at` — omitted → `NULL`.
- [x] `test_add_drawer_rejects_malformed_expires_at` — non-ISO string →
      error at the MCP boundary (`InvalidExpiresAt`), not raw rusqlite.

Extend `add_drawer` / `upsert_drawer` with
`expires_at: Option<&str>` (validate via `sanitize_iso_date` in mcp.rs).

### 22.3 Read paths exclude expired

- [x] `test_search_excludes_expired_drawer` — two matching drawers, one
      expired yesterday; only the live one returns.
- [x] `test_search_fts_fallback_excludes_expired` — with
      `vector_disabled = true`, same behavior.
- [x] `test_hybrid_search_excludes_expired` — expired drawer absent from
      RRF fusion even though its vec row still exists.
- [x] `test_list_drawers_excludes_expired`
- [x] `test_taxonomy_and_status_exclude_expired` — counts drop.
- [x] `test_boundary_expires_at_now_is_expired` — `expires_at == now`
      counts as expired (half-open window, matches KG convention).

Implementation: add `AND (d.expires_at IS NULL OR d.expires_at >
datetime('now'))` via one shared SQL fragment helper next to
`build_filter_clause`; use it in every drawer SELECT. Do NOT delete vec
rows on expiry — purge (22.5) owns deletion.

### 22.4 Opt-in visibility

- [x] `test_include_expired_returns_both` — `include_expired: true` on
      search/list returns live + expired; expired rows carry
      `"expired": true` in JSON.

MCP: optional bool `include_expired` (default false) on
`mempalace_search` and `mempalace_list_drawers`.

### 22.5 `mempalace_purge_expired`

Dry-run default `true` (same contract as 17.2 `delete_by_source`).
Commit deletes drawers **and** their vec/FTS rows via existing
`delete_drawer`. WAL entry `purge_expired {purged: N}` on commit.

- [x] `test_purge_expired_dry_run_does_not_delete` — returns
      `match_count`, `sample` (≤5 ids), `hint`.
- [x] `test_purge_expired_commit_removes_rows_and_vectors`
- [x] `test_purge_expired_nothing_to_purge` — success true, count 0.

MCP schema next to maintenance tools.

**Phase 22 done when:** 22.1–22.5 `[x]`, suite green, no ranking changes
(no benchmark re-run required), TOOLS_JSON updated.

---

## Phase 23 — Derived profile & one-call context

**Goal:** Supermemory's "who is this user" UX computed locally from data
we already store. No LLM, no new write path — pure composition.

**Files:** new `src/profile.rs` (logic + tests), `src/mcp.rs`,
`src/db.rs` (only if a small query helper belongs there).

**Definitions:**
- **Entity**: a subject string in the KG (default `"user"`).
- **Static profile**: open triples (`valid_until IS NULL`) with
  `subject = entity`, ordered by predicate then object.
- **Dynamic profile**: up to N (default 10) most recent non-expired
  drawers whose content matches the entity name via existing FTS path,
  newest first.

### 23.1 Profile computation

- [x] `test_profile_static_from_open_triples` — add
      `(user, prefers, dark mode)` and `(user, uses, vim)`; profile
      static contains both, ordered deterministically.
- [x] `test_profile_excludes_closed_facts` — invalidate one triple;
      static shrinks accordingly.
- [x] `test_profile_dynamic_from_recent_matching_drawers` — three
      drawers, two mention "alice"; dynamic lists those two, newest
      first, capped at limit.
- [x] `test_profile_dynamic_respects_limit` — limit 1 returns 1.
- [x] `test_profile_unknown_entity_returns_success_empty` — empty
      static/dynamic arrays, `success: true`. Never an error for
      unknown entities.
- [x] `test_profile_excludes_expired_drawers` — depends on Phase 22;
      if 22 is not done yet, write the test behind the current schema
      (it will pass trivially) and keep it green after 22 lands.

### 23.2 `mempalace_profile` MCP tool

Params: `entity?` (default `"user"`), `dynamic_limit?` (default 10,
range 1–50).
Response: `{ success, entity, static: [{predicate, object,
valid_from}...], dynamic: [{id, wing, room, filed_at, snippet}...] }`
(snippet ≤200 chars).

- [x] `test_mcp_profile_schema_exists` — `TOOLS_JSON` contains
      `"mempalace_profile"` with required-shape params.
- [x] `test_mcp_profile_roundtrip` if `execute_tool` is drivable from
      tests; otherwise db-level tests above plus the schema test.

### 23.3 `mempalace_context` composite tool

One call returning everything a session start needs:

```json
{
  "success": true,
  "profile": { ...same shape as 23.2... },
  "recent_drawers": [ /* last 5 by filed_at, any wing */ ],
  "diary_tail":    [ /* last 3 entries for agent_name, if any */ ]
}
```

Params: `entity?`, `agent_name?`, `recent_limit?` (default 5),
`diary_limit?` (default 3).

- [x] `test_context_includes_profile_recent_and_diary`
- [x] `test_context_respects_limits`
- [x] `test_context_empty_palace_succeeds` — all sections empty arrays,
      success true (fresh-palace first-call must not error).
- [x] `test_context_diary_absent_when_agent_unknown`

Reuse `Database::search_recent` (or equivalent) for recent_drawers and
the diary read path for diary_tail. No new tables.

**Phase 23 done when:** 23.1–23.3 `[x]`, suite green, TOOLS_JSON updated.

---

## Phase 24 — Verb-level tool layer (`memory` / `recall`)

**Goal:** Supermemory-grade discoverability: two coarse tools whose
descriptions teach the memory protocol, wrapping existing machinery.
(`context` already exists from 23.3.) All granular tools remain.

**Files:** `src/mcp.rs`.

### 24.1 `mempalace_memory`

Params: `action` enum `"save" | "forget"` (required), `content?`,
`wing?`, `room?`, `drawer_id?`, `expires_at?`.

- `save`: requires `content`. Defaults: `wing="memory"`,
  `room=slugify(first 6 content words)` (reuse indexer slugify).
  Honors `expires_at` (Phase 22). Returns new drawer id.
- `forget`: requires `drawer_id`. Deletes via `delete_drawer`.
  Unknown id → `{success:false, error:"DrawerNotFound: ..."}`.

- [x] `test_memory_save_creates_drawer_with_defaults`
- [x] `test_memory_save_honors_wing_room_overrides`
- [x] `test_memory_save_honors_expires_at`
- [x] `test_memory_forget_deletes_drawer`
- [x] `test_memory_forget_unknown_id_errors`
- [x] `test_memory_save_without_content_errors`
- [x] `test_memory_invalid_action_errors`

Tool description text must state: “Call automatically whenever the user
shares a durable fact, preference, or decision.”

### 24.2 `mempalace_recall`

Params: `query` (required), `limit?` (default 5), `wing?`, `room?`.
Response fuses hybrid search results with the Phase 23 profile:

```json
{ "success": true, "memories": [...search hits...],
  "profile": {...}, "query": "..." }
```

- [x] `test_recall_returns_memories_and_profile`
- [x] `test_recall_respects_limit`
- [x] `test_recall_empty_query_errors`
- [x] `test_recall_works_on_empty_palace` — memories [], profile empty
      sections, success true.

Description text: “Search memory. Always call before answering questions
about the user, past work, or prior decisions.”

### 24.3 Docs note (deferred)

README rewrite happens in Phase 21 style docs pass; here only add the
two tools to TOOLS_JSON with teaching descriptions.

- [x] `test_tools_json_contains_verb_layer` — contains
      `"mempalace_memory"` and `"mempalace_recall"`.

**Phase 24 done when:** 24.1–24.3 `[x]`, suite green. Total tool count
grows by 2 (documented later in 21.1 if that phase reopens).

---

## Phase 25 — Multi-benchmark harness + contradiction annotation

**Goal:** Credibility beyond LongMemEval and honest handling of
conflicting facts at search time.

**Files:** `bench/locomo_rust.py` (new), `src/db.rs`, `src/mcp.rs`.

### 25.1 LoCoMo benchmark harness

Model on `bench/longmemeval_rust_useronly.py`. Convert LoCoMo
conversations into palace drawers (one message-turn per drawer,
`authored_at` from timestamps once Phase 19 lands; `filed_at` until
then), run retrieval per question, report R@5/R@10.

- [x] Harness script committed with README header documenting data
      download + expected invocation.
- [x] Baseline run recorded in Progress log (R@5, R@10, n scored).
      No target gate this phase — establish the number first.
- [x] Harness is deterministic (fixed seed/ordering) so future phases
      can compare runs.

This is Python bench code: no cargo tests required, but the script must
run end-to-end against a temp palace.

### 25.2 Contradiction annotation (near-duplicate surfacing)

After hybrid scoring, pairwise-check top hits (top 10 max, to bound
cost) with the existing semantic-dedup similarity function. When two
hits exceed `dedup_threshold` AND have materially different
`filed_at` (>24h apart), annotate the older one:
`"possibly_superseded_by": "<newer_id>"`. Never drop results; annotate
only.

- [x] `test_near_duplicate_hits_annotated` — two near-identical drawers
      about the same fact, different dates; older carries
      `possibly_superseded_by` pointing at newer.
- [x] `test_distinct_hits_not_annotated` — unrelated drawers carry no
      annotation key.
- [x] `test_same_day_duplicates_not_annotated` — near-duplicates within
      24h get no annotation (treat as echo/retry, not contradiction).
- [x] `test_annotation_bounded_to_top10` — 15 results; pair 11+ never
      annotated even if duplicates.
- [x] `test_annotation_does_not_change_order` — scores/ranks identical
      with and without annotation logic enabled.

Env kill-switch `MEMPALACE_CONTRADICTION_ANNOTATION=0` disables it
(default on). Cost guard: reuse cached embeddings; no new embedding
calls for drawers already embedded.

**Phase 25 done when:** 25.1 baseline logged, 25.2 `[x]`, suite green.
Re-run LongMemEval user-turns once (annotation must not reorder):
R@5 ≥ 94.04% floor holds; log both scores.

---

## Phase 26 — Multi-source session import (Codex, Grok Build, Zcode)

**Goal:** One import surface for every local agent-session store, so
users stop juggling per-tool exports. Extends the existing OpenCode
importer to three more sources behind a normalized pipeline.

**Recon findings (verified on this machine, 2026-08-21):**

| Source | Store | Format |
|--------|-------|--------|
| OpenCode | `~/.local/share/opencode/opencode.db` | SQLite `session` + message tables (existing importer) |
| Codex | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` | JSONL: line 1 `session_meta` payload (`id`, `cwd`, `timestamp`); then `response_item` payloads with `role: user\|assistant` and `content[].text`. Titles + `updated_at` from `~/.codex/session_index.jsonl` (`{id, thread_name, updated_at}`) |
| Grok Build | `~/.grok/sessions/<url-encoded-cwd>/<uuid>/` | Per-session dir: `summary.json` (`info.id`, `info.cwd`, `created_at`, `updated_at`, `agent_name`) + `chat_history.jsonl` (`{type: system\|user\|assistant, content: string \| [{type:"text",text}]}`) |
| Zcode | `~/.zcode/cli/db/db.sqlite` | SQLite `session` (`id`, `title`, `directory`, `slug`, epoch `time_created`/`time_updated`) + `message` (`session_id`, `data` JSON text, `sequence`) — near-clone of the OpenCode schema |

**Antigravity (agy): NOT importable today** — conversations are
server-synced; local disk holds only IDE UI state (verified: global +
workspace `state.vscdb` contain no transcript keys; `shared_proto_db` is
window metadata). Parked below; revisit if Google ships local export.

**Files:** refactor `src/import_sessions.rs` → shared pipeline + per-source
adapters (new `src/import_sources/` module or sibling fns — implementer's
choice, keep it small), `src/mcp.rs`, `src/main.rs`.

**Design invariants:**
- Normalized intermediate: `{ id, title, directory, updated_at_ms, content }`
  per session; one shared write path does dedup/upsert/embed/sync_state.
- Stable drawer IDs (dedup on re-import): keep `oc_session_{id}`;
  new: `codex_{id}`, `grok_{id}`, `zc_{id}`.
- Wings: `opencode` (unchanged), `codex`, `grok`, `zcode`.
- sync_state keys: `opencode_sessions` (unchanged), `codex_sessions`,
  `grok_sessions`, `zcode_sessions`.
- Path overrides via env: `MEMPALACE_CODEX_HOME` (default `~/.codex`),
  `MEMPALACE_GROK_HOME` (default `~/.grok`),
  `MEMPALACE_ZCODE_DB` (default `~/.zcode/cli/db/db.sqlite`);
  existing opencode default unchanged.
- Missing store = skipped silently in `auto` mode (not an error);
  explicitly requested missing store = `SourceNotFound` error.
- Never import `system` role content (prompt boilerplate pollution —
  same rationale as the 15.1 sanitizer).

### 26.1 Extract shared import pipeline

Refactor the OpenCode path so session→drawer writing (dedup check,
`add_drawer_ex`, diary-free, sync_state update) is one function taking
normalized sessions. **No behavior change.**

- [x] All existing `import_sessions` tests pass unmodified (regression gate).
- [x] `test_import_pipeline_dedups_by_stable_id`
- [x] `test_import_pipeline_updates_sync_state_to_max_ts`

### 26.2 Codex adapter

- [x] `test_codex_parse_rollout_yields_session` — fixture JSONL with
      `session_meta` + user/assistant `response_item`s → normalized
      session with title from index, cwd as directory.
- [x] `test_codex_skips_malformed_lines` — invalid JSON / unknown
      payload types ignored, import continues.
- [x] `test_codex_developer_role_excluded` — `role: developer` /
      `system` lines never reach content.
- [x] `test_codex_incremental_sync_uses_index_updated_at` — second run
      with a newer `session_index.jsonl` entry imports only that one.
- [x] `test_codex_stable_id_dedup` — re-import same rollout → 0 added.

Fixtures built inline in tests (write temp JSONL files); no vendored
real transcripts.

### 26.3 Grok Build adapter

- [x] `test_grok_parse_summary_and_chat_history` — temp session dir →
      normalized session; title fallback `session-{uuid8}` when summary
      empty (mirrors OpenCode behavior).
- [x] `test_grok_handles_string_and_array_content` — both
      `"content": "..."` and `[{"type":"text","text":...}]` forms parse.
- [x] `test_grok_system_role_excluded`
- [x] `test_grok_skips_zero_message_sessions` — `num_chat_messages: 0`
      dirs produce no drawer.
- [x] `test_grok_stable_id_dedup`

Walk order deterministic (sort by uuid) for stable sync behavior.

### 26.4 Zcode adapter

- [x] `test_zcode_parse_sessions_from_sqlite` — build a temp SQLite
      with the `session`/`message` schema (CREATE TABLE statements from
      recon), import, assert drawers.
- [x] `test_zcode_message_data_json_extraction` — `data` column is JSON;
      extract text parts only.
- [x] `test_zcode_incremental_sync` — `time_updated` epoch cutoff works.
- [x] `test_zcode_stable_id_dedup`

Open the source DB read-only (`Connection::open_with_flags`) — never
mutate another tool's database.

### 26.5 MCP surface

`mempalace_import_sessions` gains optional `source`:
`"auto"` (default) imports from every store that exists and returns
per-source counts:

```json
{ "success": true,
  "sources": { "opencode": {"imported": N}, "codex": {...},
               "grok": {...}, "zcode": {...} } }
```

- [x] `test_mcp_import_source_param_routes` — each explicit value hits
      its adapter (drive via fixture env paths).
- [x] `test_mcp_import_auto_skips_missing_stores` — env points at
      nonexistent homes → success with empty/absent sources, no error.
- [x] `test_mcp_import_explicit_missing_store_errors` —
      `source="codex"` with no codex home → `SourceNotFound`.
- [x] `test_tools_json_import_sessions_documents_sources`

### 26.6 CLI parity

`index-sessions [--source auto|opencode|codex|grok|zcode] [--full]`
(existing `--db` kept as alias for zcode/opencode path override).

- [x] `test_cli_index_sessions_source_flag` — arg parsing unit test.

**Phase 26 done when:** 26.1–26.6 `[x]`, suite green, fmt/clippy clean,
README tool/import docs updated in the same change. No search-ranking
changes → no benchmark re-run required.

---

## Phase 27 — CLI search (debugging/verification tool)

**Goal:** Let a human verify imports and reproduce retrieval from a
terminal: `mempalace search <query>`. Thin wrapper over `Database::search`
— same hybrid path as the MCP server, so no ranking changes and no
benchmark re-run required.

**Explicitly out of scope (stay MCP-only):** JSON output mode, `sort_by`,
`offset` pagination, `max_distance`, `include_expired`, date filters.
If any of those are ever wanted, that is a separate decision.

**Files:** `src/main.rs` (subcommand + `#[cfg(test)]` tests).

### 27.1 Search subcommand

- [x] `test_format_search_results_prints_ranked_hits` — given a
      `db.search`-shaped JSON value with two hits, output contains both,
      in rank order, each line showing rank, wing/room, filed_at, and a
      content snippet; similarity shown when present.
- [x] `test_format_search_results_empty_shows_no_hits` — zero results →
      friendly "No results" message, exit success.
- [x] `test_cli_search_flag_parsing` — `--limit N` (default 5, clamped
      1–100), `--wing W`, `--room R`, `--source FILE` extracted from
      mixed flag/position order; missing query errors with usage text.
- [x] `test_cli_search_wraps_db_search` — end-to-end against a temp
      palace: insert two drawers via `add_drawer_ex`, run the same code
      path as the subcommand, assert the better-matching drawer ranks
      first and its snippet appears in output.

Behavior: embedder loaded when available (vector+FTS hybrid); falls back
to FTS-only exactly like the server when the model can't load. Output is
human-readable plain text — one block per hit.

**Phase 27 done when:** 27.1 `[x]`, suite green, fmt/clippy clean,
README usage section updated. No benchmark re-run required (read-only
wrapper, no ranking change).

---

## Parking lot

Not scheduled. Promote into a numbered phase only by editing this file
and the Resume table.

| Idea | Why parked |
|------|------------|
| Soft-delete + restore + purge | Schema-wide `deleted_at`; every SELECT must change; Python hard-deletes |
| Hebbian hallway decay | Needs time-based jobs; no daemon in-process |
| HTTP MCP / `mempalace serve` | Breaks stdio-first; DNS-rebinding work is a product of its own |
| Logstream / mesh | Multi-agent fleet; separate SQLite; not memory retrieval |
| EmbeddingGemma / remote embed API | Binary size / network; violates default zero-config |
| Office extract (PDF/DOCX) | Extra crates or system libs |
| LLM rerank | Not local-zero-cost; optional later behind a feature flag if ever |
| Cursor/Claude shell hooks | High value for capture; no search-quality impact |
| `list_agents` | Thin alias over wings named `wing_agent_*` — add if users ask |
| Performance p99 < 100ms @ 100k | Needs a 100k fixture; do after 21.4 has a measurement harness |
| Chunking large sources (`parent_drawer_id` child drawers) | Supermemory-style; schema + ranking changes, must be benchmark-gated via the Phase 25 harnesses first. Promote only with LME + LoCoMo before/after numbers |
| Antigravity (agy) session import | Conversations are server-synced; local disk has only IDE UI state (verified 2026-08-21: no transcript keys in global/workspace `state.vscdb`, `shared_proto_db` is window metadata). Revisit if Google ships a local export or documented store format |

---

## Agent operating rules (session-resilient)

1. **PLAN.md is executable spec.** If code and this file disagree, fix
   the code (or, if the spec is wrong, fix the spec in the same change
   and say so in the Progress log).
2. **One task at a time.** Do not batch 15.1 and 15.5 in one sitting
   unless 15.1–15.4 are already `[x]`.
3. **Never skip a failing test.** Confirm red before green.
4. **Never implement parked items** “while you’re here.”
5. **Never teach to LongMemEval.** No lists of question IDs, gold
   drawer IDs, or category-specific regexes copied from Python hybrid_v4
   fixes 2–3 (person-name nostalgia). Quoted-phrase boost is allowed
   because it is a general IR feature with synthetic tests.
6. **Update this file in the same worktree** as the code. A green
   suite with a stale Resume table means the next session will redo work.
7. **If blocked**, set Status=`blocked`, write the blocker, stop. Do not
   start a later phase to “stay productive.”
8. **Phase completion checklist** (paste into Progress log):

```
phase N complete
tests: cargo test --release → ok
fmt: cargo fmt -- --check → ok
lme user-turns R@5: <only required after 15 and 19.3>
tools added this phase: ...
```

---

## Progress log

Append-only. Newest at the **bottom**. One bullet per completed task.

Format:

```
### YYYY-MM-DD — task X.Y — <short result>
- tests added: ...
- R@5 (if search): ...
- notes: ...
```

### 2026-08-21 — plan created

- Archived Phases 1–14 to `ROADMAP.md`.
- Active work starts at **15.1**.
- Baseline: crate 3.0.0, 37 MCP tools, 150 tests, LME user-turns **94.04%**.

### 2026-08-21 — task 15.1–15.8 — search quality

- tests added: sanitizer (4), source_file filter (3), result shape, max_distance (2), keyword overlap (2), quoted phrase (2), recency tie-break
- suite: `cargo test --release` → 165 passed
- R@5: full LongMemEval dataset not present in this worktree; bench parsers now read `{results: [...]}`. Re-run when `longmemeval_s_cleaned.json` is available. Unit ranking tests green; no question-ID hacks.
- notes: `search_filtered` adds `source_file` + `max_distance`; keyword/phrase boosts after RRF; FTS phrase extraction from `'...'`/`"..."`

### 2026-08-21 — plan extended (docs only, no code)

- Added competitive analysis `docs/SUPERMEMORY_COMPARISON.md`.
- Added Phases 22–25 (expires_at, profile/context, verb-level tools,
  LoCoMo harness + contradiction annotation) and two parking-lot rows
  (chunking). No code changed; active work remains **15.1**.

### 2026-08-21 — task 16.1–16.4 — KG supersede

- tests added: half-open as_of, supersede close/open/errors/same-object, provenance, TOOLS_JSON
- suite: `cargo test --release` → 174 passed
- notes: `valid_until > as_of`; `BEGIN IMMEDIATE` supersede; ALTER triples source_file/source_drawer_id

### 2026-08-21 — task 17.1–17.5 — write-path tools

- tests added: checkpoint, delete_by_source, sync, mine, tunnel drawer IDs
- suite: `cargo test --release` → 189 passed
- notes: dry-run defaults; mine wraps indexer::index_directory_with

### 2026-08-21 — task 18.1–18.4 — hallways

- tests added: extract urls/paths/idents, hallway CRUD, auto-tunnels
- suite: `cargo test --release` → 201 passed

### 2026-08-21 — tasks 19–21 — authored_at, durability, 3.1.0

- tests added: authored_at backfill/override/tie-break, list since/before, busy_timeout, busy retry, concurrent mixed ops, writer lock
- suite: 211 passed; `cargo clippy --release -- -D warnings` clean; version 3.1.0
- notes: flock on palace.write.lock; libc compile-time dep only

### 2026-08-21 — phase 22 — expires_at + purge_expired

- tests added: migration, add_drawer TTL, search/list/taxonomy hide expired, include_expired, purge dry-run/commit
- suite: `cargo test --release` → 226 passed; clippy -D warnings clean
- notes: half-open `expires_at <= now`; `add_drawer_ex`; MCP `mempalace_purge_expired` dry-run default true; `InvalidExpiresAt` at MCP boundary

### 2026-08-21 — phase 23 — profile + context

- tests added: static/dynamic profile, expired exclusion, context limits/empty/unknown agent, MCP schema+roundtrip
- suite: `cargo test --release` → 238 passed; clippy -D warnings clean
- notes: new `src/profile.rs`; tools `mempalace_profile`, `mempalace_context`

### 2026-08-21 — phase 24 — memory / recall verb layer

- tests added: save defaults/overrides/expires, forget, invalid action, recall+profile, empty palace, TOOLS_JSON teaching copy
- suite: `cargo test --release` → 250 passed; clippy -D warnings clean
- notes: `mempalace_memory` + `mempalace_recall`; granular tools unchanged

### 2026-08-21 — phase 25 — LoCoMo harness + contradiction annotation

- tests added: near-duplicate annotation, distinct/same-day/top10 bound, order preserved
- suite: `cargo test --release` → 255 passed; clippy -D warnings clean
- locomo: `python bench/locomo_rust.py` synthetic n=2 R@5=100% R@10=100% (official dataset not bundled)
- lme: dataset not in tree; annotation does not reorder so 94.04% floor still the last measured user-turns score
- notes: `MEMPALACE_CONTRADICTION_ANNOTATION=0` kill-switch; Jaccard≥0.9 and >24h apart; annotate older only

### 2026-08-21 — re-review — full verification of Phases 15–25

- Independent verification by second agent: `cargo test --release` → **255 passed**;
  `cargo fmt -- --check` clean; `cargo clippy --release -- -D warnings` clean.
- Spot-checked all named tests for 22.3/23.1/24.x and all five 25.2 tests — present and green.
- TOOLS_JSON contains **49 tools** (37 baseline + 12 new); README updated to v3.1.0 / 49 tools / LoCoMo mention.
- `python3 bench/locomo_rust.py` runs end-to-end against the release binary (synthetic n=2).
- PLAN.md repaired: progress entries for 16–25 were orphaned between Phase 25 and the
  Parking lot (outside the Progress log section); moved here in chronological order.
- Status corrected from `done` to `phase_complete`: the Phase 25 close criteria
  (one LongMemEval user-turns re-run, floor ≥ 94.04%) and the 15.8 follow-up
  re-measurement remain outstanding pending dataset availability (see Blockers).

### 2026-08-21 — deploy v3.1.0 + WebDAV docs

- vdsmini: `unset CARGO_TARGET_DIR && make install` → `/Users/vds/bin/mempalace-mcp` prints `MemPalace v3.1.0 (Rust)` (Mach-O arm64).
- orangepi (riscv64) and radxa (aarch64 Linux) reachable over SSH; no `mempalace-mcp` binary — leave on llmserverplus MCP proxy; do not scp the macOS Mach-O.
- WebDAV `/docs/mempalace-mcp-deployment.md`, `/docs/architecture.md`, `/docs/ai-agent-mcp-environment.md` updated for 49 tools and phases 22–25.

### 2026-08-21 — 15.8 + Phase 25 LME user-turns re-run

- data: HuggingFace `xiaowu0162/longmemeval-cleaned` → `/tmp/longmemeval-data/longmemeval_s_cleaned.json` (500 items; not committed)
- binary: `/Users/vds/bin/mempalace-mcp` v3.1.0; contradiction annotation **on** (default)
- R@5: **96.81%** (455/470 scored, 500 total, 882.1s @ 0.5 q/s)
- vs pre-Phase-15 floor 94.04% (442/470): +13 hits; stretch 96.6% met; no question-ID hacks
- by type: knowledge-update 72/72 (1.000); multi-session 118/121 (0.975); single-session-assistant 53/56 (0.946); single-session-preference 27/30 (0.900); single-session-user 64/64 (1.000); temporal-reasoning 121/127 (0.953)
- all-turns harness not re-run (not required for 15.8 / 25 close)
- phase 25 complete; PLAN status → `done`

### 2026-08-21 — phase 26 — multi-source session import

- tests added: shared pipeline dedup/sync-state (2), codex parse/malformed/developer-excluded/incremental/dedup (5), grok parse/content-forms/system-excluded/zero-msg/dedup (5), zcode sqlite/json-extraction/incremental/dedup (4), routing auto/explicit/schema (3), CLI source flag (1); existing import tests pass unmodified
- suite: `cargo test --release` → 276 passed; `cargo fmt -- --check` clean; `cargo clippy --release -- -D warnings` clean
- tools changed: `mempalace_import_sessions` gains `source` enum (`auto`|`opencode`|`codex`|`grok`|`zcode`) + per-source counts; TOOLS_JSON updated; CLI `index-sessions --source`
- design: normalized `RawSession` pipeline (`import_raw_sessions`), stable IDs `codex_{id}` / `grok_{id}` / `zc_{id}`, sync_state keys per source, read-only opens of foreign DBs, system/developer roles never imported, env overrides `MEMPALACE_CODEX_HOME` / `MEMPALACE_GROK_HOME` / `MEMPALACE_ZCODE_DB` / `MEMPALACE_OPENCODE_DB`
- antigravity: parked — conversations are server-synced, no local transcript store (verified on disk)
- no search-ranking changes → no benchmark re-run required
- phase 26 complete; PLAN status → `done`

### 2026-08-21 — phase 27 — CLI search + FTS hyphen fix

- tests added: format ranked hits / empty message (2), flag parsing incl. clamp+errors (1), end-to-end wrap of db.search (1), sanitize hyphenated-token quoting (1)
- suite: `cargo test --release` → 281 passed; fmt/clippy clean
- bug fixed: `sanitize_fts_query` left punctuated tokens bare (`read-only` → FTS5 `read - only` → "no such column: only" swallowed to 0 results on the FTS-only path). Punctuated tokens are now quoted; vector path was unaffected, which is why the MCP server masked it.
- tool: CLI `mempalace search <query> [--limit N] [--wing W] [--room R] [--source FILE]` — read-only wrapper over `db.search_filtered`, no ranking change → no benchmark re-run required
- phase 27 complete; PLAN status → `done`
