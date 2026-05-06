// Write-Ahead Log — JSONL audit trail for all write operations.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::LazyLock;
use std::time::SystemTime;

use serde_json::{json, Value};

use crate::log;

const REDACT_KEYS: &[&str] = &["content", "query", "entry", "text", "document", "content_preview"];

static WAL: LazyLock<WalLogger> = LazyLock::new(|| {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = format!("{home}/.local/share/mempalace/wal");
    WalLogger::new(&dir)
});

/// Log a write operation to the WAL (global singleton).
pub fn log_write(operation: &str, params: Value) {
    WAL.log(operation, params);
}

/// Read the last N WAL entries (newest first).
pub fn read_entries(limit: usize) -> Vec<Value> {
    WAL.read(limit)
}

struct WalLogger {
    path: String,
}

impl WalLogger {
    /// Create a new WAL logger. Directory is created with 0o700 perms.
    /// File is created with 0o600 perms.
    pub fn new(dir: &str) -> Self {
        let dir_path = Path::new(dir);
        let _ = std::fs::create_dir_all(dir_path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir_path, std::fs::Permissions::from_mode(0o700));
        }
        let file_path = dir_path.join("write_log.jsonl");
        let path = file_path.to_str().unwrap_or("write_log.jsonl").to_string();

        // Create file atomically with restricted permissions
        if !file_path.exists() {
            if let Ok(f) = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&file_path)
            {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
                }
            }
        }

        Self { path }
    }

    /// Log a write operation with redacted params.
    pub fn log(&self, operation: &str, params: Value) {
        let ts = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Redact sensitive content
        let safe_params = redact_params(&params);

        let entry = json!({
            "timestamp": ts,
            "datetime": format_ts(ts),
            "operation": operation,
            "params": safe_params,
        });

        match OpenOptions::new().append(true).create(true).open(&self.path) {
            Ok(mut f) => {
                let line = serde_json::to_string(&entry).unwrap_or_default();
                let _ = writeln!(f, "{line}");
            }
            Err(e) => {
                log!("warn", "WAL write failed: {e}");
            }
        }
    }

    /// Read the last N WAL entries (newest first).
    pub fn read(&self, limit: usize) -> Vec<Value> {
        let content = match std::fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        let mut entries: Vec<Value> = content
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .collect();
        entries.reverse();
        entries.truncate(limit);
        entries
    }
}

fn redact_params(params: &Value) -> Value {
    let obj = match params.as_object() {
        Some(o) => o,
        None => return params.clone(),
    };
    let mut safe = serde_json::Map::new();
    for (k, v) in obj {
        if REDACT_KEYS.contains(&k.as_str()) {
            let redacted = match v.as_str() {
                Some(s) => format!("[REDACTED {} chars]", s.len()),
                None => "[REDACTED]".to_string(),
            };
            safe.insert(k.clone(), json!(redacted));
        } else {
            safe.insert(k.clone(), v.clone());
        }
    }
    Value::Object(safe)
}

fn format_ts(secs: u64) -> String {
    let days = secs / 86400;
    let time = secs % 86400;
    let year = 1970 + (days as f64 / 365.25) as i64;
    let day_of_year = days as i64 - ((year - 1970) * 365 + ((year - 1969) / 4));
    let month_days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1;
    let mut remaining = day_of_year;
    for (i, md) in month_days.iter().enumerate() {
        let mdays = if i == 1 && year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
            29
        } else {
            *md
        } as i64;
        if remaining < mdays {
            month = i as i64 + 1;
            break;
        }
        remaining -= mdays;
        month = i as i64 + 1;
    }
    let day = remaining + 1;
    let hour = time / 3600;
    let min = (time % 3600) / 60;
    let sec = time % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02}")
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_redact_params_basic() {
        let params = json!({"wing": "w", "room": "r", "content": "secret"});
        let result = redact_params(&params);
        assert_eq!(result["wing"], "w");
        assert_eq!(result["room"], "r");
        assert!(result["content"].as_str().unwrap().contains("REDACTED"));
    }

    #[test]
    fn test_redact_params_query() {
        let params = json!({"query": "confidential", "wing": "w"});
        let result = redact_params(&params);
        assert!(result["query"].as_str().unwrap().contains("REDACTED"));
        assert_eq!(result["wing"], "w");
    }

    #[test]
    fn test_redact_params_multiple_keys() {
        let params = json!({
            "content": "a", "query": "b", "text": "c",
            "entry": "d", "document": "e", "wing": "f"
        });
        let result = redact_params(&params);
        assert!(result["content"].as_str().unwrap().contains("REDACTED"));
        assert!(result["query"].as_str().unwrap().contains("REDACTED"));
        assert!(result["text"].as_str().unwrap().contains("REDACTED"));
        assert!(result["entry"].as_str().unwrap().contains("REDACTED"));
        assert!(result["document"].as_str().unwrap().contains("REDACTED"));
        assert_eq!(result["wing"], "f");
    }

    #[test]
    fn test_wal_global_write_and_read() {
        let test_op = format!("test_{}", std::time::UNIX_EPOCH.elapsed().unwrap().as_millis());
        log_write(&test_op, json!({"key": "val", "content": "secret"}));
        let entries = read_entries(100);
        let found = entries.iter().any(|e| e["operation"] == test_op);
        assert!(found, "WAL entry for {test_op} not found");
    }

    #[test]
    fn test_format_ts() {
        let ts = format_ts(0);
        assert_eq!(ts, "1970-01-01 00:00:00");
    }
}
