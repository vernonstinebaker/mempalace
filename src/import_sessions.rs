use anyhow::Result;
use rusqlite::{params, Connection};

use crate::db::Database;
use crate::embed::Embedder;
use crate::log::log;

/// A normalized session ready for the shared import pipeline.
#[derive(Clone)]
pub struct RawSession {
    pub id: String,
    pub title: String,
    /// Unix milliseconds of last update; drives incremental sync + filed_at.
    pub updated_at_ms: i64,
    /// Fully composed drawer body text.
    pub content: String,
}

/// Shared session→drawer pipeline used by every source adapter.
///
/// - Incremental: skips sessions with `updated_at_ms <= sync_state` unless `full`.
/// - Dedup: skips upsert when an existing drawer has byte-identical content.
/// - Stable IDs: drawer id = `{id_prefix}{session.id}`.
/// - Records max `updated_at_ms` in `sync_state` for the next run.
pub fn import_raw_sessions(
    db: &Database,
    source_key: &str,
    wing: &str,
    id_prefix: &str,
    sessions: Vec<RawSession>,
    embedder: Option<&Embedder>,
    full: bool,
) -> Result<usize> {
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

    let mut count = 0usize;
    let mut max_ts: i64 = since.unwrap_or(0);

    for session in sessions {
        if let Some(cut) = since {
            if session.updated_at_ms <= cut {
                continue;
            }
        }

        let room = if session.title.is_empty() {
            format!("session-{}", &session.id[..session.id.len().min(8)])
        } else {
            slugify(&session.title)
        };

        let drawer_id = format!("{id_prefix}{}", session.id);

        // Skip unchanged content so re-imports don't count as new imports.
        if db.get_drawer_content(&drawer_id)?.as_deref() == Some(session.content.as_str()) {
            if session.updated_at_ms > max_ts {
                max_ts = session.updated_at_ms;
            }
            continue;
        }

        let filed_at = millis_to_dt(session.updated_at_ms);
        match db.upsert_drawer(
            &drawer_id,
            wing,
            &room,
            &session.content,
            None,
            "import-sessions",
            Some(&filed_at),
            embedder,
        ) {
            Ok(_) => count += 1,
            Err(e) => log!("warn", "skipping session {}: {e}", session.id),
        }

        if session.updated_at_ms > max_ts {
            max_ts = session.updated_at_ms;
        }
    }

    // Record the max timestamp for next incremental sync
    if max_ts > since.unwrap_or(0) {
        db.set_sync_state(source_key, max_ts)?;
    }

    Ok(count)
}

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
    let oc = open_readonly(oc_db_path)?;
    let sessions = collect_opencode_style(&oc)?;
    import_raw_sessions(
        db,
        "opencode_sessions",
        "opencode",
        "oc_session_",
        sessions,
        embedder,
        full,
    )
}

/// Open another tool's SQLite database strictly read-only — never mutate it.
pub(crate) fn open_readonly(path: &str) -> Result<Connection> {
    let flags =
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    Ok(Connection::open_with_flags(path, flags)?)
}

// ── Codex adapter ─────────────────────────────────────────────────────────────

/// Import Codex CLI sessions (`~/.codex`) into the palace.
/// Wing="codex", stable drawer IDs `codex_{session_id}`.
pub fn import_codex(
    db: &Database,
    codex_home: &std::path::Path,
    embedder: Option<&Embedder>,
    full: bool,
) -> Result<usize> {
    let sessions = collect_codex_sessions(codex_home)?;
    import_raw_sessions(
        db,
        "codex_sessions",
        "codex",
        "codex_",
        sessions,
        embedder,
        full,
    )
}

