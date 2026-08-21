mod db;
mod embed;
mod hallways;
mod import_palace;
mod import_sessions;
mod indexer;
mod knowledge_graph;
mod lock;
mod log;
mod mcp;
mod profile;
mod validate;
mod wal;

use crate::log::log;
use rusqlite::ffi::sqlite3_auto_extension;
use sqlite_vec::sqlite3_vec_init;

fn register_sqlite_vec() {
    unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut i8,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32,
        >(sqlite3_vec_init as *const ())));
    }
}

fn get_palace_dir() -> String {
    std::env::var("MEMPALACE_PALACE_PATH").unwrap_or_else(|_| {
        format!(
            "{}/.local/share/mempalace",
            std::env::var("HOME").unwrap_or_default()
        )
    })
}

fn main() {
    // Register sqlite-vec BEFORE any connection is opened
    register_sqlite_vec();

    let args: Vec<String> = std::env::args().collect();

    // Parse subcommand
    let subcommand = args.get(1).map(|s| s.as_str()).unwrap_or("");

    match subcommand {
        "--info" | "info" => {
            let dir = get_palace_dir();
            let db = db::Database::open(&dir).expect("Failed to open database");
            let count = db.get_drawer_count();
            println!("MemPalace v3.1.0 (Rust)");
            println!("Palace dir: {dir}");
            println!("Total drawers: {count}");
        }

        "index" => {
            // index <directory>
            let target_dir = args.get(2).expect("Usage: mempalace index <directory>");
            let palace_dir = get_palace_dir();
            let db = db::Database::open(&palace_dir).expect("Failed to open database");
            let embedder = embed::try_load_embedder();
            log!("info", "Indexing: {target_dir}");
            let count = indexer::index_directory(&db, target_dir, embedder.as_ref())
                .expect("Indexing failed");
            println!("Indexed {count} files");
        }

        "index-sessions" => {
            // index-sessions [--source auto|opencode|codex|grok|zcode] [--db <path>] [--full]
            let source_raw = args
                .iter()
                .position(|a| a == "--source")
                .and_then(|pos| args.get(pos + 1))
                .map(String::as_str);
            let source =
                import_sessions::normalize_source_arg(source_raw).expect("invalid --source value");
            let full = args.iter().any(|a| a == "--full");
            let palace_dir = get_palace_dir();
            let db = db::Database::open(&palace_dir).expect("Failed to open database");
            let embedder = embed::try_load_embedder();
            let mut paths = import_sessions::SourcePaths::resolve();
            if let Some(pos) = args.iter().position(|a| a == "--db") {
                let p = args
                    .get(pos + 1)
                    .cloned()
                    .expect("--db requires a path argument");
                paths.opencode_db = std::path::PathBuf::from(&p);
                paths.zcode_db = std::path::PathBuf::from(p);
            }
            if source == "auto" {
                let results = import_sessions::import_auto(&db, &paths, embedder.as_ref(), full)
                    .expect("Session import failed");
                let total: usize = results.iter().map(|(_, n)| n).sum();
                for (name, n) in &results {
                    println!("  {name}: {n} sessions");
                }
                println!("Imported {total} sessions (auto)");
            } else {
                let (_, count) =
                    import_sessions::import_one(&db, &source, &paths, embedder.as_ref(), full)
                        .expect("Session import failed");
                println!("Imported {count} sessions from {source}");
            }
        }

        "search" => {
            // search <query> [--limit N] [--wing W] [--room R] [--source FILE]
            let opts = parse_search_args(&args[2..]).unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(2);
            });
            let palace_dir = get_palace_dir();
            let db = db::Database::open(&palace_dir).expect("Failed to open database");
            let embedder = embed::try_load_embedder();
            run_search(&db, &opts, embedder.as_ref()).expect("Search failed");
        }

        "import-palace" => {
            // import-palace <source_path>
            let source_path = args
                .get(2)
                .expect("Usage: mempalace import-palace <source_palace.db>");
            let palace_dir = get_palace_dir();
            let db = db::Database::open(&palace_dir).expect("Failed to open database");
            log!("info", "Importing palace from: {source_path}");
            let (drawers, triples) =
                import_palace::import_palace(&db, source_path).expect("Palace import failed");
            println!("Imported {drawers} drawers, {triples} triples");
        }

        "reindex" => {
            // reindex — backfill vector embeddings for all drawers missing them
            let palace_dir = get_palace_dir();
            let db = db::Database::open(&palace_dir).expect("Failed to open database");
            match embed::try_load_embedder() {
                Some(embedder) => {
                    log!("info", "Backfilling embeddings...");
                    let (total, embedded, failed) =
                        db.backfill_embeddings(&embedder).expect("Backfill failed");
                    println!("Backfill complete: {embedded}/{total} embedded, {failed} failed");
                }
                None => {
                    log!("error", "no embedder found — cannot reindex");
                    std::process::exit(1);
                }
            }
        }

        // Default: MCP stdio server
        _ => {
            let palace_dir = get_palace_dir();
            let db = db::Database::open(&palace_dir).expect("Failed to open database");
            let embedder = embed::try_load_embedder();
            let server = mcp::Server::new(&db, embedder);
            server.run_stdio();
        }
    }
}

