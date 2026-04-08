use anyhow::Result;
use rusqlite::{params, Connection};

use crate::db::Database;

/// Import drawers and triples from a source palace.db into the current palace.
/// Uses INSERT OR IGNORE so existing content is not overwritten.
/// Returns (drawers_imported, triples_imported).
pub fn import_palace(db: &Database, source_path: &str) -> Result<(usize, usize)> {
    let src = Connection::open(source_path)?;

    // ── Import drawers ─────────────────────────────────────────────────────────
    let mut drawers_imported = 0usize;

    {
        let mut stmt = src.prepare(
            "SELECT id, wing, room, content, source_file, added_by, filed_at FROM drawers",
        )?;

        struct DrawerRow {
            id: String,
            wing: String,
            room: String,
            content: String,
            source_file: Option<String>,
            added_by: Option<String>,
            filed_at: Option<String>,
        }

        let rows: Vec<DrawerRow> = stmt
            .query_map([], |row| {
                Ok(DrawerRow {
                    id: row.get(0)?,
                    wing: row.get(1)?,
                    room: row.get(2)?,
                    content: row.get(3)?,
                    source_file: row.get(4)?,
                    added_by: row.get(5)?,
                    filed_at: row.get(6)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        for row in &rows {
            let added_by = row.added_by.as_deref().unwrap_or("import-palace");
            let filed_at = row.filed_at.as_deref().unwrap_or("1970-01-01");

            let changes = db.conn.execute(
                "INSERT OR IGNORE INTO drawers (id, wing, room, content, source_file, added_by, filed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    row.id,
                    row.wing,
                    row.room,
                    row.content,
                    row.source_file,
                    added_by,
                    filed_at,
                ],
            )?;
            drawers_imported += changes as usize;
        }
    }

    // ── Import triples ─────────────────────────────────────────────────────────
    let mut triples_imported = 0usize;

    // Check if triples table exists in source
    let has_triples: bool = src
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='triples'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);

    if has_triples {
        let mut stmt = src.prepare(
            "SELECT id, subject, predicate, object, valid_from, valid_until, source_closet
             FROM triples",
        )?;

        struct TripleRow {
            id: String,
            subject: String,
            predicate: String,
            object: String,
            valid_from: Option<String>,
            valid_until: Option<String>,
            source_closet: Option<String>,
        }

        let rows: Vec<TripleRow> = stmt
            .query_map([], |row| {
                Ok(TripleRow {
                    id: row.get(0)?,
                    subject: row.get(1)?,
                    predicate: row.get(2)?,
                    object: row.get(3)?,
                    valid_from: row.get(4)?,
                    valid_until: row.get(5)?,
                    source_closet: row.get(6)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        for row in &rows {
            let changes = db.conn.execute(
                "INSERT OR IGNORE INTO triples
                 (id, subject, predicate, object, valid_from, valid_until, source_closet)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    row.id,
                    row.subject,
                    row.predicate,
                    row.object,
                    row.valid_from,
                    row.valid_until,
                    row.source_closet,
                ],
            )?;
            triples_imported += changes as usize;
        }
    }

    Ok((drawers_imported, triples_imported))
}
