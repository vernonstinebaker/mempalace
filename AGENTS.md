# Agent Instructions for MemPalace MCP Server

When working with or extending the MemPalace MCP server, follow these guidelines to maintain consistency, performance, and architectural integrity.

## 🎯 Core Principles

### 1. **Single-Binary Zero Dependency**
- Never introduce runtime dependencies that require system packages (no `apt install`, `brew install`, `pip install`, etc.)
- All codecs, models, and extensions must be vendored or bundled at compile time
- The binary must run on a fresh macOS/Linux system with only Rust 1.75+ installed

### 2. **Performance-Conscious Design**
- Target p99 latency <100ms for `search()` on a palace with 100k drawers
- Avoid allocations in hot paths; reuse buffers where possible
- Prefer stack allocation over heap for small, fixed-size objects
- Profile before optimizing — use `cargo bench` or `perf`/`Instruments`

### 3. **Correctness Over Convenience**
- Favor explicit error handling over `unwrap()` or `expect()` in library code
- Validate all inputs at the MCP boundary (`mcp.rs`)
- Use transactions for multi-table updates
- Test edge cases: empty strings, Unicode, extremely long inputs (>1MB)

### 4. **Maintainability & Clarity**
- Prefer clarity over cleverness — future maintainers (including future you) should grasp intent quickly
- Follow existing code style (run `cargo fmt` before committing)
- Document non-obvious invariants and why they exist
- Keep functions focused: one responsibility per function

## 🔧 MCP Tool Guidelines

When adding or modifying tools in `src/mcp.rs`:

### Input Validation
- Use helper functions: `get_str()`, `get_i64()`, `get_bool()` from `mcp.rs`
- Validate ranges (e.g., `limit > 0 && limit <= 1000`)
- Reject invalid UTF-8 early
- Return standardized JSON error responses:
  ```json
  {
    "success": false,
    "error": "SpecificErrorCode: human readable message"
  }
  ```

### Response Shape
- Success: `{ "success": true, ...result fields... }`
- Error: `{ "success": false, "error": "..." }`
- Never return raw database errors to clients — map to user-actionable messages
- For list results, return `{"success": true, "items": [...]}` rather than nesting

### Tool Lifecycle
1. Add JSON schema to `TOOLS_JSON` constant (keep alphabetized)
2. Add handler arm in `handle_tool_call` match statement
3. Implement core logic in `db.rs` (prefer private helpers over putting everything in mcp.rs)
4. Update `docs/` or README if user-facing behavior changes
5. Add tests if non-trivial logic

## 💾 Database Guidelines (`src/db.rs`)

### Connection Handling
- The `Database` struct holds a single `rusqlite::Connection`
- All methods take `&self` — connection lifetime is the server lifetime
- No connection pooling needed (SQLite handles concurrent readers well with WAL)

### Transaction Boundaries
- Wrap multi-statement operations in explicit transactions:
  ```rust
  let tx = self.conn.transaction()?;
  // ... statements ...
  tx.commit()?;
  ```
- Single-statement operations are auto-committed (fine for simple INSERT/UPDATE/DELETE)

### Error Handling
- Propagate `anyhow::Error` up the stack
- At the MCP boundary (`mcp.rs`), convert to user-friendly messages
- Never log raw SQL errors — they may contain schema details

### Performance Patterns
- Use `query_row` for single-result lookups
- Use `prepare()` + `query_map()` for reusable parameterized queries
- Avoid `SELECT *` — specify columns explicitly
- Index foreign keys and filtered columns (see schema comments)
- For bulk operations, consider PRAGMA adjustments:
  ```rust
  conn.execute_batch("PRAGMA synchronous=OFF; PRAGMA journal_mode=MEMORY;")?;
  // ... risky bulk op ...
  conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA journal_mode=WAL;")?;
  ```

### Testing (TDD Required)

This project follows **Test-Driven Development**:

1. **Write a failing test first.**
   - Every new feature, bug fix, or behavior change MUST begin with a failing test.
   - Tests live in `#[cfg(test)] mod tests { ... }` within the file being changed.
   - Use `tempfile::TempDir` for test databases (add `tempfile` as a `[dev-dependency]` in Cargo.toml).

2. **Write the minimum code to make it pass.**
   - Only write enough production code to satisfy the test.
   - Do not pre-emptively add features not covered by tests.

