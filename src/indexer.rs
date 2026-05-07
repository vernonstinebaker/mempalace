use anyhow::Result;
use std::path::Path;
use walkdir::WalkDir;

use crate::db::Database;
use crate::embed::Embedder;
use crate::log::log;

/// Directory names that are always skipped during indexing.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target", // Rust build output
    "__pycache__",
    ".venv",
    "venv",
    ".env", // Python virtualenv named .env
    "vendor",
    "dist",
    "build",
    ".build",
    "out",
    ".next",
    ".nuxt",
    ".svelte-kit",
    "coverage",
    ".nyc_output",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    "buck-out",
    "bazel-out",
    ".gradle",
    ".idea",
    ".vscode",
    "pods",        // iOS CocoaPods
    "deriveddata", // Xcode
];

/// Exact file names (case-insensitive) that are always skipped.
const SKIP_FILES: &[&str] = &[
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "cargo.lock",
    "go.sum",
    "gemfile.lock",
    "poetry.lock",
    "composer.lock",
    "pipfile.lock",
    "packages.lock.json",  // NuGet
    "project.assets.json", // .NET
    "npm-shrinkwrap.json",
];

/// File extensions treated as text (indexable).
/// Override at runtime with the MEMPALACE_EXTENSIONS env var:
///   MEMPALACE_EXTENSIONS="rs,go,py,md"  (comma-separated, replaces the default list)
const DEFAULT_TEXT_EXTENSIONS: &[&str] = &[
    "rs",
    "c",
    "cpp",
    "cc",
    "cxx",
    "c++",
    "h",
    "hpp",
    "hh",
    "go",
    "py",
    "js",
    "mjs",
    "cjs",
    "ts",
    "tsx",
    "jsx",
    "html",
    "htm",
    "css",
    "scss",
    "sass",
    "less",
    "json",
    "toml",
    "yaml",
    "yml",
    "md",
    "mdx",
    "markdown",
    "txt",
    "sh",
    "bash",
    "zsh",
    "fish",
    "rb",
    "java",
    "kt",
    "kts",
    "swift",
    "lua",
    "sql",
    "env",
    "gitignore",
    "dockerfile",
    "makefile",
    "cmake",
    "nix",
    "vim",
    "el",
    "ex",
    "exs", // Elixir
    "erl",
    "hrl", // Erlang
    "hs",
    "lhs", // Haskell
    "ml",
    "mli", // OCaml
    "clj",
    "cljs", // Clojure
    "scala",
    "cs", // C#
    "fs",
    "fsi", // F#
    "php",
    "r",
    "jl", // Julia
    "tf",
    "tfvars", // Terraform
    "proto",  // Protobuf
    "graphql",
    "gql",
    "svelte",
    "vue",
];

/// Max file size to index (500 KB)
const MAX_FILE_BYTES: u64 = 500 * 1024;

/// Max content length stored per drawer (4000 chars)
const MAX_CONTENT_CHARS: usize = 4000;

