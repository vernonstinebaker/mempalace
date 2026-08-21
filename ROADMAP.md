# MemPalace Roadmap — Historical Archive

**Active work lives in [PLAN.md](PLAN.md).** Do not execute new tasks from
this file. Phases 1–14 below are complete (with the leftovers called out).
The 2026-08-21 audit against Python MemPalace **v3.7.1** found this
roadmap still targeting upstream **v3.3.5**, with a dashboard that claimed
A-level completeness while several Phase 7 boxes were never closed.

---

## Status at archive time (2026-08-21)

| Dimension            | After Phases 1–14 | Next (see PLAN.md) |
|----------------------|-------------------|--------------------|
| Search quality       | A (RRF + recency; LME 94.04%) | Phase 15 — close gap vs Python 96.6% raw |
| Architecture         | A (single binary, sqlite-vec) | Keep; no extra backends |
| Test coverage        | B+ (~150 tests)   | Keep TDD on every new task |
| Code quality         | A- (fmt ok; clippy `-D warnings` not gated) | Phase 21 |
| Feature completeness | A- vs Python 3.3.5; **behind 3.7.1** | Phases 16–19 |
| Import pipeline      | A- (sessions + indexer CLI) | Phase 17 `mine` / `sync` MCP |
| Error handling       | B+                | Phase 20 busy/lock |
| Data integrity       | B                 | Phase 20 |
| Operations           | B                 | Phase 17 + 21 docs |

Crate version remains **3.0.0**. README still says “21 tools” / “107 tests”
(actually 37 tools / ~150 tests) — PLAN.md Phase 21.

---

## What 1–14 delivered

| Phase | Title | Outcome |
|-------|-------|---------|
| 1 | Test harness & error safety | TempDir tests, structured `log.rs`, import error propagation |
| 2 | Dedup query filters; graph tools → `db.rs` | `build_filter_clause`; traverse / find_tunnels / graph_stats tested |
| 3 | Search recency | `sort_by=relevance\|recency\|hybrid`, `filed_at` on hits |
| 4 | Session import quality | Richer session text, `mempalace_import_sessions` |
| 5 | Incremental session sync | `sync_state` table |
| 6 | Pagination, export, backup | offset, date range, export, backup/restore, list_recent |
| 6.5 | Soft-delete | **Not done** — parked in PLAN.md |
| 7 | Perf benches, clippy gate, semver | **Partial** — benches/clippy-as-gate/3.1.0 moved to PLAN 20–21 |
| 8 | Input sanitization | `src/validate.rs` |
| 9 | Vector health + FTS fallback | `probe_vec0_health`, `vector_disabled` |
| 10 | Write-ahead log | `src/wal.rs`, `mempalace_wal_log` |
| 11 | Tunnel CRUD | create/list/delete/follow (drawer IDs not wired — PLAN 17.5) |
| 12 | get_drawer / list_drawers | Done |
| 13 | `kg_add` `valid_to` | Done; **supersede** is PLAN 16 |
| 14 | repair / reconnect / integrity | Done |

---

## Why a new plan

Python MemPalace shipped **3.4 → 3.7.1** after our audit snapshot:

- Search: query sanitizer, `source_file` filter, date windows (we had
  `filed_*` already), keyword/phrase hybrid, `authored_at`
- KG: `kg_supersede`, half-open intervals
- Tools: `checkpoint`, `delete_by_source`, `sync`, `mine`, hallways
- Ops: writer leases, busy recovery, HTTP/logstream/backends — **rejected**
  for this port (single-binary constraint)

PLAN.md takes only what fits that constraint, is TDD-first, and has a
**Resume here** table so a new session can continue without chat history.

---

## Agent tracking

1. Open [PLAN.md](PLAN.md). Read **Resume here**.
2. Execute the active task with the TDD loop in PLAN.md §2.
3. Check boxes and append the Progress log **in PLAN.md**, not here.
4. Keep [AGENTS.md](AGENTS.md) in sync if process rules change.
