use anyhow::Result;
use rusqlite::{params, Connection};

use crate::db::Database;
use crate::embed::Embedder;
use crate::log::log;

/// Import OpenCode sessions from opencode.db into the palace.
/// Each session becomes one drawer: wing="opencode", room=slugified title.
/// Content = timestamp + title + directory + tool summary + first message + assistant text.
/// Pass full=true to re-import all sessions; default is incremental (only new/changed).
pub fn import_sessions(
    db: &Database,
    oc_db_path: &str,
    embedder: Option<&Embedder>,
    full: bool,
) -> Result<usize> {
    let oc = Connection::open(oc_db_path)?;

    let max_chars = session_max_chars();
    let source_key = "opencode_sessions";

    // Determine cutoff for incremental sync
    let since: Option<i64> = if full {
        None
    } else {
        let last = db.get_sync_state(source_key);
        if last > 0 {
            Some(last)
        } else {
            None
        }
    };

    // Fetch sessions, optionally filtered by time_updated
    let (sql, params_slice): (&str, Vec<rusqlite::types::Value>) = if let Some(s) = since {
        (
            "SELECT id, title, directory, time_updated FROM session WHERE time_updated > ?1 ORDER BY time_updated DESC",
            vec![rusqlite::types::Value::Integer(s)],
        )
    } else {
        (
            "SELECT id, title, directory, time_updated FROM session ORDER BY time_updated DESC",
            vec![],
        )
    };

    let mut stmt = oc.prepare(sql)?;

    struct SessionRow {
        id: String,
        title: String,
        directory: String,
        time_updated: i64,
    }

    let sessions: Vec<SessionRow> = stmt
        .query_map(rusqlite::params_from_iter(params_slice.iter()), |row| {
            Ok(SessionRow {
                id: row.get(0)?,
                title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                directory: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                time_updated: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut count = 0usize;
    let mut max_ts: i64 = since.unwrap_or(0);

    for session in &sessions {
        let text_parts = collect_assistant_text(&oc, &session.id, max_chars);
        let filed_at = millis_to_dt(session.time_updated);

        let ts_line = format!("Date: {}", millis_to_dt(session.time_updated));
        let title_line = if session.title.is_empty() {
            format!("Session: {}", &session.id[..session.id.len().min(16)])
        } else {
            format!("Session: {}", session.title)
        };

        let dir_line = if session.directory.is_empty() {
            String::new()
        } else {
            format!("Directory: {}", session.directory)
        };

        let tool_line = collect_tool_names(&oc, &session.id);
        let first_msg = collect_first_user_message(&oc, &session.id);
        let summary = session_summary(&oc, &session.id);

        let mut content = String::new();
        content.push_str(&title_line);
        content.push('\n');
        content.push_str(&ts_line);
        if !dir_line.is_empty() {
            content.push('\n');
            content.push_str(&dir_line);
        }
        if !tool_line.is_empty() {
            content.push('\n');
            content.push_str(&tool_line);
        }
        if let Some(ref msg) = first_msg {
            content.push('\n');
            content.push_str(msg);
        }
        if !summary.is_empty() {
            content.push('\n');
            content.push_str(&summary);
        }
        if !text_parts.is_empty() {
            content.push('\n');
            content.push_str(&text_parts);
        }

        let room = if session.title.is_empty() {
            format!("session-{}", &session.id[..session.id.len().min(8)])
        } else {
            slugify(&session.title)
        };

        let drawer_id = format!("oc_session_{}", &session.id);
        match db.upsert_drawer(
            &drawer_id,
            "opencode",
            &room,
            &content,
            None,
            "import-sessions",
            Some(&filed_at),
            embedder,
        ) {
            Ok(_) => count += 1,
            Err(e) => log!("warn", "skipping session {}: {e}", session.id),
        }

        if session.time_updated > max_ts {
            max_ts = session.time_updated;
        }
    }

    // Record the max timestamp for next incremental sync
    if max_ts > since.unwrap_or(0) {
        db.set_sync_state(source_key, max_ts)?;
    }

    Ok(count)
}

/// Convert millisecond unix timestamp to "YYYY-MM-DD HH:MM:SS"
fn millis_to_dt(millis: i64) -> String {
    let secs = millis / 1000;
    let days = secs / 86400;
    // Simple conversion: days since epoch
    let year = 1970 + (days as f64 / 365.25) as i32;
    let day_of_year = days - ((year - 1970) as i64 * 365 + ((year - 1969) / 4) as i64);
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
            month = i + 1;
            break;
        }
        remaining -= mdays;
        month = i + 1;
    }
    let day = remaining + 1;
    let time = secs % 86400;
    let hour = time / 3600;
    let min = (time % 3600) / 60;
    let sec = time % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02}")
}

fn session_max_chars() -> usize {
    std::env::var("MEMPALACE_SESSION_MAX_CHARS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000)
}

/// Collect tool names used in the session.
fn collect_tool_names(conn: &Connection, session_id: &str) -> String {
    let sql = "SELECT DISTINCT json_extract(p.data, '$.name') FROM part p
               JOIN message m ON p.message_id = m.id
               WHERE p.session_id = ?1
               AND json_extract(m.data, '$.role') = 'assistant'
               AND json_extract(p.data, '$.type') = 'tool_use'
               AND json_extract(p.data, '$.name') IS NOT NULL
               ORDER BY 1";

    let names: Vec<String> = match conn.prepare(sql) {
        Ok(mut stmt) => stmt
            .query_map(params![session_id], |r| r.get::<_, Option<String>>(0))
            .ok()
            .map(|iter| iter.filter_map(|r| r.ok()).filter_map(|n| n).collect())
            .unwrap_or_default(),
        Err(_) => return String::new(),
    };

    if names.is_empty() {
        return String::new();
    }
    format!("Used tools: {}", names.join(", "))
}

/// Get the first user message for session context.
fn collect_first_user_message(conn: &Connection, session_id: &str) -> Option<String> {
    let sql = "SELECT p.data FROM part p
               JOIN message m ON p.message_id = m.id
               WHERE p.session_id = ?1
               AND json_extract(m.data, '$.role') = 'user'
               AND json_extract(p.data, '$.type') = 'text'
               ORDER BY p.rowid ASC LIMIT 1";

    let data: Option<String> = conn
        .prepare(sql)
        .ok()
        .and_then(|mut s| s.query_row(params![session_id], |r| r.get(0)).ok());

    data.and_then(|part_json| {
        let v = serde_json::from_str::<serde_json::Value>(&part_json).ok()?;
        let text = v.get("text")?.as_str()?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            let truncated: String = trimmed.chars().take(200).collect();
            Some(format!("First message: {truncated}"))
        }
    })
}

/// Build a summary line with message and part counts.
fn session_summary(conn: &Connection, session_id: &str) -> String {
    let msg_count: i64 = conn
        .prepare("SELECT COUNT(*) FROM message WHERE session_id = ?1")
        .ok()
        .and_then(|mut s| s.query_row(params![session_id], |r| r.get(0)).ok())
        .unwrap_or(0);

    let part_count: i64 = conn
        .prepare("SELECT COUNT(*) FROM part WHERE session_id = ?1")
        .ok()
        .and_then(|mut s| s.query_row(params![session_id], |r| r.get(0)).ok())
        .unwrap_or(0);

    if msg_count == 0 && part_count == 0 {
        return String::new();
    }
    format!("Messages: {msg_count}, Parts: {part_count}")
}

/// Collect text snippets from a session: first ~half from the start (establishes topic),
/// last ~half from the end (shows outcome). This way even long sessions that drifted
/// topic have both context represented.
fn collect_assistant_text(conn: &Connection, session_id: &str, max_chars: usize) -> String {
    let sql = "SELECT p.data FROM part p
               JOIN message m ON p.message_id = m.id
               WHERE p.session_id = ?1
               AND json_extract(m.data, '$.role') = 'assistant'
               AND json_extract(p.data, '$.type') = 'text'
               ORDER BY p.rowid ASC";

    let all_parts: Vec<String> = match conn.prepare(sql) {
        Ok(mut stmt) => stmt
            .query_map(params![session_id], |r| r.get::<_, String>(0))
            .ok()
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default(),
        Err(e) => {
            log!("warn", "prepare failed for session {session_id}: {e}");
            return String::new();
        }
    };

    let texts: Vec<String> = all_parts
        .iter()
        .filter_map(|part_json| {
            let v = serde_json::from_str::<serde_json::Value>(part_json).ok()?;
            let t = v.get("text").and_then(|t| t.as_str())?;
            let trimmed = t.trim();
            if !trimmed.is_empty() {
                Some(trimmed.to_string())
            } else {
                None
            }
        })
        .collect();

    if texts.is_empty() {
        return String::new();
    }

    let half = max_chars / 2;
    let mut head = String::new();
    for t in &texts {
        if head.len() >= half {
            break;
        }
        if !head.is_empty() {
            head.push('\n');
        }
        let remaining = half - head.len();
        head.push_str(&t.chars().take(remaining).collect::<String>());
    }

    let mut tail = String::new();
    for t in texts.iter().rev() {
        if tail.len() >= half {
            break;
        }
        let remaining = half - tail.len();
        let chunk: String = t.chars().take(remaining).collect();
        if !tail.is_empty() {
            tail.insert(0, '\n');
        }
        tail.insert_str(0, &chunk);
    }

    if head.len() + tail.len() <= max_chars {
        if tail.trim() == head.trim() || tail.is_empty() {
            head
        } else {
            format!("{head}\n...\n{tail}")
        }
    } else {
        format!("{head}\n...\n{tail}")
    }
}

fn slugify(s: &str) -> String {
    let slug: String = s
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            ' ' | '-' | '_' | '/' | '.' => '-',
            _ => '-',
        })
        .collect();

    // Collapse multiple dashes, trim
    let mut result = String::new();
    let mut last_dash = true;
    for c in slug.chars() {
        if c == '-' {
            if !last_dash {
                result.push('-');
                last_dash = true;
            }
        } else {
            result.push(c);
            last_dash = false;
        }
    }
    let result = result.trim_matches('-').to_string();
    if result.is_empty() {
        "session".to_string()
    } else {
        result.chars().take(64).collect()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify_replaces_spaces_with_dashes() {
        assert_eq!(slugify("Hello World"), "hello-world");
    }

    #[test]
    fn test_slugify_collapses_multiple_dashes() {
        assert_eq!(slugify("a--b"), "a-b");
    }

    #[test]
    fn test_slugify_max_64_chars() {
        let long = "a".repeat(100);
        let result = slugify(&long);
        assert!(result.len() <= 64);
        assert!(result.chars().all(|c| c == 'a'));
    }

    #[test]
    fn test_slugify_empty_string() {
        assert_eq!(slugify(""), "session");
    }

    #[test]
    fn test_slugify_special_chars() {
        // "Session: Memory?" → each char mapped:
        // S→s, e→e, s→s, s→s, i→i, o→o, n→n, :→-, space→-, M→m, e→e, m→m, o→o, r→r, y→y, ?→-
        // → "session--memory-" → collapse dashes → "session-memory-" → trim → "session-memory"
        assert_eq!(slugify("Session: Memory?"), "session-memory");
    }

    #[test]
    fn test_slugify_dots_become_dashes() {
        assert_eq!(slugify("2026-04-07T16:08:41.328Z"), "2026-04-07t16-08-41-328z");
    }

    #[test]
    fn test_slugify_leading_trailing_dashes_trimmed() {
        assert_eq!(slugify("-hello-"), "hello");
    }

    #[test]
    fn test_slugify_mixed_case_and_numbers() {
        assert_eq!(slugify("TestRoom42"), "testroom42");
    }

    #[test]
    fn test_millis_to_dt_basic() {
        // 2026-05-05 18:08:06 UTC is approximately 1777975686000 ms
        let dt = millis_to_dt(1777975686000);
        assert!(dt.starts_with("2026-05-"));
        assert!(dt.contains(":"));
    }

    #[test]
    fn test_millis_to_dt_epoch() {
        let dt = millis_to_dt(0);
        assert_eq!(dt, "1970-01-01 00:00:00");
    }

    #[test]
    fn test_session_max_chars() {
        std::env::remove_var("MEMPALACE_SESSION_MAX_CHARS");
        assert_eq!(session_max_chars(), 3000);
        std::env::set_var("MEMPALACE_SESSION_MAX_CHARS", "1000");
        assert_eq!(session_max_chars(), 1000);
        std::env::remove_var("MEMPALACE_SESSION_MAX_CHARS");
    }
}