/// Resolve the active extension list: MEMPALACE_EXTENSIONS env var overrides the default.
fn active_extensions() -> Vec<String> {
    if let Ok(val) = std::env::var("MEMPALACE_EXTENSIONS") {
        let exts: Vec<String> = val
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        if !exts.is_empty() {
            return exts;
        }
    }
    DEFAULT_TEXT_EXTENSIONS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

pub fn index_directory(db: &Database, root: &str, embedder: Option<&Embedder>) -> Result<usize> {
    let root_path = Path::new(root).canonicalize()?;
    // Wing is the last component of the root directory
    let wing_name = root_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("files")
        .to_lowercase()
        .replace(' ', "_");

    let extensions = active_extensions();

    let mut count = 0usize;

    for entry in WalkDir::new(&root_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Prune entire directories early (much faster than post-filter)
            if e.file_type().is_dir() {
                let name = e.file_name().to_str().unwrap_or("");
                // Always skip hidden directories
                if name.starts_with('.') {
                    return false;
                }
                // Skip known noisy build/dependency directories
                let name_lower = name.to_lowercase();
                if SKIP_DIRS.iter().any(|d| *d == name_lower) {
                    return false;
                }
            }
            true
        })
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        // Skip directories
        if path.is_dir() {
            continue;
        }

        // Skip hidden files
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') {
                continue;
            }
        }

        // Skip known lock/noise files (case-insensitive name match)
        let file_name_lower = path
            .file_name()
            .and_then(|f| f.to_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();

        if SKIP_FILES.contains(&file_name_lower.as_str()) {
            continue;
        }

        // Check file size
        if let Ok(meta) = path.metadata() {
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
            if meta.len() == 0 {
                continue;
            }
        } else {
            continue;
        }

        // Check extension
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_lowercase());

        let is_text = match &ext {
            Some(e) => extensions.contains(e),
            None => {
                // Extensionless: allow known names
                matches!(
                    file_name_lower.as_str(),
                    "makefile"
                        | "dockerfile"
                        | "readme"
                        | "license"
                        | "gitignore"
                        | "gitattributes"
                )
            }
        };

        if !is_text {
            continue;
        }

        // Read file content (also catches files that look like text but are binary)
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        if content.trim().is_empty() {
            continue;
        }

        // Compute room name from relative path
        let rel = path.strip_prefix(&root_path).unwrap_or(path);
        let room = slugify_path(rel);

        // Truncate content
        let content_truncated: String = content.chars().take(MAX_CONTENT_CHARS).collect();

        // Build stored content: include path header
        let stored = format!("FILE: {}\n\n{}", path.display(), content_truncated);

        let source_file = path.to_str().unwrap_or("").to_string();

        match db.add_drawer(
            &wing_name,
            &room,
            &stored,
            Some(&source_file),
            "indexer",
            embedder,
        ) {
            Ok(_) => count += 1,
            Err(e) => log!("warn", "skipping {}: {e}", path.display()),
        }
    }

    Ok(count)
}

/// Turn a relative path into a slug suitable for a room name.
/// e.g. "src/db.rs" → "src-db-rs"
fn slugify_path(path: &Path) -> String {
    let s = path.to_str().unwrap_or("unknown");
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' => c.to_ascii_lowercase(),
            _ => '-',
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify_path_simple() {
        let p = std::path::Path::new("src/db.rs");
        assert_eq!(slugify_path(p), "src-db-rs");
    }

    #[test]
    fn test_slugify_path_complex() {
        let p = std::path::Path::new("projects/nested/deep/file.go");
        assert_eq!(slugify_path(p), "projects-nested-deep-file-go");
    }

    #[test]
    fn test_slugify_path_extensionless() {
        let p = std::path::Path::new("Makefile");
        assert_eq!(slugify_path(p), "makefile");
    }

    #[test]
    fn test_slugify_path_hidden_file() {
        let p = std::path::Path::new(".gitignore");
        // "." → "-", then "gitignore" → "gitignore"
        // Combined: "-gitignore" → trim_matches('-') → "gitignore"
        assert_eq!(slugify_path(p), "gitignore");
    }

    #[test]
    fn test_active_extensions_default() {
        let exts = active_extensions();
        // Core extensions should always be present
        assert!(exts.contains(&"rs".to_string()));
        assert!(exts.contains(&"py".to_string()));
        assert!(exts.contains(&"go".to_string()));
        assert!(exts.contains(&"md".to_string()));
        assert!(exts.contains(&"js".to_string()));
        assert!(exts.contains(&"txt".to_string()));
    }

    #[test]
    fn test_active_extensions_env_override() {
        std::env::set_var("MEMPALACE_EXTENSIONS", "go,py");
        let exts = active_extensions();
        std::env::remove_var("MEMPALACE_EXTENSIONS");
        assert_eq!(exts, vec!["go".to_string(), "py".to_string()]);
    }

    #[test]
    fn test_active_extensions_empty_env_uses_default() {
        std::env::set_var("MEMPALACE_EXTENSIONS", "");
        let exts = active_extensions();
        std::env::remove_var("MEMPALACE_EXTENSIONS");
        // Should fall back to default (which is long)
        assert!(exts.len() > 5);
    }

    #[test]
    fn test_skip_dirs_are_lowercase() {
        for d in SKIP_DIRS {
            assert_eq!(
                *d,
                d.to_lowercase(),
                "SKIP_DIRS entry '{d}' must be lowercase"
            );
        }
    }

    #[test]
    fn test_skip_files_are_lowercase() {
        for f in SKIP_FILES {
            assert_eq!(f, &f.to_lowercase(), "SKIP_FILES must be lowercase");
        }
    }
}