// ── CLI search (Phase 27) ─────────────────────────────────────────────────────

pub struct SearchOpts {
    pub query: String,
    pub limit: usize,
    pub wing: Option<String>,
    pub room: Option<String>,
    pub source_file: Option<String>,
}

/// Parse `search` subcommand flags. Query is the first positional arg.
fn parse_search_args(args: &[String]) -> Result<SearchOpts, String> {
    let mut query: Option<String> = None;
    let mut limit = 5usize;
    let mut wing = None;
    let mut room = None;
    let mut source_file = None;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" => {
                i += 1;
                limit = args
                    .get(i)
                    .and_then(|v| v.parse().ok())
                    .ok_or("--limit requires a number")?;
            }
            "--wing" => {
                i += 1;
                wing = Some(args.get(i).ok_or("--wing requires a value")?.clone());
            }
            "--room" => {
                i += 1;
                room = Some(args.get(i).ok_or("--room requires a value")?.clone());
            }
            "--source" => {
                i += 1;
                source_file = Some(args.get(i).ok_or("--source requires a value")?.clone());
            }
            other => {
                if other.starts_with('-') && other != "-" {
                    return Err(format!("Unknown flag: {other}"));
                }
                if query.is_some() {
                    return Err("Only one query argument allowed".to_string());
                }
                query = Some(other.to_string());
            }
        }
        i += 1;
    }

    let query = query.ok_or(
        "Usage: mempalace search <query> [--limit N] [--wing W] [--room R] [--source FILE]",
    )?;
    Ok(SearchOpts {
        query,
        limit: limit.clamp(1, 100),
        wing,
        room,
        source_file,
    })
}

/// Run a search and print human-readable results. Uses the same hybrid
/// path as the MCP server; falls back to FTS-only when no embedder loads.
fn run_search(
    db: &db::Database,
    opts: &SearchOpts,
    embedder: Option<&embed::Embedder>,
) -> anyhow::Result<()> {
    let result = db.search_filtered(
        &opts.query,
        opts.limit,
        0,
        opts.wing.as_deref(),
        opts.room.as_deref(),
        None,
        None,
        opts.source_file.as_deref(),
        0.0,
        embedder,
        "relevance",
        false,
    )?;
    print!("{}", format_search_results(&result));
    Ok(())
}