3. **Refactor with confidence.**
   - After green, clean up: extract helpers, reduce duplication, improve names.
   - The test suite is your safety net — if it stays green, the behavior is preserved.

4. **Test categories:**
   - **Unit tests**: Test individual functions in isolation (e.g., `sanitize_fts_query`, `slugify`, embedding dimensions).
   - **Integration tests**: Test database operations end-to-end (create table → insert → search → delete).
   - **Edge cases**: Empty strings, Unicode (CJK, emoji), extremely long inputs (>1MB), null bytes, concurrent access.
   - **Error paths**: Invalid inputs, missing required args, database corrupted/missing.

5. **Red-Green-Refactor checklist per commit:**
   - [ ] Failing test written and confirmed failing
   - [ ] Production code written, test passes
   - [ ] `cargo fmt` and `cargo clippy -- -D warnings` clean
   - [ ] All existing tests still pass
   - [ ] No new `unwrap()` or `expect()` in library code

- Add unit tests in `db.rs`, `embed.rs`, `knowledge_graph.rs`, `import_sessions.rs`, `indexer.rs` using `#[cfg(test)] mod tests { ... }`
- Use temporary directories via `tempfile::TempDir`
- Test both success and error paths
- Test concurrent access if relevant

## 🧠 Embedder Guidelines (`src/embed.rs`)

### Model Constraints
- Must produce fixed-size embeddings (currently 384 dimensions)
- Must be deterministic (same input → same output)
- Must handle UTF-8 text
- Should return `None` only on unrecoverable error (not for empty input)

### Thread Safety
- The `Embedder` trait is implemented for `&Embedder` — must be `Sync`
- Current implementation uses `tract-onnx` which is thread-safe for inference
- If adding a new model, verify thread safety before declaring `Sync`

### Performance
- Preprocess outside timing measurements if benchmarking
- The model loads once at startup — amortize cost over many queries
- Consider batching if many embeddings are needed simultaneously (not currently needed)

## 🔄 Import/Export Guidelines

### Import from Foreign Formats
- Validate all inputs before writing to palace
- Normalize line endings (`\r\n` → `\n`)
- Strip BOM if present
- Respect file size limits (reject >100MB files to prevent OOM)
- For session imports: maintain stable IDs to enable re-import without duplication

### Export Formats
- Provide both human-readable and machine-readable forms when useful
- For knowledge graph: support JSON and AAAK
- For drawer exports: JSON lines is preferred
- Never export raw binary blobs (vec0 embeddings) without context

## 📦 Release Process

### Versioning
- Use SemVer: MAJOR.MINOR.PATCH
- MAJOR: breaking changes to MCP tool contracts or storage format
- MINOR: backward-compatible feature additions
- PATCH: bug fixes, performance improvements, documentation

### Pre-Release Checklist
1. `cargo fmt -- --check`
2. `cargo clippy -- -D warnings`
3. Run full test suite: `cargo test --release`
4. Run LongMemEval benchmark: `python bench/longmemeval_rust_useronly.py`
5. Verify binary size hasn't jumped unexpectedly (`size target/release/mempalace-mcp`)
6. Test on both Intel and Apple Silicon macOS (if possible)
7. Check that `MEMPALACE_PALACE_PATH` expansion works correctly
8. Ensure `--info` works with empty palace

### Post-Release
1. Tag release: `git tag vX.Y.Z && git push origin vX.Y.Z`
2. Create GitHub/Gitea release with changelog
3. Announce in relevant channels

## ❓ When in Doubt

- Check existing code for similar patterns
- Run the benchmark to ensure no regressions
- Ask: does this change preserve the single-binary guarantee?
- Ask: would this break if deployed to a fresh VM with only Rust installed?
- Remember: the goal is a reliable, embeddable memory system — not a feature-rich research prototype

## 📋 ROADMAP.md Tracking

- The project roadmap lives at `ROADMAP.md` in the repo root.
- Every time a phase or individual step is completed, update the corresponding
  `- [ ]` checkbox to `- [x]` in ROADMAP.md.
- When all steps in a phase are checked, update the Status Dashboard at the
  top of ROADMAP.md to reflect the new grade for that dimension.
- The roadmap is the single source of truth for project progress — keep it and
  AGENTS.md in sync.
