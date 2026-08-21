# MemPalace vs. Supermemory — Competitive Analysis & Adoption Plan

**Date:** 2026-08-21
**Status:** Analysis plus executed PLAN.md Phases 22–25 (expiry, profile/context, memory/recall, LoCoMo harness, contradiction annotation).
**Compared against:** [supermemoryai/supermemory](https://github.com/supermemoryai/supermemory) (29k★, MIT, TypeScript monorepo; core memory engine is closed-source, self-hosted binary ships the API)

---

## 1. What Supermemory is

Supermemory is a commercial "memory and context engine" for AI agents:

| Capability | Description |
|---|---|
| **Memory engine** | Automatically extracts facts from conversations; tracks knowledge updates, resolves contradictions ("I moved to SF" supersedes "I live in NYC"), auto-forgets expired facts |
| **User profiles** | Auto-maintained per-user profile with `static` (stable facts) and `dynamic` (recent activity) layers; one call, ~50ms; injected into system prompts via framework middleware |
| **Hybrid search** | RAG (documents) + Memory (facts) in a single query (`searchMode: hybrid \| memories`) |
| **Connectors** | Google Drive, Gmail, Notion, OneDrive, GitHub — cloud-only, real-time webhooks |
| **Multi-modal extractors** | PDFs, images (OCR), video transcription, AST-aware code chunking |
| **Scoping** | `containerTag` / project tags to separate work vs. personal vs. per-client contexts |
| **MCP surface** | Deliberately tiny: 3 tools — `memory` (save/forget), `recall` (search + profile summary), `context` (full profile injection) |
| **Benchmarks** | Claims #1 on LongMemEval (~81.6% accuracy-s / 95% R@15), LoCoMo, ConvoMem; ships open-source MemoryBench harness |
| **ASMR (research)** | Experimental agentic retrieval (~99% LME-s) using 11–15 LLM calls/question — explicitly not production |

Independent evaluations note their production engine scores lower than marketing claims (e.g., ~81.6% LongMemEval-s overall vs. Hindsight's reported 91.4%), and their backend engine is closed-source.

## 2. Head-to-head

| Dimension | MemPalace (Rust) | Supermemory | Edge |
|---|---|---|---|
| Deployment | Single ~16MB binary, zero runtime deps, stdio MCP | Cloud API or local server binary (Node-based, needs model config wizard) | **MemPalace** |
| Privacy / locality | Fully local, verbatim storage, no network | Local mode exists but pushes toward cloud; engine closed-source | **MemPalace** |
| Retrieval | Hybrid vec0 + FTS5/BM25 → RRF; 94.04% LME user-turns R@5 | Proprietary hybrid; 95% R@15 claimed (different metric — not directly comparable) | Even (different metrics) |
| Temporal reasoning | KG triples with validity windows; `authored_at` planned (Phase 19); drawer-level time decay in `hybrid` sort | First-class: contradiction resolution + auto-forgetting built into the write path | **Supermemory** |
| Fact lifecycle | Manual: agent must call `kg_invalidate` + `kg_add`; supersede planned (Phase 16) | Automatic extraction/update/expiry from raw conversation | **Supermemory** |
| User profiles | None as a concept (wings/rooms are organizational, not semantic summaries) | Static+dynamic profiles, one-call retrieval | **Supermemory** |
| MCP ergonomics | 37 granular tools — powerful but high agent cognitive load | 3 tools — trivially discoverable, agents use them reliably | **Supermemory** |
| Knowledge graph | Explicit triple store, temporal validity, tunnels/hallways planned | Similarity-based "graph memory", less rigorous than bi-temporal triples | **MemPalace** |
| Agent diaries / AAAK | Unique | None | **MemPalace** |
| Maintainability tooling | update_drawer, bulk_replace, reindex, backup/restore, WAL audit | Limited (cloud-managed) | **MemPalace** |
| Benchmarks | LongMemEval only, self-run | LongMemEval + LoCoMo + ConvoMem + open MemoryBench harness | **Supermemory** |
| Connectors / multimodal | File indexer + OpenCode session import only | Drive/Gmail/Notion/GitHub sync; PDF/OCR/video | Supermemory (mostly out of scope for us) |

**Summary:** MemPalace wins on architecture (local-first, single binary, explicit temporal KG, verbatim storage, maintainability). Supermemory wins on *product semantics*: it manages the fact lifecycle for the user (extract → update → expire) and exposes a radically simple agent-facing surface. Those two gaps are where the adoption opportunities are.

---

## 3. What we should adopt

Ranked by fit with our strategy filter (single binary, no LLM-in-the-loop by default, local-first, TDD).

### A. High value, fits constraints

#### A1. Profile view over the palace (`mempalace_profile`)
Supermemory's killer UX is "one call returns who the user is." We can approximate it **without any LLM**: derive a profile from data we already store.

- `static`: KG triples about the user entity with long validity and no `valid_to` (stable facts).
- `dynamic`: most recent N drawers in `wing_agent_*` / diary entries / recently-filed drawers.
- Optional query param fuses profile + top-k search results (their `recall` behavior).

New read-only MCP tool; pure composition of existing queries. No schema change required (maybe an index on triples by subject).

#### A2. Drawer expiry / auto-forgetting (`expires_at`)
Supermemory auto-forgets temporary facts ("exam tomorrow"). We can support this deterministically:

- Additive migration: `ALTER TABLE drawers ADD COLUMN expires_at DATETIME;`
- All search/list paths filter `expires_at IS NULL OR expires_at > now` (or annotate hits with `expired: true` and exclude by default).
- `add_drawer` accepts optional `expires_at`; a cheap maintenance command (`mempalace_purge_expired`, dry-run default) hard-deletes.
- Pairs naturally with Phase 19's `authored_at` work — same migration window.

This gives us Supermemory's temporal hygiene without their closed-source magic: the *agent* declares TTL at write time; the *engine* enforces it at read time.

#### A3. One-call context injection (`mempalace_context`)
Their `/context` pattern: session-start injection of profile + recent state. For us: a composite tool returning `{ profile (A1), recent_drawers (last N by filed_at), open_questions/diary_tail }` in one response. Pure aggregation; big win for token efficiency and agent adoption since it replaces 3–5 round-trips.

#### A4. Simplified top-level tool surface
37 tools is a discovery burden for small models. Keep all 37, but add 2–3 coarse tools whose descriptions teach the protocol:
- `mempalace_memory` (save/forget wrapper over add/delete/kg_supersede)
- `mempalace_recall` (search + profile, A1)
- `mempalace_context` (A3)

Tool descriptions are prompt engineering — Supermemory proves agents reliably use a 3-tool surface. Our granular tools remain for power users.

### B. Medium value

#### B1. Broaden benchmarks beyond LongMemEval
Adopt their MemoryBench idea (not their code): add LoCoMo and/or ConvoMed-style evals to `bench/`. Our current 94.04% R@5 is strong but single-benchmark; multi-benchmark coverage would be a genuine differentiator among local-first memory servers and guard against overfitting to LME question shapes.

#### B2. Contradiction surfacing at search time
When search returns multiple drawers that are near-duplicates semantically but differ in `filed_at`, annotate results (`superseded_candidate: true`) instead of silently ranking both. Cheap heuristic (existing dedup threshold machinery); helps agents resolve "which fact is current" without LLM rerank. Complements Phase 16 `kg_supersede`.

#### B3. Chunking large sources at index time
Supermemory chunks documents before embedding; we embed whole files/drawers. MiniLM's effective input is short — very large indexed files likely have degraded embeddings today. Consider splitting content >N tokens into linked child drawers (`parent_drawer_id` column) during `index`/`mine`. This is the highest-risk item on this list (schema + search changes) and should be benchmark-gated like Phase 15.

### C. Explicitly NOT adopting (consistent with PLAN.md out-of-scope list)

| Supermemory feature | Why not |
|---|---|
| Cloud connectors (Drive/Gmail/Notion/GitHub webhooks) | Network daemons violate local-first/single-binary; our `index`/`mine`/`sync` cover local sources |
| PDF/OCR/video extractors | Already parked (Office extract); needs heavy deps |
| ASMR agentic retrieval | 11–15 LLM calls/query; violates zero-config local cost model; also just research-grade |
| Hosted API / multi-tenant | Different product entirely |
| Framework middleware (Vercel AI SDK etc.) | We're stdio-MCP only; hooks already parked |
| Their graph model | Ours (bi-temporal triples) is strictly more rigorous |

---

## 4. Suggested phasing (if adopted into PLAN.md)

These would slot **after Phase 21** (or replace parking-lot items), keeping the current phase sequence intact:

| New phase | Content | Depends on |
|---|---|---|
| 22 | A2 `expires_at` (+ piggyback on Phase 19 migration patterns) | none (additive) |
| 23 | A1 `profile` + A3 `context` composite read tools | 16 (KG provenance helps static facts) |
| 24 | A4 simplified tool trio + docs | 23 |
| 25 | B1 multi-benchmark harness; B2 contradiction annotation | 15 done |
| parked→promote? | B3 chunking | bench gate from 25 |

Each phase keeps the existing hard gates: suite green, fmt/clippy clean, LME R@5 ≥ 94.04% floor, no new runtime deps, no LLM calls in the default path.

---

## 5. Bottom line

Supermemory's moat is not retrieval quality — our hybrid RRF is competitive and our KG is more principled. Its moat is **lifecycle automation and interface simplicity**: facts get extracted, superseded, and forgotten without the caller thinking about it, and agents interact through three obvious verbs. Of those, everything except LLM-based automatic extraction is achievable within our constraints:

1. Deterministic forgetting (`expires_at`) — engine-enforced, agent-declared.
2. Derived profiles & one-call context — computed from KG + recency, no LLM.
3. A tiny verb-level tool layer over our 37-tool foundation.
4. Multi-benchmark credibility.

That combination would make MemPalace match Supermemory's product semantics while staying strictly superior on privacy, deployability, and temporal correctness.