/// Collect normalized sessions from a Codex home directory.
///
/// Layout: `sessions/YYYY/MM/DD/rollout-*.jsonl` where line 1 is
/// `session_meta` (payload.id / payload.cwd / payload.timestamp) followed by
/// `response_item` payloads. Titles and last-update times come from
/// `session_index.jsonl` when available.
pub fn collect_codex_sessions(codex_home: &std::path::Path) -> Result<Vec<RawSession>> {
    // Titles + updated_at from the index, keyed by session id.
    let mut index: std::collections::HashMap<String, (String, Option<i64>)> =
        std::collections::HashMap::new();
    let index_path = codex_home.join("session_index.jsonl");
    if let Ok(text) = std::fs::read_to_string(&index_path) {
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let Some(id) = v.get("id").and_then(|x| x.as_str()).map(str::to_string) else {
                continue;
            };
            let title = v
                .get("thread_name")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            let updated = v
                .get("updated_at")
                .and_then(|x| x.as_str())
                .and_then(parse_rfc3339_ms);
            index.insert(id, (title, updated));
        }
    }

    let mut rollouts: Vec<std::path::PathBuf> = Vec::new();
    let sessions_root = codex_home.join("sessions");
    let mut stack = vec![sessions_root];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                rollouts.push(p);
            }
        }
    }
    rollouts.sort();

    let max_chars = session_max_chars();
    let mut out = Vec::new();

    for path in rollouts {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };

        let mut id = String::new();
        let mut cwd = String::new();
        let mut meta_ts_ms: Option<i64> = None;
        let mut user_texts: Vec<String> = Vec::new();
        let mut assistant_texts: Vec<String> = Vec::new();

        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue; // malformed line — skip, keep going
            };
            match v.get("type").and_then(|t| t.as_str()) {
                Some("session_meta") => {
                    let payload = v.get("payload").cloned().unwrap_or(serde_json::Value::Null);
                    id = payload
                        .get("id")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string();
                    cwd = payload
                        .get("cwd")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string();
                    meta_ts_ms = payload
                        .get("timestamp")
                        .and_then(|x| x.as_str())
                        .and_then(parse_rfc3339_ms);
                }
                Some("response_item") => {
                    let payload = v.get("payload").cloned().unwrap_or(serde_json::Value::Null);
                    if payload.get("type").and_then(|t| t.as_str()) != Some("message") {
                        continue;
                    }
                    let role = payload.get("role").and_then(|r| r.as_str()).unwrap_or("");
                    // Only real conversation turns; never developer/system boilerplate.
                    let bucket = match role {
                        "user" => &mut user_texts,
                        "assistant" => &mut assistant_texts,
                        _ => continue,
                    };
                    if let Some(items) = payload.get("content").and_then(|c| c.as_array()) {
                        for item in items {
                            if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                                let trimmed = t.trim();
                                if !trimmed.is_empty() {
                                    bucket.push(trimmed.to_string());
                                }
                            }
                        }
                    }
                }
                _ => continue, // event_msg, function_call, etc.
            }
        }

        if id.is_empty() {
            continue;
        }

        let (title, index_updated) = index.get(&id).cloned().unwrap_or((String::new(), None));
        let updated_at_ms = index_updated.or(meta_ts_ms).unwrap_or(0);

        let title_line = if title.is_empty() {
            format!("Session: {}", &id[..id.len().min(16)])
        } else {
            format!("Session: {title}")
        };
        let mut content = String::new();
        content.push_str(&title_line);
        content.push('\n');
        content.push_str(&format!("Date: {}", millis_to_dt(updated_at_ms)));
        if !cwd.is_empty() {
            content.push('\n');
            content.push_str(&format!("Directory: {cwd}"));
        }
        if let Some(first) = user_texts.first() {
            let truncated: String = first.chars().take(200).collect();
            content.push_str(&format!("\nFirst message: {truncated}"));
        }
        let body = head_tail_texts(&assistant_texts, max_chars);
        if !body.is_empty() {
            content.push('\n');
            content.push_str(&body);
        }

        out.push(RawSession {
            id,
            title,
            updated_at_ms,
            content,
        });
    }

    Ok(out)
}

/// Parse an RFC3339 timestamp ("2026-08-04T09:59:24.241258Z") to unix millis.
/// Fractional seconds of any precision are accepted (truncated to ms).
pub(crate) fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = s.split_once('T')?;
    let mut dp = date.split('-');
    let year: i64 = dp.next()?.parse().ok()?;
    let month: u32 = dp.next()?.parse().ok()?;
    let day: u32 = dp.next()?.parse().ok()?;

    let time = time.trim_end_matches('Z').trim_end_matches("+00:00");
    let (hms, frac) = match time.split_once('.') {
        Some((a, b)) => (a, b),
        None => (time, ""),
    };
    let mut tp = hms.split(':');
    let hour: i64 = tp.next()?.parse().ok()?;
    let min: i64 = tp.next()?.parse().ok()?;
    let sec: i64 = tp.next()?.parse().ok()?;

    let frac_ms: i64 = if frac.is_empty() {
        0
    } else {
        let digits = frac.chars().take(3).collect::<String>();
        let padded = format!("{digits:0<3}");
        padded.parse().unwrap_or(0)
    };

    let days = days_from_civil(year, month, day);
    Some((days * 86_400 + hour * 3_600 + min * 60 + sec) * 1_000 + frac_ms)
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// ── Grok Build adapter ────────────────────────────────────────────────────────

/// Import Grok Build sessions (`~/.grok`) into the palace.
/// Wing="grok", stable drawer IDs `grok_{session_id}`.
pub fn import_grok(
    db: &Database,
    grok_home: &std::path::Path,
    embedder: Option<&Embedder>,
    full: bool,
) -> Result<usize> {
    let sessions = collect_grok_sessions(grok_home)?;
    import_raw_sessions(
        db,
        "grok_sessions",
        "grok",
        "grok_",
        sessions,
        embedder,
        full,
    )
}

