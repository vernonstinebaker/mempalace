use anyhow::Result;
use rusqlite::{params, Connection};

use crate::db::Database;
use crate::embed::Embedder;

/// Import OpenCode sessions from opencode.db into the palace.
/// Each session becomes one drawer: wing="opencode", room=slugified title.
/// Content = title + directory + recent assistant text (up to ~2000 chars).
pub fn import_sessions(
    db: &Database,
    oc_db_path: &str,
    embedder: Option<&Embedder>,
) -> Result<usize> {
    let oc = Connection::open(oc_db_path)?;

    // Fetch all sessions
    let mut stmt =
        oc.prepare("SELECT id, title, directory FROM session ORDER BY time_updated DESC")?;

    struct SessionRow {
        id: String,
        title: String,
        directory: String,
    }

    let sessions: Vec<SessionRow> = stmt
        .query_map([], |row| {
            Ok(SessionRow {
                id: row.get(0)?,
                title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                directory: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut count = 0usize;

    for session in &sessions {
        // Collect recent assistant text parts for this session (up to 2000 chars)
        let text_parts = collect_assistant_text(&oc, &session.id, 2000);

        // Build content
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

        let content = if text_parts.is_empty() {
            if dir_line.is_empty() {
                title_line
            } else {
                format!("{title_line}\n{dir_line}")
            }
        } else {
            if dir_line.is_empty() {
                format!("{title_line}\n\n{text_parts}")
            } else {
                format!("{title_line}\n{dir_line}\n\n{text_parts}")
            }
        };

        // Room = slugified title (or session id prefix)
        let room = if session.title.is_empty() {
            format!("session-{}", &session.id[..session.id.len().min(8)])
        } else {
            slugify(&session.title)
        };

        match db.add_drawer(
            "opencode",
            &room,
            &content,
            None,
            "import-sessions",
            embedder,
        ) {
            Ok(_) => count += 1,
            Err(e) => eprintln!("WARN: skipping session {}: {e}", session.id),
        }
    }

    Ok(count)
}

/// Collect text snippets from a session: first ~half from the start (establishes topic),
/// last ~half from the end (shows outcome). This way even long sessions that drifted
/// topic have both context represented.
fn collect_assistant_text(conn: &Connection, session_id: &str, max_chars: usize) -> String {
    // All assistant text parts for this session, chronological order.
    // Join through message to filter role='assistant' and type='text' only —
    // excludes user prompts, tool calls, patches, reasoning tokens, etc.
    let sql = "SELECT p.data FROM part p
               JOIN message m ON p.message_id = m.id
               WHERE p.session_id = ?1
               AND json_extract(m.data, '$.role') = 'assistant'
               AND json_extract(p.data, '$.type') = 'text'
               ORDER BY p.rowid ASC";

    let all_parts: Vec<String> = conn
        .prepare(sql)
        .ok()
        .and_then(|mut s| {
            s.query_map(params![session_id], |r| r.get::<_, String>(0))
                .ok()
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();

    // Extract text content — SQL already filtered for type='text', just get $.text
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

    // Take up to half from the start, up to half from the end
    // (deduplicating if the session is short enough to fit entirely)
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

    // Combine: if head and tail overlap (short session), just use head
    if head.len() + tail.len() <= max_chars {
        // Short session — head already has everything, append tail only if distinct
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
