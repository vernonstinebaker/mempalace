use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::Path;

use crate::embed::Embedder;
use crate::log::log;

pub struct Database {
    pub conn: Connection,
}

impl Database {
    pub fn open(dir: &str) -> Result<Self> {
        // Ensure the directory exists
        std::fs::create_dir_all(dir)?;
        let db_path = Path::new(dir).join("palace.db");
        let conn = Connection::open(&db_path)?;

        // Performance pragmas
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=OFF;",
        )?;

        let db = Self { conn };
        db.create_tables()?;
        Ok(db)
    }

    fn create_tables(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS drawers (
                id TEXT PRIMARY KEY,
                wing TEXT NOT NULL,
                room TEXT NOT NULL,
                content TEXT NOT NULL,
                source_file TEXT,
                added_by TEXT,
                filed_at DATETIME DEFAULT CURRENT_TIMESTAMP
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS drawers_fts USING fts5(
                content, wing, room,
                content='drawers',
                content_rowid='rowid'
            );

            CREATE TRIGGER IF NOT EXISTS drawers_ai AFTER INSERT ON drawers BEGIN
                INSERT INTO drawers_fts(rowid, content, wing, room)
                VALUES (new.rowid, new.content, new.wing, new.room);
            END;

            CREATE TRIGGER IF NOT EXISTS drawers_ad AFTER DELETE ON drawers BEGIN
                INSERT INTO drawers_fts(drawers_fts, rowid, content, wing, room)
                VALUES ('delete', old.rowid, old.content, old.wing, old.room);
            END;

            CREATE TABLE IF NOT EXISTS triples (
                id TEXT PRIMARY KEY,
                subject TEXT NOT NULL,
                predicate TEXT NOT NULL,
                object TEXT NOT NULL,
                valid_from DATETIME,
                valid_until DATETIME,
                source_closet TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_drawers_wing ON drawers(wing);
            CREATE INDEX IF NOT EXISTS idx_drawers_room ON drawers(room);
            CREATE INDEX IF NOT EXISTS idx_drawers_wing_room ON drawers(wing, room);
            CREATE INDEX IF NOT EXISTS idx_triples_subject ON triples(subject);
            CREATE INDEX IF NOT EXISTS idx_triples_predicate ON triples(predicate);
            CREATE INDEX IF NOT EXISTS idx_triples_object ON triples(object);",
        )?;

        // vec0 table — ignore if sqlite-vec not loaded
        let _ = self.conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS vec_drawers USING vec0(embedding float[384]);",
        );

        // Shadow table to track which drawers have been embedded
        // (vec0 doesn't support reliable rowid point-lookups)
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS vec_embedded (rowid INTEGER PRIMARY KEY);",
        )?;

        Ok(())
    }

    // ── drawer count ──────────────────────────────────────────────────────────

    pub fn get_drawer_count(&self) -> i64 {
        self.conn
            .query_row("SELECT COUNT(*) FROM drawers", [], |r| r.get(0))
            .unwrap_or(0)
    }

    // ── wing / room queries ───────────────────────────────────────────────────

    pub fn get_wing_counts(&self) -> Result<Value> {
        let mut stmt = self
            .conn
            .prepare("SELECT wing, COUNT(*) FROM drawers GROUP BY wing")?;
        let mut obj = serde_json::Map::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let wing: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            obj.insert(wing, json!(count));
        }
        Ok(Value::Object(obj))
    }

    pub fn get_room_counts(&self, wing_filter: Option<&str>) -> Result<Value> {
        let mut obj = serde_json::Map::new();
        if let Some(wing) = wing_filter {
            let mut stmt = self
                .conn
                .prepare("SELECT room, COUNT(*) FROM drawers WHERE wing = ?1 GROUP BY room")?;
            let mut rows = stmt.query(params![wing])?;
            while let Some(row) = rows.next()? {
                let room: String = row.get(0)?;
                let count: i64 = row.get(1)?;
                obj.insert(room, json!(count));
            }
        } else {
            let mut stmt = self
                .conn
                .prepare("SELECT room, COUNT(*) FROM drawers GROUP BY room")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let room: String = row.get(0)?;
                let count: i64 = row.get(1)?;
                obj.insert(room, json!(count));
            }
        }
        Ok(Value::Object(obj))
    }

    pub fn get_taxonomy(&self) -> Result<Value> {
        let mut stmt = self
            .conn
            .prepare("SELECT wing, room, COUNT(*) FROM drawers GROUP BY wing, room")?;
        let mut root = serde_json::Map::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let wing: String = row.get(0)?;
            let room: String = row.get(1)?;
            let count: i64 = row.get(2)?;
            let wing_obj = root
                .entry(wing)
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Value::Object(m) = wing_obj {
                m.insert(room, json!(count));
            }
        }
        Ok(Value::Object(root))
    }

    // ── embedding ─────────────────────────────────────────────────────────────

    /// Backfill embeddings for all drawers that don't have one yet.
    /// Returns (total, embedded, failed) counts.
    ///
    /// Note: vec0 virtual tables don't support reliable rowid point-lookups for
    /// existence checks. We use a regular shadow table `vec_embedded` to track
    /// which rowids have been indexed, and embed everything missing from it.
    pub fn backfill_embeddings(
        &self,
        embedder: &crate::embed::Embedder,
    ) -> Result<(usize, usize, usize)> {
        // Ensure the shadow table exists
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS vec_embedded (rowid INTEGER PRIMARY KEY);",
        )?;

        // Sync vec_embedded from vec_drawers_rowids (the internal vec0 shadow table)
        // so we don't re-embed rows that are already in vec_drawers.
        // vec_drawers_rowids is an internal sqlite-vec table — if it doesn't exist
        // (e.g. vec0 not loaded), this is a no-op.
        let _ = self.conn.execute_batch(
            "INSERT OR IGNORE INTO vec_embedded(rowid)
             SELECT rowid FROM vec_drawers_rowids;",
        );

        // Find drawers not yet in the shadow table
        let mut stmt = self.conn.prepare(
            "SELECT d.rowid, d.content FROM drawers d
             WHERE d.rowid NOT IN (SELECT rowid FROM vec_embedded)
             ORDER BY d.rowid ASC",
        )?;

        let rows: Vec<(i64, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        let total = rows.len();
        let mut embedded = 0usize;
        let mut failed = 0usize;

        let mut first_embed_error_logged = false;
        let mut first_vec_error_logged = false;
        for (i, (rowid, content)) in rows.iter().enumerate() {
            if let Some(vec_bytes) = embedder.embed(content) {
                match self.add_embedding(*rowid, &vec_bytes) {
                    Ok(()) => {
                        // Mark as done in shadow table
                        let _ = self.conn.execute(
                            "INSERT OR IGNORE INTO vec_embedded(rowid) VALUES (?1)",
                            params![rowid],
                        );
                        embedded += 1;
                    }
                    Err(e) => {
                        if !first_vec_error_logged {
                            log!("warn", "[backfill] add_embedding error (rowid={rowid}): {e}");
                            first_vec_error_logged = true;
                        }
                        failed += 1;
                    }
                }
            } else {
                if !first_embed_error_logged {
                    log!("warn", "[backfill] embed() returned None (rowid={rowid})");
                    first_embed_error_logged = true;
                }
                failed += 1;
            }
            if (i + 1) % 500 == 0 {
                log!(
                    "info",
                    "backfill: {}/{total} (embedded={embedded} failed={failed})",
                    i + 1
                );
            }
        }

        log!(
            "info",
            "backfill done: total={total} embedded={embedded} failed={failed}"
        );
        Ok((total, embedded, failed))
    }

    /// Mark a drawer as embedded (called from add_drawer to keep shadow table in sync)
    fn mark_embedded(&self, rowid: i64) {
        let _ = self.conn.execute(
            "INSERT OR IGNORE INTO vec_embedded(rowid) VALUES (?1)",
            params![rowid],
        );
    }

    pub fn add_embedding(&self, rowid: i64, vec_bytes: &[u8]) -> Result<()> {
        // vec0 doesn't support INSERT OR REPLACE — delete first, then insert
        let _ = self
            .conn
            .execute("DELETE FROM vec_drawers WHERE rowid = ?1", params![rowid]);
        self.conn.execute(
            "INSERT INTO vec_drawers(rowid, embedding) VALUES (?1, ?2)",
            params![rowid, vec_bytes],
        )?;
        Ok(())
    }

    // ── search ────────────────────────────────────────────────────────────────

    pub fn search(
        &self,
        query: &str,
        limit: usize,
        wing: Option<&str>,
        room: Option<&str>,
        embedder: Option<&Embedder>,
    ) -> Result<Value> {
        // Hybrid: vector + FTS5 BM25 fused via Reciprocal Rank Fusion
        if let Some(emb) = embedder {
            return self.search_hybrid(query, limit, wing, room, emb);
        }
        // No embedder — pure FTS5 fallback
        self.fts_search(query, limit, wing, room)
    }

    #[allow(dead_code)]
    fn vector_search(
        &self,
        vec_bytes: &[u8],
        limit: usize,
        wing: Option<&str>,
        room: Option<&str>,
    ) -> Result<Value> {
        // sqlite-vec KNN query: fetch limit*4 then apply filters, cap at limit
        let fetch = limit * 4;
        let sql = match (wing, room) {
            (Some(_), Some(_)) => format!(
                "SELECT d.id, d.wing, d.room, d.content, v.distance
                 FROM vec_drawers v
                 JOIN drawers d ON v.rowid = d.rowid
                 WHERE v.embedding MATCH ?1 AND k = {fetch}
                 AND d.wing = ?2 AND d.room = ?3
                 ORDER BY v.distance"
            ),
            (Some(_), None) => format!(
                "SELECT d.id, d.wing, d.room, d.content, v.distance
                 FROM vec_drawers v
                 JOIN drawers d ON v.rowid = d.rowid
                 WHERE v.embedding MATCH ?1 AND k = {fetch}
                 AND d.wing = ?2
                 ORDER BY v.distance"
            ),
            (None, Some(_)) => format!(
                "SELECT d.id, d.wing, d.room, d.content, v.distance
                 FROM vec_drawers v
                 JOIN drawers d ON v.rowid = d.rowid
                 WHERE v.embedding MATCH ?1 AND k = {fetch}
                 AND d.room = ?2
                 ORDER BY v.distance"
            ),
            (None, None) => format!(
                "SELECT d.id, d.wing, d.room, d.content, v.distance
                 FROM vec_drawers v
                 JOIN drawers d ON v.rowid = d.rowid
                 WHERE v.embedding MATCH ?1 AND k = {fetch}
                 ORDER BY v.distance"
            ),
        };

        let mut stmt = self.conn.prepare(&sql)?;
        let mut results = Vec::new();

        let rows: Vec<(String, String, String, String, f64)> = match (wing, room) {
            (Some(w), Some(r)) => {
                let mut rows_raw = stmt.query(params![vec_bytes, w, r])?;
                let mut v = Vec::new();
                while let Some(row) = rows_raw.next()? {
                    v.push((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ));
                }
                v
            }
            (Some(w), None) => {
                let mut rows_raw = stmt.query(params![vec_bytes, w])?;
                let mut v = Vec::new();
                while let Some(row) = rows_raw.next()? {
                    v.push((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ));
                }
                v
            }
            (None, Some(r)) => {
                let mut rows_raw = stmt.query(params![vec_bytes, r])?;
                let mut v = Vec::new();
                while let Some(row) = rows_raw.next()? {
                    v.push((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ));
                }
                v
            }
            (None, None) => {
                let mut rows_raw = stmt.query(params![vec_bytes])?;
                let mut v = Vec::new();
                while let Some(row) = rows_raw.next()? {
                    v.push((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ));
                }
                v
            }
        };

        for (id, w, r, content, distance) in rows.into_iter().take(limit) {
            let similarity = 1.0 - (distance / 2.0);
            results.push(json!({
                "id": id,
                "wing": w,
                "room": r,
                "content": content,
                "rank": similarity,
            }));
        }

        Ok(Value::Array(results))
    }

    fn vector_search_raw(
        &self,
        vec_bytes: &[u8],
        fetch: usize,
        wing: Option<&str>,
        room: Option<&str>,
    ) -> Vec<(String, String, String, String, f64)> {
        let sql = match (wing, room) {
            (Some(_), Some(_)) => format!(
                "SELECT d.id, d.wing, d.room, d.content, v.distance
                 FROM vec_drawers v
                 JOIN drawers d ON v.rowid = d.rowid
                 WHERE v.embedding MATCH ?1 AND k = {fetch}
                 AND d.wing = ?2 AND d.room = ?3
                 ORDER BY v.distance"
            ),
            (Some(_), None) => format!(
                "SELECT d.id, d.wing, d.room, d.content, v.distance
                 FROM vec_drawers v
                 JOIN drawers d ON v.rowid = d.rowid
                 WHERE v.embedding MATCH ?1 AND k = {fetch}
                 AND d.wing = ?2
                 ORDER BY v.distance"
            ),
            (None, Some(_)) => format!(
                "SELECT d.id, d.wing, d.room, d.content, v.distance
                 FROM vec_drawers v
                 JOIN drawers d ON v.rowid = d.rowid
                 WHERE v.embedding MATCH ?1 AND k = {fetch}
                 AND d.room = ?2
                 ORDER BY v.distance"
            ),
            (None, None) => format!(
                "SELECT d.id, d.wing, d.room, d.content, v.distance
                 FROM vec_drawers v
                 JOIN drawers d ON v.rowid = d.rowid
                 WHERE v.embedding MATCH ?1 AND k = {fetch}
                 ORDER BY v.distance"
            ),
        };

        let mut stmt = match self.conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };

        match (wing, room) {
            (Some(w), Some(r)) => stmt
                .query_map(params![vec_bytes, w, r], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
                .unwrap_or_default(),
            (Some(w), None) => stmt
                .query_map(params![vec_bytes, w], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
                .unwrap_or_default(),
            (None, Some(r)) => stmt
                .query_map(params![vec_bytes, r], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
                .unwrap_or_default(),
            (None, None) => stmt
                .query_map(params![vec_bytes], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
                .unwrap_or_default(),
        }
    }

    fn fts_search(
        &self,
        query: &str,
        limit: usize,
        wing: Option<&str>,
        room: Option<&str>,
    ) -> Result<Value> {
        let safe_query = sanitize_fts_query(query);

        let sql = match (wing, room) {
            (Some(_), Some(_)) => format!(
                "SELECT d.id, d.wing, d.room, d.content, rank
                 FROM drawers_fts
                 JOIN drawers d ON drawers_fts.rowid = d.rowid
                 WHERE drawers_fts MATCH ?1 AND d.wing = ?2 AND d.room = ?3
                 ORDER BY rank LIMIT {limit}"
            ),
            (Some(_), None) => format!(
                "SELECT d.id, d.wing, d.room, d.content, rank
                 FROM drawers_fts
                 JOIN drawers d ON drawers_fts.rowid = d.rowid
                 WHERE drawers_fts MATCH ?1 AND d.wing = ?2
                 ORDER BY rank LIMIT {limit}"
            ),
            (None, Some(_)) => format!(
                "SELECT d.id, d.wing, d.room, d.content, rank
                 FROM drawers_fts
                 JOIN drawers d ON drawers_fts.rowid = d.rowid
                 WHERE drawers_fts MATCH ?1 AND d.room = ?2
                 ORDER BY rank LIMIT {limit}"
            ),
            (None, None) => format!(
                "SELECT d.id, d.wing, d.room, d.content, rank
                 FROM drawers_fts
                 JOIN drawers d ON drawers_fts.rowid = d.rowid
                 WHERE drawers_fts MATCH ?1
                 ORDER BY rank LIMIT {limit}"
            ),
        };

        let mut stmt = match self.conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return Ok(Value::Array(vec![])),
        };

        let mut results = Vec::new();

        let rows_result: rusqlite::Result<Vec<(String, String, String, String, f64)>> =
            match (wing, room) {
                (Some(w), Some(r)) => stmt
                    .query_map(params![safe_query, w, r], |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    })
                    .and_then(|iter| iter.collect()),
                (Some(w), None) => stmt
                    .query_map(params![safe_query, w], |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    })
                    .and_then(|iter| iter.collect()),
                (None, Some(r)) => stmt
                    .query_map(params![safe_query, r], |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    })
                    .and_then(|iter| iter.collect()),
                (None, None) => stmt
                    .query_map(params![safe_query], |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    })
                    .and_then(|iter| iter.collect()),
            };

        if let Ok(rows) = rows_result {
            for (id, w, r, content, rank) in rows {
                results.push(json!({
                    "id": id,
                    "wing": w,
                    "room": r,
                    "content": content,
                    "rank": rank,
                }));
            }
        }

        Ok(Value::Array(results))
    }

    fn fts_search_raw(
        &self,
        query: &str,
        fetch: usize,
        wing: Option<&str>,
        room: Option<&str>,
    ) -> Vec<(String, String, String, String)> {
        let safe_query = sanitize_fts_query(query);

        let sql = match (wing, room) {
            (Some(_), Some(_)) => format!(
                "SELECT d.id, d.wing, d.room, d.content
                 FROM drawers_fts
                 JOIN drawers d ON drawers_fts.rowid = d.rowid
                 WHERE drawers_fts MATCH ?1 AND d.wing = ?2 AND d.room = ?3
                 ORDER BY rank LIMIT {fetch}"
            ),
            (Some(_), None) => format!(
                "SELECT d.id, d.wing, d.room, d.content
                 FROM drawers_fts
                 JOIN drawers d ON drawers_fts.rowid = d.rowid
                 WHERE drawers_fts MATCH ?1 AND d.wing = ?2
                 ORDER BY rank LIMIT {fetch}"
            ),
            (None, Some(_)) => format!(
                "SELECT d.id, d.wing, d.room, d.content
                 FROM drawers_fts
                 JOIN drawers d ON drawers_fts.rowid = d.rowid
                 WHERE drawers_fts MATCH ?1 AND d.room = ?2
                 ORDER BY rank LIMIT {fetch}"
            ),
            (None, None) => format!(
                "SELECT d.id, d.wing, d.room, d.content
                 FROM drawers_fts
                 JOIN drawers d ON drawers_fts.rowid = d.rowid
                 WHERE drawers_fts MATCH ?1
                 ORDER BY rank LIMIT {fetch}"
            ),
        };

        let mut stmt = match self.conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };

        match (wing, room) {
            (Some(w), Some(r)) => stmt
                .query_map(params![safe_query, w, r], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
                .unwrap_or_default(),
            (Some(w), None) => stmt
                .query_map(params![safe_query, w], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
                .unwrap_or_default(),
            (None, Some(r)) => stmt
                .query_map(params![safe_query, r], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
                .unwrap_or_default(),
            (None, None) => stmt
                .query_map(params![safe_query], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
                .unwrap_or_default(),
        }
    }

    fn search_hybrid(
        &self,
        query: &str,
        limit: usize,
        wing: Option<&str>,
        room: Option<&str>,
        embedder: &Embedder,
    ) -> Result<Value> {
        use std::collections::HashMap;
        const K: f64 = 60.0; // standard RRF k parameter
        let fetch = limit * 8; // wide candidate pool for fusion

        // Vector candidates (empty vec if embedding fails or vec0 not loaded)
        let vec_hits = if let Some(vec_bytes) = embedder.embed(query) {
            self.vector_search_raw(&vec_bytes, fetch, wing, room)
        } else {
            vec![]
        };

        // FTS BM25 candidates
        let fts_hits = self.fts_search_raw(query, fetch, wing, room);

        if vec_hits.is_empty() && fts_hits.is_empty() {
            return Ok(Value::Array(vec![]));
        }

        // RRF: score(doc) = sum of 1/(K + rank_i + 1) across all lists
        let mut rrf_scores: HashMap<String, f64> = HashMap::new();
        let mut meta: HashMap<String, (String, String, String)> = HashMap::new();

        for (i, (id, w, r, c, _dist)) in vec_hits.iter().enumerate() {
            *rrf_scores.entry(id.clone()).or_insert(0.0) += 1.0 / (K + i as f64 + 1.0);
            meta.entry(id.clone())
                .or_insert_with(|| (w.clone(), r.clone(), c.clone()));
        }
        for (i, (id, w, r, c)) in fts_hits.iter().enumerate() {
            *rrf_scores.entry(id.clone()).or_insert(0.0) += 1.0 / (K + i as f64 + 1.0);
            meta.entry(id.clone())
                .or_insert_with(|| (w.clone(), r.clone(), c.clone()));
        }

        let mut ranked: Vec<(String, f64)> = rrf_scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let results: Vec<Value> = ranked
            .into_iter()
            .take(limit)
            .filter_map(|(id, score)| {
                let (w, r, c) = meta.get(&id)?;
                Some(json!({"id": id, "wing": w, "room": r, "content": c, "rank": score}))
            })
            .collect();

        Ok(Value::Array(results))
    }

    // ── check_duplicate ───────────────────────────────────────────────────────

    pub fn check_duplicate(
        &self,
        content: &str,
        threshold: f64,
        embedder: Option<&Embedder>,
    ) -> Result<Value> {
        // Try vector-based check first
        if let Some(emb) = embedder {
            if let Some(vec_bytes) = emb.embed(content) {
                let sql = "SELECT d.id, d.wing, d.room, d.content, v.distance
                           FROM vec_drawers v
                           JOIN drawers d ON v.rowid = d.rowid
                           WHERE v.embedding MATCH ?1 AND k = 5
                           ORDER BY v.distance";
                if let Ok(mut stmt) = self.conn.prepare(sql) {
                    let mut matches = Vec::new();
                    let mut is_dup = false;
                    let rows: Vec<(String, String, String, String, f64)> = stmt
                        .query_map(params![vec_bytes], |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                            ))
                        })
                        .map(|iter| iter.filter_map(|r| r.ok()).collect())
                        .unwrap_or_default();

                    for (id, w, r, c, distance) in rows {
                        // cosine sim from L2 on unit vectors: 1 - d²/2
                        let similarity = 1.0 - (distance * distance / 2.0);
                        if similarity >= threshold {
                            is_dup = true;
                            let truncated: String = c.chars().take(200).collect();
                            matches.push(json!({
                                "id": id,
                                "wing": w,
                                "room": r,
                                "content": truncated,
                                "similarity": similarity,
                            }));
                        }
                    }
                    return Ok(json!({
                        "is_duplicate": is_dup,
                        "matches": matches,
                    }));
                }
            }
        }

        // Fallback: exact content match
        let row: Option<(String, String, String, String)> = self
            .conn
            .query_row(
                "SELECT id, wing, room, content FROM drawers WHERE content = ?1 LIMIT 1",
                params![content],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;

        let (is_dup, matches) = if let Some((id, w, r, c)) = row {
            let truncated: String = c.chars().take(200).collect();
            (
                true,
                vec![json!({
                    "id": id,
                    "wing": w,
                    "room": r,
                    "content": truncated,
                    "similarity": 1.0,
                })],
            )
        } else {
            (false, vec![])
        };

        Ok(json!({
            "is_duplicate": is_dup,
            "matches": matches,
        }))
    }

    // ── upsert_drawer (for importers) ─────────────────────────────────────────

    /// Insert or replace a drawer with a caller-supplied stable ID.
    /// Used by importers (e.g. index-sessions) that want a stable key independent
    /// of content, so re-indexing updated content doesn't create duplicate drawers.
    pub fn upsert_drawer(
        &self,
        id: &str,
        wing: &str,
        room: &str,
        content: &str,
        source_file: Option<&str>,
        added_by: &str,
        embedder: Option<&Embedder>,
    ) -> Result<()> {
        // Get old rowid if exists (to clean up vec_drawers before replace)
        let old_rowid: Option<i64> = self
            .conn
            .query_row(
                "SELECT rowid FROM drawers WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;

        if let Some(old) = old_rowid {
            let _ = self
                .conn
                .execute("DELETE FROM vec_drawers WHERE rowid = ?1", params![old]);
            let _ = self
                .conn
                .execute("DELETE FROM vec_embedded WHERE rowid = ?1", params![old]);
        }

        self.conn.execute(
            "INSERT OR REPLACE INTO drawers (id, wing, room, content, source_file, added_by, filed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))",
            params![id, wing, room, content, source_file, added_by],
        )?;

        let rowid = self.conn.last_insert_rowid();
        if let Some(emb) = embedder {
            if let Some(vec_bytes) = emb.embed(content) {
                if self.add_embedding(rowid, &vec_bytes).is_ok() {
                    self.mark_embedded(rowid);
                }
            }
        }

        Ok(())
    }

    // ── add_drawer ────────────────────────────────────────────────────────────

    pub fn add_drawer(
        &self,
        wing: &str,
        room: &str,
        content: &str,
        source_file: Option<&str>,
        added_by: &str,
        embedder: Option<&Embedder>,
    ) -> Result<String> {
        // Generate deterministic ID: MD5(content + wing + room)
        let mut ctx = md5::Context::new();
        ctx.consume(content.as_bytes());
        ctx.consume(wing.as_bytes());
        ctx.consume(room.as_bytes());
        let hash = ctx.compute();
        let hex = format!("{:x}", hash);
        let drawer_id = format!("drawer_{}_{}_{}", wing, room, &hex[..16]);

        self.conn.execute(
            "INSERT OR IGNORE INTO drawers (id, wing, room, content, source_file, added_by, filed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))",
            params![drawer_id, wing, room, content, source_file, added_by],
        )?;

        let changes = self.conn.changes();
        if changes > 0 {
            let rowid = self.conn.last_insert_rowid();
            if let Some(emb) = embedder {
                if let Some(vec_bytes) = emb.embed(content) {
                    if self.add_embedding(rowid, &vec_bytes).is_ok() {
                        self.mark_embedded(rowid);
                    }
                }
            }
        }

        Ok(drawer_id)
    }

    // ── delete_drawer ─────────────────────────────────────────────────────────

    // ── update_drawer ─────────────────────────────────────────────────────────

    /// Update content (and optionally wing/room) of an existing drawer.
    /// Re-indexes FTS5 and re-embeds automatically.
    pub fn update_drawer(
        &self,
        drawer_id: &str,
        new_content: &str,
        new_wing: Option<&str>,
        new_room: Option<&str>,
        embedder: Option<&Embedder>,
    ) -> Result<()> {
        // Get current values + rowid
        let (old_content, cur_wing, cur_room, rowid): (String, String, String, i64) = self
            .conn
            .query_row(
                "SELECT content, wing, room, rowid FROM drawers WHERE id = ?1",
                params![drawer_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(|_| anyhow!("DrawerNotFound: {drawer_id}"))?;

        let wing = new_wing.unwrap_or(&cur_wing);
        let room = new_room.unwrap_or(&cur_room);

        // Update the drawers table
        self.conn.execute(
            "UPDATE drawers SET content = ?1, wing = ?2, room = ?3 WHERE id = ?4",
            params![new_content, wing, room, drawer_id],
        )?;

        // Sync FTS5 (triggers only fire on INSERT/DELETE, not UPDATE)
        // Delete old entry then insert new
        self.conn.execute(
            "INSERT INTO drawers_fts(drawers_fts, rowid, content, wing, room)
             VALUES ('delete', ?1, ?2, ?3, ?4)",
            params![rowid, old_content, cur_wing, cur_room],
        )?;
        self.conn.execute(
            "INSERT INTO drawers_fts(rowid, content, wing, room) VALUES (?1, ?2, ?3, ?4)",
            params![rowid, new_content, wing, room],
        )?;

        // Re-embed: remove old embedding, insert new
        let _ = self
            .conn
            .execute("DELETE FROM vec_drawers WHERE rowid = ?1", params![rowid]);
        let _ = self
            .conn
            .execute("DELETE FROM vec_embedded WHERE rowid = ?1", params![rowid]);

        if let Some(emb) = embedder {
            if let Some(vec_bytes) = emb.embed(new_content) {
                if self.add_embedding(rowid, &vec_bytes).is_ok() {
                    self.mark_embedded(rowid);
                }
            }
        }

        Ok(())
    }

    /// Bulk find-and-replace across all drawer content.
    /// Returns the number of drawers updated.
    /// Re-indexes FTS5 via full rebuild and clears stale embeddings.
    pub fn bulk_replace(
        &self,
        find: &str,
        replace: &str,
        wing: Option<&str>,
        embedder: Option<&Embedder>,
    ) -> Result<usize> {
        // Collect affected rows before updating
        let sql = match wing {
            Some(_) => {
                "SELECT id, rowid, content, wing, room FROM drawers \
                         WHERE content LIKE '%' || ?1 || '%' AND wing = ?2"
            }
            None => {
                "SELECT id, rowid, content, wing, room FROM drawers \
                     WHERE content LIKE '%' || ?1 || '%'"
            }
        };

        let mut stmt = self.conn.prepare(sql)?;
        let rows: Vec<(String, i64, String, String, String)> = match wing {
            Some(w) => stmt
                .query_map(params![find, w], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })
                .map(|iter| iter.filter_map(|r| r.ok()).collect())?,
            None => stmt
                .query_map(params![find], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })
                .map(|iter| iter.filter_map(|r| r.ok()).collect())?,
        };

        if rows.is_empty() {
            return Ok(0);
        }

        let count = rows.len();

        for (id, rowid, old_content, w, r) in &rows {
            let new_content = old_content.replace(find, replace);

            self.conn.execute(
                "UPDATE drawers SET content = ?1 WHERE id = ?2",
                params![new_content, id],
            )?;

            // FTS5: delete old, insert new
            let _ = self.conn.execute(
                "INSERT INTO drawers_fts(drawers_fts, rowid, content, wing, room)
                 VALUES ('delete', ?1, ?2, ?3, ?4)",
                params![rowid, old_content, w, r],
            );
            let _ = self.conn.execute(
                "INSERT INTO drawers_fts(rowid, content, wing, room) VALUES (?1, ?2, ?3, ?4)",
                params![rowid, new_content, w, r],
            );

            // Clear stale embeddings (will re-embed below)
            let _ = self
                .conn
                .execute("DELETE FROM vec_drawers WHERE rowid = ?1", params![rowid]);
            let _ = self
                .conn
                .execute("DELETE FROM vec_embedded WHERE rowid = ?1", params![rowid]);

            // Re-embed immediately if embedder available
            if let Some(emb) = embedder {
                if let Some(vec_bytes) = emb.embed(&new_content) {
                    if self.add_embedding(*rowid, &vec_bytes).is_ok() {
                        self.mark_embedded(*rowid);
                    }
                }
            }
        }

        Ok(count)
    }

    pub fn delete_drawer(&self, drawer_id: &str) -> Result<()> {
        // Get rowid first
        let rowid: Option<i64> = self
            .conn
            .query_row(
                "SELECT rowid FROM drawers WHERE id = ?1",
                params![drawer_id],
                |r| r.get(0),
            )
            .optional()?;

        let rowid = rowid.ok_or_else(|| anyhow!("DrawerNotFound"))?;

        self.conn
            .execute("DELETE FROM drawers WHERE id = ?1", params![drawer_id])?;

        // Also delete embedding and shadow table entry
        let _ = self
            .conn
            .execute("DELETE FROM vec_drawers WHERE rowid = ?1", params![rowid]);
        let _ = self
            .conn
            .execute("DELETE FROM vec_embedded WHERE rowid = ?1", params![rowid]);

        Ok(())
    }

    // ── diary entries ─────────────────────────────────────────────────────────

    pub fn get_diary_entries(&self, wing: &str, limit: usize) -> Result<Value> {
        let mut stmt = self.conn.prepare(
            "SELECT content, filed_at FROM drawers
             WHERE wing = ?1 AND room = 'diary'
             ORDER BY filed_at DESC LIMIT ?2",
        )?;

        let rows: Vec<(String, String)> = stmt
            .query_map(params![wing, limit as i64], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();

        let mut entries = Vec::new();
        for (content, ts) in &rows {
            let date = if ts.len() >= 10 {
                &ts[..10]
            } else {
                ts.as_str()
            };
            // Parse [topic] prefix
            let (topic, body) = if content.starts_with('[') {
                if let Some(close) = content[1..].find("] ") {
                    let t = &content[1..1 + close];
                    let b = &content[1 + close + 2..];
                    (t.to_string(), b.to_string())
                } else {
                    ("general".to_string(), content.clone())
                }
            } else {
                ("general".to_string(), content.clone())
            };

            entries.push(json!({
                "date": date,
                "timestamp": ts,
                "topic": topic,
                "content": body,
            }));
        }

        // Total count
        let total: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM drawers WHERE wing = ?1 AND room = 'diary'",
                params![wing],
                |r| r.get(0),
            )
            .unwrap_or(entries.len() as i64);

        Ok(json!({
            "entries": entries,
            "total": total,
            "showing": entries.len() as i64,
        }))
    }
}

// ── FTS5 query sanitization ───────────────────────────────────────────────────

fn sanitize_fts_query(query: &str) -> String {
    // If the query already contains FTS5 syntax, pass through unchanged
    if query.contains('"')
        || query.contains('*')
        || query.contains('(')
        || query.contains(')')
        || query.contains('+')
        || query.contains(" AND ")
        || query.contains(" OR ")
        || query.contains(" NOT ")
    {
        return query.to_string();
    }

    // Split on whitespace and join with OR
    let tokens: Vec<&str> = query.split_whitespace().collect();
    if tokens.len() <= 1 {
        query.to_string()
    } else {
        tokens.join(" OR ")
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Helper: create a fresh test palace in a temp directory
    fn test_db() -> (TempDir, Database) {
        let dir = TempDir::new().unwrap();
        let db = Database::open(dir.path().to_str().unwrap()).unwrap();
        (dir, db)
    }

    // ── Database open & schema ──────────────────────────────────────────────────

    #[test]
    fn test_open_creates_tables() {
        let (_dir, db) = test_db();
        // drawers, drawers_fts, triples must always exist.
        // vec_drawers only exists if sqlite-vec extension is loaded.
        let required: &[&str] = &["drawers", "drawers_fts", "triples", "vec_embedded"];
        for name in required {
            let exists: bool = db.conn.query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type IN ('table','view') AND name = ?1",
                params![name],
                |r| r.get(0),
            ).unwrap();
            assert!(exists, "expected table {name} to exist");
        }
    }

    #[test]
    fn test_tables_have_expected_columns() {
        let (_dir, db) = test_db();
        let cols: Vec<String> = db
            .conn
            .prepare("PRAGMA table_info(drawers)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(cols.contains(&"id".to_string()));
        assert!(cols.contains(&"wing".to_string()));
        assert!(cols.contains(&"room".to_string()));
        assert!(cols.contains(&"content".to_string()));
        assert!(cols.contains(&"source_file".to_string()));
        assert!(cols.contains(&"added_by".to_string()));
        assert!(cols.contains(&"filed_at".to_string()));
    }

    // ── add_drawer ──────────────────────────────────────────────────────────────

    #[test]
    fn test_add_drawer_basic() {
        let (_dir, db) = test_db();
        let id = db
            .add_drawer("proj", "notes", "hello world", None, "test", None)
            .unwrap();
        assert!(id.starts_with("drawer_"));
        // Verify in drawers table
        let content: String = db
            .conn
            .query_row(
                "SELECT content FROM drawers WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    fn test_add_drawer_sets_wing_and_room() {
        let (_dir, db) = test_db();
        let id = db
            .add_drawer("mywing", "myroom", "content", None, "test", None)
            .unwrap();
        let (wing, room): (String, String) = db
            .conn
            .query_row(
                "SELECT wing, room FROM drawers WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(wing, "mywing");
        assert_eq!(room, "myroom");
    }

    #[test]
    fn test_add_drawer_idempotent_same_content() {
        let (_dir, db) = test_db();
        let id1 = db
            .add_drawer("w", "r", "same content", None, "test", None)
            .unwrap();
        let id2 = db
            .add_drawer("w", "r", "same content", None, "test", None)
            .unwrap();
        assert_eq!(id1, id2);
        let count = db.get_drawer_count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_add_drawer_different_content_different_id() {
        let (_dir, db) = test_db();
        let id1 = db
            .add_drawer("w", "r", "content A", None, "test", None)
            .unwrap();
        let id2 = db
            .add_drawer("w", "r", "content B", None, "test", None)
            .unwrap();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_add_drawer_different_wing_different_id() {
        let (_dir, db) = test_db();
        let id1 = db
            .add_drawer("wing1", "room", "content", None, "test", None)
            .unwrap();
        let id2 = db
            .add_drawer("wing2", "room", "content", None, "test", None)
            .unwrap();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_add_drawer_with_source_file() {
        let (_dir, db) = test_db();
        let id = db
            .add_drawer("w", "r", "content", Some("/path/to/file.rs"), "test", None)
            .unwrap();
        let sf: String = db
            .conn
            .query_row(
                "SELECT source_file FROM drawers WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sf, "/path/to/file.rs");
    }

    #[test]
    fn test_add_drawer_filed_at_is_set() {
        let (_dir, db) = test_db();
        let id = db
            .add_drawer("w", "r", "content", None, "test", None)
            .unwrap();
        let filed_at: String = db
            .conn
            .query_row(
                "SELECT filed_at FROM drawers WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!filed_at.is_empty());
        // Should be a datetime string (YYYY-MM-DD HH:MM:SS)
        assert!(filed_at.contains('-'));
    }

    #[test]
    fn test_add_drawer_fts_indexed() {
        let (_dir, db) = test_db();
        let id = db
            .add_drawer("w", "r", "hello unique zigzag", None, "test", None)
            .unwrap();
        // Look up the rowid
        let rowid: i64 = db
            .conn
            .query_row(
                "SELECT rowid FROM drawers WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        // Verify FTS index has this rowid
        let fts_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM drawers_fts WHERE rowid = ?1",
                params![rowid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts_count, 1);
    }

    // ── get_drawer_count ────────────────────────────────────────────────────────

    #[test]
    fn test_get_drawer_count_zero() {
        let (_dir, db) = test_db();
        assert_eq!(db.get_drawer_count(), 0);
    }

    #[test]
    fn test_get_drawer_count_after_inserts() {
        let (_dir, db) = test_db();
        db.add_drawer("w", "r1", "c1", None, "test", None).unwrap();
        db.add_drawer("w", "r2", "c2", None, "test", None).unwrap();
        db.add_drawer("w", "r3", "c3", None, "test", None).unwrap();
        assert_eq!(db.get_drawer_count(), 3);
    }

    // ── delete_drawer ───────────────────────────────────────────────────────────

    #[test]
    fn test_delete_drawer() {
        let (_dir, db) = test_db();
        let id = db
            .add_drawer("w", "r", "content", None, "test", None)
            .unwrap();
        assert_eq!(db.get_drawer_count(), 1);
        db.delete_drawer(&id).unwrap();
        assert_eq!(db.get_drawer_count(), 0);
    }

    #[test]
    fn test_delete_drawer_removes_fts() {
        let (_dir, db) = test_db();
        let id = db
            .add_drawer("w", "r", "unique text", None, "test", None)
            .unwrap();
        let rowid: i64 = db
            .conn
            .query_row(
                "SELECT rowid FROM drawers WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        db.delete_drawer(&id).unwrap();
        let fts_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM drawers_fts WHERE rowid = ?1",
                params![rowid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts_count, 0);
    }

    #[test]
    fn test_delete_nonexistent_drawer() {
        let (_dir, db) = test_db();
        let result = db.delete_drawer("nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("DrawerNotFound"));
    }

    // ── upsert_drawer ───────────────────────────────────────────────────────────

    #[test]
    fn test_upsert_drawer_insert() {
        let (_dir, db) = test_db();
        db.upsert_drawer("custom_id_1", "w", "r", "hello upsert", None, "test", None)
            .unwrap();
        let count = db.get_drawer_count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_upsert_drawer_replace() {
        let (_dir, db) = test_db();
        db.upsert_drawer(
            "custom_id_2",
            "wing_a",
            "room_a",
            "initial",
            None,
            "test",
            None,
        )
        .unwrap();
        db.upsert_drawer(
            "custom_id_2",
            "wing_b",
            "room_b",
            "updated",
            None,
            "test",
            None,
        )
        .unwrap();
        // Count should still be 1 (replaced, not added)
        assert_eq!(db.get_drawer_count(), 1);
        let (wing, room, content): (String, String, String) = db
            .conn
            .query_row(
                "SELECT wing, room, content FROM drawers WHERE id = 'custom_id_2'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(wing, "wing_b");
        assert_eq!(room, "room_b");
        assert_eq!(content, "updated");
    }

    // ── FTS search ──────────────────────────────────────────────────────────────

    #[test]
    fn test_fts_search_basic() {
        let (_dir, db) = test_db();
        db.add_drawer("w", "r", "the quick brown fox", None, "test", None)
            .unwrap();
        db.add_drawer("w", "r", "some unrelated stuff", None, "test", None)
            .unwrap();
        let results = db.search("quick fox", 5, None, None, None).unwrap();
        let arr = results.as_array().unwrap();
        assert!(!arr.is_empty());
        let first = &arr[0];
        assert!(first["content"].as_str().unwrap().contains("quick"));
    }

    #[test]
    fn test_fts_search_no_match() {
        let (_dir, db) = test_db();
        db.add_drawer("w", "r", "hello world", None, "test", None)
            .unwrap();
        let results = db
            .search("zzzznotpresentzzzz", 5, None, None, None)
            .unwrap();
        let arr = results.as_array().unwrap();
        assert!(arr.is_empty());
    }

    #[test]
    fn test_fts_search_wing_filter() {
        let (_dir, db) = test_db();
        db.add_drawer("alpha", "r", "shared keyword", None, "test", None)
            .unwrap();
        db.add_drawer("beta", "r", "shared keyword", None, "test", None)
            .unwrap();
        let results = db.search("shared", 10, Some("alpha"), None, None).unwrap();
        let arr = results.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["wing"].as_str().unwrap(), "alpha");
    }

    #[test]
    fn test_fts_search_room_filter() {
        let (_dir, db) = test_db();
        db.add_drawer("w", "kitchen", "recipe ingredients", None, "test", None)
            .unwrap();
        db.add_drawer("w", "garden", "planting tips", None, "test", None)
            .unwrap();
        let results = db
            .search("recipe", 10, None, Some("kitchen"), None)
            .unwrap();
        let arr = results.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["room"].as_str().unwrap(), "kitchen");
    }

    #[test]
    fn test_fts_search_limit() {
        let (_dir, db) = test_db();
        for i in 0..10 {
            db.add_drawer(
                "w",
                &format!("r{i}"),
                &format!("common term {i}"),
                None,
                "test",
                None,
            )
            .unwrap();
        }
        let results = db.search("common", 3, None, None, None).unwrap();
        let arr = results.as_array().unwrap();
        assert_eq!(arr.len(), 3);
    }

    // ── fts_search sanitize ─────────────────────────────────────────────────────

    #[test]
    fn test_sanitize_fts_query_multi_word() {
        assert_eq!(
            sanitize_fts_query("hello world"),
            "hello OR world".to_string()
        );
    }

    #[test]
    fn test_sanitize_fts_query_single_word() {
        assert_eq!(sanitize_fts_query("hello"), "hello".to_string());
    }

    #[test]
    fn test_sanitize_fts_query_three_words() {
        assert_eq!(sanitize_fts_query("a b c"), "a OR b OR c".to_string());
    }

    #[test]
    fn test_sanitize_fts_query_empty() {
        assert_eq!(sanitize_fts_query(""), "".to_string());
    }

    #[test]
    fn test_sanitize_fts_query_already_has_quotes() {
        let q = r#""exact phrase""#;
        assert_eq!(sanitize_fts_query(q), q.to_string());
    }

    #[test]
    fn test_sanitize_fts_query_already_has_wildcard() {
        let q = "prefix*";
        assert_eq!(sanitize_fts_query(q), q.to_string());
    }

    #[test]
    fn test_sanitize_fts_query_already_has_and() {
        let q = "foo AND bar";
        assert_eq!(sanitize_fts_query(q), q.to_string());
    }

    #[test]
    fn test_sanitize_fts_query_already_has_or() {
        let q = "foo OR bar";
        assert_eq!(sanitize_fts_query(q), q.to_string());
    }

    #[test]
    fn test_sanitize_fts_query_already_has_parentheses() {
        let q = "(hello world)";
        assert_eq!(sanitize_fts_query(q), q.to_string());
    }

    #[test]
    fn test_sanitize_fts_query_whitespace_only() {
        // Whitespace-only input: split_whitespace yields empty vec, len=0 <= 1,
        // so the original query is returned as-is.
        assert_eq!(sanitize_fts_query("   "), "   ".to_string());
    }

    // ── bulk_replace ────────────────────────────────────────────────────────────

    #[test]
    fn test_bulk_replace_basic() {
        let (_dir, db) = test_db();
        db.add_drawer("w", "r1", "hello old world", None, "test", None)
            .unwrap();
        db.add_drawer("w", "r2", "goodbye old friend", None, "test", None)
            .unwrap();
        db.add_drawer("w", "r3", "nothing to change", None, "test", None)
            .unwrap();
        let updated = db.bulk_replace("old", "new", None, None).unwrap();
        assert_eq!(updated, 2);
        // Verify content changed
        let c1: String = db
            .conn
            .query_row(
                "SELECT content FROM drawers WHERE wing='w' AND room='r1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(c1, "hello new world");
    }

    #[test]
    fn test_bulk_replace_no_match() {
        let (_dir, db) = test_db();
        db.add_drawer("w", "r", "nothing here", None, "test", None)
            .unwrap();
        let updated = db.bulk_replace("zzz", "yyy", None, None).unwrap();
        assert_eq!(updated, 0);
    }

    #[test]
    fn test_bulk_replace_wing_filter() {
        let (_dir, db) = test_db();
        db.add_drawer("wing1", "r", "replace me", None, "test", None)
            .unwrap();
        db.add_drawer("wing2", "r", "replace me", None, "test", None)
            .unwrap();
        let updated = db
            .bulk_replace("replace", "done", Some("wing1"), None)
            .unwrap();
        assert_eq!(updated, 1);
    }

    // ── wing / room / taxonomy ──────────────────────────────────────────────────

    #[test]
    fn test_wing_counts() {
        let (_dir, db) = test_db();
        db.add_drawer("foo", "a", "c", None, "test", None).unwrap();
        db.add_drawer("foo", "b", "c", None, "test", None).unwrap();
        db.add_drawer("bar", "x", "c", None, "test", None).unwrap();
        let counts = db.get_wing_counts().unwrap();
        let map = counts.as_object().unwrap();
        assert_eq!(map["foo"].as_i64().unwrap(), 2);
        assert_eq!(map["bar"].as_i64().unwrap(), 1);
    }

    #[test]
    fn test_room_counts() {
        let (_dir, db) = test_db();
        db.add_drawer("w", "bedroom", "c", None, "test", None)
            .unwrap();
        db.add_drawer("w", "kitchen", "c", None, "test", None)
            .unwrap();
        db.add_drawer("w", "bedroom", "c2", None, "test", None)
            .unwrap();
        let counts = db.get_room_counts(Some("w")).unwrap();
        let map = counts.as_object().unwrap();
        assert_eq!(map["bedroom"].as_i64().unwrap(), 2);
        assert_eq!(map["kitchen"].as_i64().unwrap(), 1);
    }

    #[test]
    fn test_taxonomy() {
        let (_dir, db) = test_db();
        db.add_drawer("w1", "r1", "c", None, "test", None).unwrap();
        db.add_drawer("w1", "r2", "c", None, "test", None).unwrap();
        db.add_drawer("w2", "r1", "c", None, "test", None).unwrap();
        let tax = db.get_taxonomy().unwrap();
        let map = tax.as_object().unwrap();
        assert!(map.contains_key("w1"));
        assert!(map.contains_key("w2"));
    }

    // ── diary ───────────────────────────────────────────────────────────────────

    #[test]
    fn test_get_diary_entries_empty() {
        let (_dir, db) = test_db();
        let data = db.get_diary_entries("nonexistent", 10).unwrap();
        let entries = data["entries"].as_array().unwrap();
        assert!(entries.is_empty());
        assert_eq!(data["total"].as_i64().unwrap(), 0);
    }

    #[test]
    fn test_get_diary_entries_with_data() {
        let (_dir, db) = test_db();
        // diary entries are filed as wing="wing_X", room="diary"
        db.add_drawer(
            "wing_testagent",
            "diary",
            "[general] logged something",
            None,
            "testagent",
            None,
        )
        .unwrap();
        db.add_drawer(
            "wing_testagent",
            "diary",
            "[code] wrote a function",
            None,
            "testagent",
            None,
        )
        .unwrap();
        let data = db.get_diary_entries("wing_testagent", 10).unwrap();
        let entries = data["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(data["showing"].as_i64().unwrap(), 2);
    }

    #[test]
    fn test_get_diary_topic_parsing() {
        let (_dir, db) = test_db();
        db.add_drawer(
            "wing_testagent",
            "diary",
            "[debug] fixed a crash",
            None,
            "testagent",
            None,
        )
        .unwrap();
        let data = db.get_diary_entries("wing_testagent", 1).unwrap();
        let entries = data["entries"].as_array().unwrap();
        let entry = &entries[0];
        assert_eq!(entry["topic"].as_str().unwrap(), "debug");
        assert_eq!(entry["content"].as_str().unwrap(), "fixed a crash");
    }

    // ── update_drawer ───────────────────────────────────────────────────────────

    #[test]
    fn test_update_drawer_content() {
        let (_dir, db) = test_db();
        let id = db
            .add_drawer("w", "r", "original", None, "test", None)
            .unwrap();
        db.update_drawer(&id, "modified", None, None, None).unwrap();
        let content: String = db
            .conn
            .query_row(
                "SELECT content FROM drawers WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(content, "modified");
    }

    #[test]
    fn test_update_drawer_wing_and_room() {
        let (_dir, db) = test_db();
        let id = db
            .add_drawer("w", "r", "content", None, "test", None)
            .unwrap();
        db.update_drawer(&id, "content", Some("new_wing"), Some("new_room"), None)
            .unwrap();
        let (wing, room): (String, String) = db
            .conn
            .query_row(
                "SELECT wing, room FROM drawers WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(wing, "new_wing");
        assert_eq!(room, "new_room");
    }

    #[test]
    fn test_update_drawer_nonexistent() {
        let (_dir, db) = test_db();
        let result = db.update_drawer("nonexistent", "content", None, None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("DrawerNotFound"));
    }

    #[test]
    fn test_update_drawer_re_indexes_fts() {
        let (_dir, db) = test_db();
        let id = db
            .add_drawer("w", "r", "old text here", None, "test", None)
            .unwrap();
        let _rowid: i64 = db
            .conn
            .query_row(
                "SELECT rowid FROM drawers WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        db.update_drawer(&id, "new refreshed content", None, None, None)
            .unwrap();
        // Search for new text via FTS
        let results = db.search("refreshed", 5, None, None, None).unwrap();
        let arr = results.as_array().unwrap();
        assert!(!arr.is_empty());
        assert!(arr[0]["content"].as_str().unwrap().contains("refreshed"));
        // Old text should not match
        let old_results = db.search("old text", 5, None, None, None).unwrap();
        assert!(old_results.as_array().unwrap().is_empty());
    }
}