/// Collect normalized sessions from a Grok home directory.
///
/// Layout: `sessions/<url-encoded-cwd>/<uuid>/` with `summary.json`
/// (info.id, info.cwd, updated_at, num_chat_messages) and
/// `chat_history.jsonl` lines of
/// `{type: system|user|assistant, content: string | [{type:"text",text}]}`.
pub fn collect_grok_sessions(grok_home: &std::path::Path) -> Result<Vec<RawSession>> {
    let sessions_root = grok_home.join("sessions");
    let mut session_dirs: Vec<std::path::PathBuf> = Vec::new();
    let mut stack = vec![sessions_root];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                // Leaf dirs contain summary.json; deeper dirs are project buckets.
                if p.join("summary.json").is_file() {
                    session_dirs.push(p);
                } else {
                    stack.push(p);
                }
            }
        }
    }
    session_dirs.sort();

    let max_chars = session_max_chars();
    let mut out = Vec::new();

    for dir in session_dirs {
        let Ok(summary_text) = std::fs::read_to_string(dir.join("summary.json")) else {
            continue;
        };
        let Ok(summary) = serde_json::from_str::<serde_json::Value>(&summary_text) else {
            continue;
        };
        let id = summary
            .pointer("/info/id")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            continue;
        }
        let num_msgs = summary
            .get("num_chat_messages")
            .and_then(|x| x.as_i64())
            .unwrap_or(0);
        if num_msgs == 0 {
            continue; // empty session — nothing worth remembering
        }
        let cwd = summary
            .pointer("/info/cwd")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        let updated_at_ms = summary
            .get("updated_at")
            .and_then(|x| x.as_str())
            .and_then(parse_rfc3339_ms)
            .unwrap_or(0);

        let mut user_texts: Vec<String> = Vec::new();
        let mut assistant_texts: Vec<String> = Vec::new();
        if let Ok(text) = std::fs::read_to_string(dir.join("chat_history.jsonl")) {
            for line in text.lines() {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                let role = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                let bucket = match role {
                    "user" => &mut user_texts,
                    "assistant" => &mut assistant_texts,
                    _ => continue, // system boilerplate never imported
                };
                match v.get("content") {
                    Some(serde_json::Value::String(s)) => {
                        let trimmed = s.trim();
                        if !trimmed.is_empty() {
                            bucket.push(trimmed.to_string());
                        }
                    }
                    Some(serde_json::Value::Array(items)) => {
                        for item in items {
                            if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                                if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                                    let trimmed = t.trim();
                                    if !trimmed.is_empty() {
                                        bucket.push(trimmed.to_string());
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        let title_line = format!("Session: {}", &id[..id.len().min(16)]);
        let mut content = String::new();
        content.push_str(&title_line);
        content.push('\n');
        content.push_str(&format!("Date: {}", millis_to_dt(updated_at_ms)));
        if !cwd.is_empty() {
            content.push('\n');
            content.push_str(&format!("Directory: {cwd}"));
        }
        if let Some(first) = user_texts.first() {
            let truncated: String = first.chars().take(200).collect();
            content.push_str(&format!("\nFirst message: {truncated}"));
        }
        let body = head_tail_texts(&assistant_texts, max_chars);
        if !body.is_empty() {
            content.push('\n');
            content.push_str(&body);
        }

        out.push(RawSession {
            id,
            title: String::new(), // Grok summaries carry no title; pipeline slugs the id
            updated_at_ms,
            content,
        });
    }

    Ok(out)
}

// ── Zcode adapter ─────────────────────────────────────────────────────────────

/// Import Zcode CLI sessions (`~/.zcode/cli/db/db.sqlite`) into the palace.
/// Wing="zcode", stable drawer IDs `zc_{session_id}`.
pub fn import_zcode(
    db: &Database,
    zcode_db_path: &str,
    embedder: Option<&Embedder>,
    full: bool,
) -> Result<usize> {
    let sessions = collect_zcode_sessions(zcode_db_path)?;
    import_raw_sessions(
        db,
        "zcode_sessions",
        "zcode",
        "zc_",
        sessions,
        embedder,
        full,
    )
}

/// Collect normalized sessions from a Zcode database. The schema
/// (`session`/`message`/`part` with JSON `data`) matches OpenCode's, so the
/// same extraction path is reused. The source DB is opened read-only.
pub fn collect_zcode_sessions(zcode_db_path: &str) -> Result<Vec<RawSession>> {
    let conn = open_readonly(zcode_db_path)?;
    collect_opencode_style(&conn)
}

// ── Multi-source routing ──────────────────────────────────────────────────────

pub const SOURCE_NAMES: &[&str] = &["opencode", "codex", "grok", "zcode"];

/// Filesystem locations of every known session store, env-overridable.
pub struct SourcePaths {
    pub opencode_db: std::path::PathBuf,
    pub codex_home: std::path::PathBuf,
    pub grok_home: std::path::PathBuf,
    pub zcode_db: std::path::PathBuf,
}

impl SourcePaths {
    /// Resolve store locations from env overrides or HOME defaults.
    pub fn resolve() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let env_path = |key: &str, default: std::path::PathBuf| -> std::path::PathBuf {
            std::env::var(key)
                .ok()
                .filter(|v| !v.is_empty())
                .map(std::path::PathBuf::from)
                .unwrap_or(default)
        };
        SourcePaths {
            opencode_db: env_path(
                "MEMPALACE_OPENCODE_DB",
                std::path::PathBuf::from(format!("{home}/.local/share/opencode/opencode.db")),
            ),
            codex_home: env_path(
                "MEMPALACE_CODEX_HOME",
                std::path::PathBuf::from(format!("{home}/.codex")),
            ),
            grok_home: env_path(
                "MEMPALACE_GROK_HOME",
                std::path::PathBuf::from(format!("{home}/.grok")),
            ),
            zcode_db: env_path(
                "MEMPALACE_ZCODE_DB",
                std::path::PathBuf::from(format!("{home}/.zcode/cli/db/db.sqlite")),
            ),
        }
    }

    fn exists(&self, source: &str) -> bool {
        match source {
            "opencode" => self.opencode_db.is_file(),
            "codex" => self.codex_home.join("sessions").is_dir(),
            "grok" => self.grok_home.join("sessions").is_dir(),
            "zcode" => self.zcode_db.is_file(),
            _ => false,
        }
    }
}

/// Import from one named source. Errors with `SourceNotFound` when the
/// store does not exist on disk.
pub fn import_one(
    db: &Database,
    source: &str,
    paths: &SourcePaths,
    embedder: Option<&Embedder>,
    full: bool,
) -> Result<(String, usize)> {
    if !SOURCE_NAMES.contains(&source) {
        anyhow::bail!(
            "InvalidSource: unknown session source '{source}' (expected one of: {})",
            SOURCE_NAMES.join(", ")
        );
    }
    if !paths.exists(source) {
        anyhow::bail!("SourceNotFound: no '{source}' session store found on this machine");
    }
    let n = match source {
        "opencode" => import_sessions(db, &paths.opencode_db.to_string_lossy(), embedder, full)?,
        "codex" => import_codex(db, &paths.codex_home, embedder, full)?,
        "grok" => import_grok(db, &paths.grok_home, embedder, full)?,
        "zcode" => import_zcode(db, &paths.zcode_db.to_string_lossy(), embedder, full)?,
        _ => unreachable!(),
    };
    Ok((source.to_string(), n))
}

/// Import from every store that exists. Missing stores are silently
/// skipped (auto mode must never fail just because a tool isn't installed).
/// Returns per-source imported counts in SOURCE_NAMES order.
pub fn import_auto(
    db: &Database,
    paths: &SourcePaths,
    embedder: Option<&Embedder>,
    full: bool,
) -> Result<Vec<(String, usize)>> {
    let mut results = Vec::new();
    for source in SOURCE_NAMES {
        if paths.exists(source) {
            // A broken individual store shouldn't sink the whole auto run.
            match import_one(db, source, paths, embedder, full) {
                Ok((name, n)) => results.push((name, n)),
                Err(e) => log!("warn", "import from {source} failed: {e}"),
            }
        }
    }
    Ok(results)
}

/// Validate/normalize an `index-sessions --source` CLI value.
/// Absent flag means "auto".
pub fn normalize_source_arg(raw: Option<&str>) -> Result<String> {
    match raw.unwrap_or("auto") {
        s if SOURCE_NAMES.contains(&s) || s == "auto" => Ok(s.to_string()),
        other => Err(anyhow::anyhow!(
            "InvalidSource: unknown session source '{other}' (expected one of: auto, {})",
            SOURCE_NAMES.join(", ")
        )),
    }
}

/// Collect normalized sessions from an OpenCode/Zcode-style schema
/// (`session` + `message` + `part` tables, JSON `data` columns).
fn collect_opencode_style(conn: &Connection) -> Result<Vec<RawSession>> {
    let max_chars = session_max_chars();

    let mut stmt = conn.prepare(
        "SELECT id, title, directory, time_updated FROM session ORDER BY time_updated DESC",
    )?;

    struct SessionRow {
        id: String,
        title: String,
        directory: String,
        time_updated: i64,
    }

    let sessions: Vec<SessionRow> = stmt
        .query_map([], |row| {
            Ok(SessionRow {
                id: row.get(0)?,
                title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                directory: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                time_updated: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut out = Vec::with_capacity(sessions.len());
    for session in &sessions {
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

        let tool_line = collect_tool_names(conn, &session.id);
        let first_msg = collect_first_user_message(conn, &session.id);
        let summary = session_summary(conn, &session.id);
        let text_parts = collect_assistant_text(conn, &session.id, max_chars);

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

        out.push(RawSession {
            id: session.id.clone(),
            title: session.title.clone(),
            updated_at_ms: session.time_updated,
            content,
        });
    }
    Ok(out)
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
            .map(|iter| iter.filter_map(|r| r.ok().flatten()).collect())
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

    head_tail_texts(&texts, max_chars)
}

/// Compose a bounded body from ordered text snippets: first ~half of the
/// budget from the head, last ~half from the tail, joined by an ellipsis
/// when both are present. Shared by every source adapter.
pub(crate) fn head_tail_texts(texts: &[String], max_chars: usize) -> String {
    if texts.is_empty() {
        return String::new();
    }

    let half = max_chars / 2;
    let mut head = String::new();
    for t in texts {
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

pub(crate) fn slugify(s: &str) -> String {
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
        assert_eq!(
            slugify("2026-04-07T16:08:41.328Z"),
            "2026-04-07t16-08-41-328z"
        );
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

    // ── shared pipeline tests (26.1) ───────────────────────────────────────────

    fn test_db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Database::open(dir.path().to_str().unwrap()).unwrap();
        (dir, db)
    }

    fn raw(id: &str, title: &str, updated_ms: i64, content: &str) -> RawSession {
        RawSession {
            id: id.to_string(),
            title: title.to_string(),
            updated_at_ms: updated_ms,
            content: content.to_string(),
        }
    }

    #[test]
    fn test_import_pipeline_dedups_by_stable_id() {
        let (_dir, db) = test_db();
        let sessions = vec![raw(
            "abc123",
            "My Session",
            1_770_000_000_000,
            "hello world",
        )];
        let n1 = import_raw_sessions(
            &db,
            "test_src",
            "testwing",
            "tw_",
            sessions.clone(),
            None,
            false,
        )
        .unwrap();
        assert_eq!(n1, 1);
        // Re-import identical content → skipped, not counted again.
        let n2 =
            import_raw_sessions(&db, "test_src", "testwing", "tw_", sessions, None, true).unwrap();
        assert_eq!(n2, 0);
        // Changed content with same stable ID → re-imported (updated), still one drawer.
        let changed = vec![raw(
            "abc123",
            "My Session",
            1_770_000_001_000,
            "changed body",
        )];
        let n3 =
            import_raw_sessions(&db, "test_src", "testwing", "tw_", changed, None, true).unwrap();
        assert_eq!(n3, 1);
        assert_eq!(db.get_drawer_count(), 1);
        assert!(db.get_drawer_content("tw_abc123").unwrap().is_some());
    }

    #[test]
    fn test_import_pipeline_updates_sync_state_to_max_ts() {
        let (_dir, db) = test_db();
        let sessions = vec![
            raw("a", "A", 1_000, "content a"),
            raw("b", "B", 5_000, "content b"),
            raw("c", "C", 3_000, "content c"),
        ];
        import_raw_sessions(&db, "sync_src", "w", "p_", sessions, None, false).unwrap();
        assert_eq!(db.get_sync_state("sync_src"), 5_000);

        // Incremental run: only sessions strictly newer than cutoff are imported.
        let newer = vec![
            raw("d", "D", 5_000, "content d2"),
            raw("e", "E", 9_000, "content e"),
        ];
        let n = import_raw_sessions(&db, "sync_src", "w", "p_", newer, None, false).unwrap();
        assert_eq!(n, 1); // only "e" (d is at/below cutoff)
        assert_eq!(db.get_sync_state("sync_src"), 9_000);
    }

    /// Build a minimal OpenCode/Zcode-style source database for adapter tests.
    /// Returns the path to the created SQLite file.
    pub(crate) fn make_session_db(dir: &std::path::Path, file_name: &str) -> String {
        let path = dir.join(file_name);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (
                id text primary key,
                project_id text not null default '',
                directory text not null default '',
                title text not null default '',
                time_created integer not null,
                time_updated integer not null
            );
            CREATE TABLE message (
                id text primary key,
                session_id text not null,
                time_created integer not null,
                time_updated integer not null,
                data text not null
            );
            CREATE TABLE part (
                id text primary key,
                message_id text not null,
                session_id text not null,
                data text not null
            );",
        )
        .unwrap();
        path.to_str().unwrap().to_string()
    }

    pub(crate) fn insert_session(
        conn: &Connection,
        id: &str,
        title: &str,
        directory: &str,
        updated_ms: i64,
        user_text: &str,
        assistant_texts: &[&str],
    ) {
        conn.execute(
            "INSERT INTO session (id, title, directory, time_created, time_updated)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, title, directory, updated_ms],
        )
        .unwrap();
        let msg_id = format!("msg_{id}");
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            params![msg_id, id, updated_ms, format!(r#"{{"role":"user"}}"#)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, data) VALUES (?1, ?2, ?3, ?4)",
            params![format!("p1_{id}"), msg_id, id, json_part(user_text)],
        )
        .unwrap();
        for (i, t) in assistant_texts.iter().enumerate() {
            let mid = format!("msg_{id}_a{i}");
            conn.execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data)
                 VALUES (?1, ?2, ?3, ?3, ?4)",
                params![mid, id, updated_ms, format!(r#"{{"role":"assistant"}}"#)],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO part (id, message_id, session_id, data) VALUES (?1, ?2, ?3, ?4)",
                params![format!("pa{i}_{id}"), mid, id, json_part(t)],
            )
            .unwrap();
        }
    }

    fn json_part(text: &str) -> String {
        format!(
            r#"{{"type":"text","text":{}}}"#,
            serde_json::to_string(text).unwrap()
        )
    }

    // ── Codex adapter tests (26.2) ─────────────────────────────────────────────

    /// Create a fake ~/.codex layout: sessions/YYYY/MM/DD/rollout-*.jsonl
    /// plus session_index.jsonl. Returns the codex home path.
    fn make_codex_home(dir: &std::path::Path) -> String {
        let home = dir.join("codex");
        std::fs::create_dir_all(home.join("sessions/2026/08/01")).unwrap();
        home.to_str().unwrap().to_string()
    }

    fn write_rollout(home: &std::path::Path, name: &str, lines: &[String]) {
        let p = home.join("sessions/2026/08/01").join(name);
        std::fs::write(p, lines.join("\n") + "\n").unwrap();
    }

    fn meta_line(id: &str, cwd: &str, ts: &str) -> String {
        format!(
            r#"{{"timestamp":"{ts}","type":"session_meta","payload":{{"id":"{id}","timestamp":"{ts}","cwd":"{cwd}"}}}}"#
        )
    }

    fn msg_line(role: &str, text: &str, ts: &str) -> String {
        format!(
            r#"{{"timestamp":"{ts}","type":"response_item","payload":{{"type":"message","role":"{role}","content":[{{"type":"input_text","text":{}}}]}}}}"#,
            serde_json::to_string(text).unwrap()
        )
    }

    fn write_index(home: &std::path::Path, entries: &[String]) {
        let p = home.join("session_index.jsonl");
        std::fs::write(p, entries.join("\n") + "\n").unwrap();
    }

    #[test]
    fn test_codex_parse_rollout_yields_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home_s = make_codex_home(tmp.path());
        let home = std::path::Path::new(&home_s);
        write_rollout(
            home,
            "rollout-2026-08-01T10-00-00-11111111-2222-3333-4444-555555555555.jsonl",
            &[
                meta_line(
                    "11111111-2222-3333-4444-555555555555",
                    "/proj/api",
                    "2026-08-01T10:00:00.000Z",
                ),
                msg_line("user", "fix the login bug", "2026-08-01T10:00:05.000Z"),
                msg_line(
                    "assistant",
                    "I traced it to the token refresh path.",
                    "2026-08-01T10:00:20.000Z",
                ),
            ],
        );
        write_index(
            home,
            &[format!(
                r#"{{"id":"11111111-2222-3333-4444-555555555555","thread_name":"Fix login bug","updated_at":"2026-08-01T10:05:00.000000Z"}}"#
            )],
        );

        let sessions = collect_codex_sessions(home).unwrap();
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.id, "11111111-2222-3333-4444-555555555555");
        assert_eq!(s.title, "Fix login bug");
        assert!(s.content.contains("Directory: /proj/api"));
        assert!(s.content.contains("Session: Fix login bug"));
        assert!(s.content.contains("First message: fix the login bug"));
        assert!(s.content.contains("token refresh path"));
        // updated_at from index: 2026-08-01T10:05:00Z
        assert_eq!(s.updated_at_ms, 1_785_578_700_000);
    }

    #[test]
    fn test_codex_skips_malformed_lines() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home_s = make_codex_home(tmp.path());
        let home = std::path::Path::new(&home_s);
        write_rollout(
            home,
            "rollout-2026-08-01T10-00-00-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl",
            &[
                "this is not json".to_string(),
                r#"{"unexpected":"shape"}"#.to_string(),
                meta_line(
                    "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                    "/x",
                    "2026-08-01T09:00:00.000Z",
                ),
                r#"{"timestamp":"x","type":"response_item","payload":{"type":"function_call"}}"#
                    .to_string(),
                msg_line("user", "still works", "2026-08-01T09:00:10.000Z"),
            ],
        );
        let sessions = collect_codex_sessions(home).unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].content.contains("still works"));
    }

    #[test]
    fn test_codex_developer_role_excluded() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home_s = make_codex_home(tmp.path());
        let home = std::path::Path::new(&home_s);
        write_rollout(
            home,
            "rollout-2026-08-01T10-00-00-11112222-3333-4444-5555-666666666666.jsonl",
            &[
                meta_line(
                    "11112222-3333-4444-5555-666666666666",
                    "/p",
                    "2026-08-01T10:00:00.000Z",
                ),
                msg_line(
                    "developer",
                    "<permissions instructions> SECRET BOILERPLATE",
                    "2026-08-01T10:00:01.000Z",
                ),
                msg_line(
                    "system",
                    "You are Codex SECRET BOILERPLATE",
                    "2026-08-01T10:00:02.000Z",
                ),
                msg_line("user", "real question", "2026-08-01T10:00:03.000Z"),
            ],
        );
        let sessions = collect_codex_sessions(home).unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(!sessions[0].content.contains("BOILERPLATE"));
        assert!(sessions[0].content.contains("real question"));
    }

    #[test]
    fn test_codex_incremental_sync_uses_index_updated_at() {
        let (_dir, db) = test_db();
        let tmp = tempfile::TempDir::new().unwrap();
        let home_s = make_codex_home(tmp.path());
        let home = std::path::Path::new(&home_s);
        write_rollout(
            home,
            "rollout-2026-08-01T10-00-00-aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl",
            &[
                meta_line(
                    "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee",
                    "/p",
                    "2026-08-01T10:00:00.000Z",
                ),
                msg_line("user", "q1", "2026-08-01T10:00:03.000Z"),
            ],
        );
        write_index(
            home,
            &[r#"{"id":"aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee","thread_name":"t1","updated_at":"2026-08-01T10:30:00.000000Z"}"#.to_string()],
        );

        let n1 = import_codex(&db, home, None, false).unwrap();
        assert_eq!(n1, 1);

        // Re-run without changes → nothing new.
        let n2 = import_codex(&db, home, None, false).unwrap();
        assert_eq!(n2, 0);

        // Index reports a newer update for the same session → re-imported (updated).
        write_index(
            home,
            &[r#"{"id":"aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee","thread_name":"t1 renamed","updated_at":"2026-08-02T10:30:00.000000Z"}"#.to_string()],
        );
        let n3 = import_codex(&db, home, None, false).unwrap();
        assert_eq!(n3, 1);
        assert_eq!(db.get_drawer_count(), 1); // stable ID, no duplicate
    }

    #[test]
    fn test_codex_stable_id_dedup() {
        let (_dir, db) = test_db();
        let tmp = tempfile::TempDir::new().unwrap();
        let home_s = make_codex_home(tmp.path());
        let home = std::path::Path::new(&home_s);
        write_rollout(
            home,
            "rollout-2026-08-01T10-00-00-dddd1111-2222-3333-4444-555555555555.jsonl",
            &[
                meta_line(
                    "dddd1111-2222-3333-4444-555555555555",
                    "/p",
                    "2026-08-01T10:00:00.000Z",
                ),
                msg_line("user", "dedup me", "2026-08-01T10:00:03.000Z"),
            ],
        );
        let n1 = import_codex(&db, home, None, true).unwrap();
        let n2 = import_codex(&db, home, None, true).unwrap();
        assert_eq!(n1, 1);
        assert_eq!(n2, 0);
        assert_eq!(db.get_drawer_count(), 1);
        assert!(db
            .get_drawer_content("codex_dddd1111-2222-3333-4444-555555555555")
            .unwrap()
            .is_some());
    }

    // ── Grok Build adapter tests (26.3) ────────────────────────────────────────

    use std::fmt::Write as _;

    /// Create a fake ~/.grok/sessions/<encoded-cwd>/<uuid>/ session dir.
    fn make_grok_session(
        grok_home: &std::path::Path,
        uuid: &str,
        summary_json: &str,
        chat_lines: &[String],
    ) {
        let dir = grok_home.join("sessions/%2FUsers%2Fvds").join(uuid);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("summary.json"), summary_json).unwrap();
        if !chat_lines.is_empty() {
            let mut body = String::new();
            for l in chat_lines {
                let _ = writeln!(body, "{l}");
            }
            std::fs::write(dir.join("chat_history.jsonl"), body).unwrap();
        }
    }

    fn grok_summary(id: &str, num_msgs: i64) -> String {
        format!(
            r#"{{"info":{{"id":"{id}","cwd":"/Users/vds"}},"session_summary":"","created_at":"2026-08-08T12:29:50.440630Z","updated_at":"2026-08-08T12:31:00.000000Z","num_messages":0,"num_chat_messages":{num_msgs}}}"#
        )
    }

    fn grok_chat(role: &str, content: serde_json::Value) -> String {
        format!(
            r#"{{"type":"{role}","content":{}}}"#,
            serde_json::to_string(&content).unwrap()
        )
    }

    #[test]
    fn test_grok_parse_summary_and_chat_history() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("grok");
        make_grok_session(
            &home,
            "019fe159-db8e-7f82-a5cc-c7c1ba2786c9",
            &grok_summary("019fe159-db8e-7f82-a5cc-c7c1ba2786c9", 2),
            &[
                grok_chat(
                    "user",
                    serde_json::json!([{"type":"text","text":"profile the rust binary"}]),
                ),
                grok_chat(
                    "assistant",
                    serde_json::json!([{"type":"text","text":"Used instruments for sampling."}]),
                ),
            ],
        );
        let sessions = collect_grok_sessions(&home).unwrap();
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.id, "019fe159-db8e-7f82-a5cc-c7c1ba2786c9");
        assert!(s.content.contains("Directory: /Users/vds"));
        // updated_at 2026-08-08T12:31:00Z
        assert_eq!(s.updated_at_ms, 1_786_192_260_000);
        assert!(s.content.contains("First message: profile the rust binary"));
        assert!(s.content.contains("instruments for sampling"));
    }

    #[test]
    fn test_grok_handles_string_and_array_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("grok");
        make_grok_session(
            &home,
            "aaaa0000-0000-0000-0000-000000000001",
            &grok_summary("aaaa0000-0000-0000-0000-000000000001", 2),
            &[
                grok_chat("user", serde_json::json!("plain string question")),
                grok_chat("assistant", serde_json::json!("plain string answer")),
            ],
        );
        let sessions = collect_grok_sessions(&home).unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].content.contains("plain string answer"));
        assert!(sessions[0].content.contains("plain string question"));
    }

    #[test]
    fn test_grok_system_role_excluded() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("grok");
        make_grok_session(
            &home,
            "bbbb0000-0000-0000-0000-000000000002",
            &grok_summary("bbbb0000-0000-0000-0000-000000000002", 2),
            &[
                grok_chat(
                    "system",
                    serde_json::json!("You are Grok SECRET BOILERPLATE"),
                ),
                grok_chat("user", serde_json::json!("visible question")),
            ],
        );
        let sessions = collect_grok_sessions(&home).unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(!sessions[0].content.contains("BOILERPLATE"));
        assert!(sessions[0].content.contains("visible question"));
    }

    #[test]
    fn test_grok_skips_zero_message_sessions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("grok");
        make_grok_session(
            &home,
            "cccc0000-0000-0000-0000-000000000003",
            &grok_summary("cccc0000-0000-0000-0000-000000000003", 0),
            &[],
        );
        let sessions = collect_grok_sessions(&home).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_grok_stable_id_dedup() {
        let (_dir, db) = test_db();
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("grok");
        make_grok_session(
            &home,
            "dddd0000-0000-0000-0000-000000000004",
            &grok_summary("dddd0000-0000-0000-0000-000000000004", 2),
            &[grok_chat("user", serde_json::json!("grok dedup"))],
        );
        let n1 = import_grok(&db, &home, None, true).unwrap();
        let n2 = import_grok(&db, &home, None, true).unwrap();
        assert_eq!(n1, 1);
        assert_eq!(n2, 0);
        assert_eq!(db.get_drawer_count(), 1);
        assert!(db
            .get_drawer_content("grok_dddd0000-0000-0000-0000-000000000004")
            .unwrap()
            .is_some());
    }

    // ── Zcode adapter tests (26.4) ─────────────────────────────────────────────

    #[test]
    fn test_zcode_parse_sessions_from_sqlite() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = make_session_db(tmp.path(), "zcode.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            insert_session(
                &conn,
                "sess-1",
                "Explore the Rust codebase",
                "/Users/vds/ZCodeProject",
                1_786_200_000_000,
                "what does the importer do?",
                &["It normalizes sessions into drawers."],
            );
        }
        let sessions = collect_zcode_sessions(&db_path).unwrap();
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.id, "sess-1");
        assert_eq!(s.title, "Explore the Rust codebase");
        assert!(s.content.contains("Directory: /Users/vds/ZCodeProject"));
        assert_eq!(s.updated_at_ms, 1_786_200_000_000);
        assert!(s.content.contains("Session: Explore the Rust codebase"));
        assert!(s.content.contains("normalizes sessions"));
    }

    #[test]
    fn test_zcode_message_data_json_extraction() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = make_session_db(tmp.path(), "zcode.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            insert_session(
                &conn,
                "sess-2",
                "T",
                "/d",
                1_000,
                "user text here",
                &["assistant text here"],
            );
            // Malformed JSON in a part must not break extraction.
            conn.execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data)
                 VALUES ('msg_sess-2_x', 'sess-2', 1000, 1000, '{\"role\":\"assistant\"}')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO part (id, message_id, session_id, data)
                 VALUES ('bad1', 'msg_sess-2_x', 'sess-2', 'not json at all')",
                [],
            )
            .unwrap();
        }
        let sessions = collect_zcode_sessions(&db_path).unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].content.contains("assistant text here"));
        assert!(!sessions[0].content.contains("not json"));
    }

    #[test]
    fn test_zcode_incremental_sync() {
        let (_dir, db) = test_db();
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = make_session_db(tmp.path(), "zcode.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            insert_session(&conn, "s1", "Old", "/d", 1_000, "q", &["a"]);
        }
        let n1 = import_zcode(&db, &db_path, None, false).unwrap();
        assert_eq!(n1, 1);

        {
            let conn = Connection::open(&db_path).unwrap();
            insert_session(&conn, "s2", "Newer", "/d", 5_000, "q2", &["a2"]);
        }
        let n2 = import_zcode(&db, &db_path, None, false).unwrap();
        assert_eq!(n2, 1); // only s2
        assert_eq!(db.get_sync_state("zcode_sessions"), 5_000);
    }

    #[test]
    fn test_zcode_stable_id_dedup() {
        let (_dir, db) = test_db();
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = make_session_db(tmp.path(), "zcode.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            insert_session(&conn, "s9", "Same", "/d", 1_000, "q", &["a"]);
        }
        let n1 = import_zcode(&db, &db_path, None, true).unwrap();
        let n2 = import_zcode(&db, &db_path, None, true).unwrap();
        assert_eq!(n1, 1);
        assert_eq!(n2, 0);
        assert_eq!(db.get_drawer_count(), 1);
        assert!(db.get_drawer_content("zc_s9").unwrap().is_some());
    }

    // ── Multi-source routing tests (26.5) ──────────────────────────────────────

    use std::path::PathBuf;

    fn fixture_paths(tmp: &tempfile::TempDir, with_stores: &[&str]) -> SourcePaths {
        let root = tmp.path();
        // Build a codex store when requested.
        if with_stores.contains(&"codex") {
            let home = make_codex_home(root);
            write_rollout(
                std::path::Path::new(&home),
                "rollout-2026-08-01T10-00-00-99990000-1111-2222-3333-444444444444.jsonl",
                &[
                    meta_line(
                        "99990000-1111-2222-3333-444444444444",
                        "/p",
                        "2026-08-01T10:00:00.000Z",
                    ),
                    msg_line("user", "route codex", "2026-08-01T10:00:03.000Z"),
                ],
            );
        }
        if with_stores.contains(&"grok") {
            let home = root.join("grok");
            make_grok_session(
                &home,
                "88880000-0000-0000-0000-000000000005",
                &grok_summary("88880000-0000-0000-0000-000000000005", 2),
                &[grok_chat("user", serde_json::json!("route grok"))],
            );
        }
        let zcode_db: PathBuf = if with_stores.contains(&"zcode") {
            let p = make_session_db(root, "routing-zcode.db");
            {
                let conn = Connection::open(&p).unwrap();
                insert_session(&conn, "route-1", "Route Zcode", "/d", 1_000, "q", &["a"]);
            }
            PathBuf::from(p)
        } else {
            root.join("no-zcode.db")
        };
        let opencode_db: PathBuf = if with_stores.contains(&"opencode") {
            let p = make_session_db(root, "routing-opencode.db");
            {
                let conn = Connection::open(&p).unwrap();
                insert_session(
                    &conn,
                    "oc-route-1",
                    "Route OpenCode",
                    "/d",
                    1_000,
                    "q",
                    &["a"],
                );
            }
            PathBuf::from(p)
        } else {
            root.join("no-opencode.db")
        };
        SourcePaths {
            opencode_db,
            codex_home: PathBuf::from(if with_stores.contains(&"codex") {
                root.join("codex")
            } else {
                root.join("no-codex")
            }),
            grok_home: PathBuf::from(if with_stores.contains(&"grok") {
                root.join("grok")
            } else {
                root.join("no-grok")
            }),
            zcode_db,
        }
    }

    #[test]
    fn test_mcp_import_source_param_routes() {
        for source in SOURCE_NAMES {
            let (_dir, db) = test_db();
            let tmp = tempfile::TempDir::new().unwrap();
            let paths = fixture_paths(&tmp, &[source]);
            let (name, n) = import_one(&db, source, &paths, None, true).unwrap();
            assert_eq!(name.as_str(), *source);
            assert_eq!(n, 1, "source {source} should import its fixture session");
        }
    }

    #[test]
    fn test_mcp_import_auto_skips_missing_stores() {
        let (_dir, db) = test_db();
        let tmp = tempfile::TempDir::new().unwrap();
        // No stores exist at all → auto succeeds with zero results.
        let empty = fixture_paths(&tmp, &[]);
        let results = import_auto(&db, &empty, None, false).unwrap();
        assert!(results.is_empty());

        // Only two of four stores exist → only those appear.
        let tmp2 = tempfile::TempDir::new().unwrap();
        let partial = fixture_paths(&tmp2, &["codex", "zcode"]);
        let mut results = import_auto(&db, &partial, None, false).unwrap();
        results.sort();
        assert_eq!(
            results,
            vec![("codex".to_string(), 1), ("zcode".to_string(), 1),]
        );
    }

    #[test]
    fn test_mcp_import_explicit_missing_store_errors() {
        let (_dir, db) = test_db();
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = fixture_paths(&tmp, &[]);
        let err = import_one(&db, "codex", &paths, None, false).unwrap_err();
        assert!(err.to_string().contains("SourceNotFound"), "got: {err}");
        let err = import_one(&db, "bogus", &paths, None, false).unwrap_err();
        assert!(err.to_string().contains("InvalidSource"), "got: {err}");
    }

    #[test]
    fn test_tools_json_import_sessions_documents_sources() {
        let tools = crate::mcp::TOOLS_JSON;
        assert!(tools.contains("\"mempalace_import_sessions\""));
        assert!(tools.contains("\"source\""));
        for s in SOURCE_NAMES {
            assert!(tools.contains(s), "TOOLS_JSON missing source name {s}");
        }
        assert!(tools.contains("\"auto\""));
    }

    // ── CLI flag tests (26.6) ──────────────────────────────────────────────────

    #[test]
    fn test_cli_index_sessions_source_flag() {
        assert_eq!(normalize_source_arg(None).unwrap(), "auto");
        for s in SOURCE_NAMES {
            assert_eq!(normalize_source_arg(Some(s)).unwrap(), *s);
        }
        let err = normalize_source_arg(Some("claude")).unwrap_err();
        assert!(err.to_string().contains("InvalidSource"), "got: {err}");
    }
}
