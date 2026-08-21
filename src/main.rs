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