/// Format a `db.search` JSON response as plain text, one block per hit.
fn format_search_results(result: &serde_json::Value) -> String {
    use std::fmt::Write as _;
    let total = result.get("total").and_then(|t| t.as_u64()).unwrap_or(0);
    let Some(results) = result.get("results").and_then(|r| r.as_array()) else {
        return String::new();
    };
    if results.is_empty() {
        return format!("No results ({total} total drawers matched nothing).\n");
    }

    let mut out = String::new();
    for (i, hit) in results.iter().enumerate() {
        let wing = hit.get("wing").and_then(|v| v.as_str()).unwrap_or("?");
        let room = hit.get("room").and_then(|v| v.as_str()).unwrap_or("?");
        let filed_at = hit.get("filed_at").and_then(|v| v.as_str()).unwrap_or("?");
        let id = hit.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let content = hit.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let snippet: String = content.chars().take(160).collect();
        let _ = writeln!(out, "#{} [{}] {}/{} ({})", i + 1, id, wing, room, filed_at);
        let _ = writeln!(out, "  {snippet}");
        if let Some(sim) = hit.get("similarity").and_then(|v| v.as_f64()) {
            let _ = writeln!(out, "  similarity: {sim:.3}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> String {
        v.to_string()
    }

    #[test]
    fn test_cli_search_flag_parsing() {
        let args = vec![
            s("rust importer"),
            s("--limit"),
            s("10"),
            s("--wing"),
            s("codex"),
            s("--room"),
            s("backend"),
            s("--source"),
            s("/tmp/x.md"),
        ];
        let opts = parse_search_args(&args).unwrap();
        assert_eq!(opts.query, "rust importer");
        assert_eq!(opts.limit, 10);
        assert_eq!(opts.wing.as_deref(), Some("codex"));
        assert_eq!(opts.room.as_deref(), Some("backend"));
        assert_eq!(opts.source_file.as_deref(), Some("/tmp/x.md"));

        // Defaults and clamping.
        let opts = parse_search_args(&[s("q")]).unwrap();
        assert_eq!(opts.limit, 5);
        let opts = parse_search_args(&[s("q"), s("--limit"), s("500")]).unwrap();
        assert_eq!(opts.limit, 100);

        // Missing query → usage error.
        assert!(parse_search_args(&[]).is_err());
        // Unknown flag → error.
        assert!(parse_search_args(&[s("q"), s("--bogus")]).is_err());
    }

    #[test]
    fn test_format_search_results_prints_ranked_hits() {
        let result = serde_json::json!({
            "total": 2,
            "results": [
                {"id": "a1", "wing": "codex", "room": "login-fix",
                 "filed_at": "2026-08-01 10:00:00", "similarity": 0.91,
                 "content": "Fixed the token refresh path in the login flow."},
                {"id": "b2", "wing": "opencode", "room": "misc",
                 "filed_at": "2026-08-02 09:00:00",
                 "content": "Unrelated note about something else entirely."}
            ]
        });
        let out = format_search_results(&result);
        let a1_pos = out
            .find("#1 [a1] codex/login-fix (2026-08-01 10:00:00)")
            .unwrap();
        let b2_pos = out
            .find("#2 [b2] opencode/misc (2026-08-02 09:00:00)")
            .unwrap();
        assert!(a1_pos < b2_pos, "hits must appear in rank order");
        assert!(out.contains("token refresh path"));
        assert!(out.contains("similarity: 0.910"));
    }

    #[test]
    fn test_format_search_results_empty_shows_no_hits() {
        let result = serde_json::json!({"total": 42, "results": []});
        let out = format_search_results(&result);
        assert!(out.contains("No results"));
    }

    #[test]
    fn test_cli_search_wraps_db_search() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = db::Database::open(dir.path().to_str().unwrap()).unwrap();
        db.add_drawer_ex(
            "w",
            "target",
            "The zcode adapter opens foreign databases read-only.",
            None,
            "test",
            None,
            None,
        )
        .unwrap();
        db.add_drawer_ex(
            "w",
            "other",
            "Completely unrelated content about gardening tools and soil.",
            None,
            "test",
            None,
            None,
        )
        .unwrap();

        let opts = SearchOpts {
            query: s("zcode adapter read-only databases"),
            limit: 5,
            wing: None,
            room: None,
            source_file: None,
        };
        // Capture output by formatting what run_search would print.
        let result = db
            .search_filtered(
                &opts.query,
                opts.limit,
                0,
                None,
                None,
                None,
                None,
                None,
                0.0,
                None,
                "relevance",
                false,
            )
            .unwrap();
        let out = format_search_results(&result);
        let target_pos = out
            .find("zcode adapter")
            .unwrap_or_else(|| panic!("no target hit; raw result: {result}"));
        let other_pos = out.find("gardening").map(|p| p).unwrap_or(usize::MAX);
        assert!(target_pos < other_pos || other_pos == usize::MAX);
        assert!(out.contains("w/target"));
    }
}
