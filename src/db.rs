use anyhow::{anyhow, Result};
use rusqlite::{params, types::Value as SqlValue, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::Path;

use crate::embed::Embedder;
use crate::log::log;

pub struct Database {
    pub conn: Connection,
    pub vector_disabled: bool,
}

impl Database {
    pub fn open(dir: &str) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let db_path = Path::new(dir).join("palace.db");
        let conn = Connection::open(&db_path)?;

        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=OFF;",
        )?;

        let mut db = Self {
            conn,
            vector_disabled: false,
        };
        db.create_tables()?;
        db.probe_vec0_health();
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

        // Track import state for incremental syncs
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sync_state (
                source TEXT PRIMARY KEY,
                last_time_updated INTEGER NOT NULL
            );",
        )?;

        Ok(())
    }

    // ── drawer count ──────────────────────────────────────────────────────────

    pub fn get_drawer_count(&self) -> i64 {
        self.conn
            .query_row("SELECT COUNT(*) FROM drawers", [], |r| r.get(0))
            .unwrap_or(0)
    }

    // ── sync state ────────────────────────────────────────────────────────────

    /// Get the last imported timestamp for a sync source.
    /// Returns 0 if never imported.
    pub fn get_sync_state(&self, source: &str) -> i64 {
        self.conn
            .query_row(
                "SELECT last_time_updated FROM sync_state WHERE source = ?1",
                params![source],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }

    /// Record the last imported timestamp for a sync source.
    pub fn set_sync_state(&self, source: &str, last_time_updated: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO sync_state (source, last_time_updated) VALUES (?1, ?2)",
            params![source, last_time_updated],
        )?;
        Ok(())
    }

    // ── vec0 health probe ──────────────────────────────────────────────────────

    /// Probe vector index health. Compares drawer count against embedded count.
    /// If divergence exceeds 5%, sets vector_disabled and logs a warning.
    fn probe_vec0_health(&mut self) {
        let drawer_count = self.get_drawer_count();
        if drawer_count == 0 {
            return;
        }

        let embedded_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM vec_embedded", [], |r| r.get(0))
            .unwrap_or(0);

        if embedded_count == 0 {
            self.vector_disabled = true;
            log!(
                "warn",
                "vec0 health: 0/{drawer_count} drawers embedded — vector search disabled"
            );
            return;
        }

        let gap = if drawer_count > embedded_count {
            drawer_count - embedded_count
        } else {
            0
        };
        let pct = if drawer_count > 0 {
            gap as f64 / drawer_count as f64 * 100.0
        } else {
            0.0
        };

        if pct > 5.0 {
            self.vector_disabled = true;
            log!(
                "warn",
                "vec0 health: {embedded_count}/{drawer_count} embedded ({pct:.1}% gap) — vector search disabled"
            );
        } else {
            self.vector_disabled = false;
        }
    }

    /// Get the health status of the vector index.
    pub fn vec0_health(&self) -> Value {
        let drawer_count = self.get_drawer_count();
        let embedded_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM vec_embedded", [], |r| r.get(0))
            .unwrap_or(0);
        let gap = if drawer_count > embedded_count {
            drawer_count - embedded_count
        } else {
            0
        };
        let pct = if drawer_count > 0 {
            gap as f64 / drawer_count as f64 * 100.0
        } else {
            0.0
        };
        json!({
            "drawer_count": drawer_count,
            "embedded_count": embedded_count,
            "gap": gap,
            "gap_pct": pct,
            "vector_disabled": self.vector_disabled,
        })
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
        offset: usize,
        wing: Option<&str>,
        room: Option<&str>,
        filed_after: Option<&str>,
        filed_before: Option<&str>,
        embedder: Option<&Embedder>,
        sort_by: &str,
    ) -> Result<Value> {
        let limit = limit.clamp(1, 1000);
        if self.vector_disabled {
            return self.fts_search(query, limit, offset, wing, room, filed_after, filed_before);
        }
        if sort_by == "recency" {
            return self.search_recent(query, limit, offset, wing, room, filed_after, filed_before);
        }
        let use_recency = sort_by == "hybrid";
        if let Some(emb) = embedder {
            return self.search_hybrid(
                query, limit, offset, wing, room, filed_after, filed_before, emb, use_recency,
            );
        }
        self.fts_search(query, limit, offset, wing, room, filed_after, filed_before)
    }

    // ── filter clause builder (shared by all search functions) ─────────────────

    /// Builds `AND d.wing = ?N AND d.room = ?M` clauses and collects param values.
    /// Returns (sql_fragment, filter_params) where sql_fragment is empty when
    /// both wing and room are None.
    fn build_filter_clause(
        wing: Option<&str>,
        room: Option<&str>,
        idx_start: usize,
    ) -> (String, Vec<SqlValue>) {
        let mut sql = String::new();
        let mut params = Vec::new();
        let mut idx = idx_start;
        if let Some(w) = wing {
            sql.push_str(&format!(" AND d.wing = ?{idx}"));
            params.push(SqlValue::Text(w.to_string()));
            idx += 1;
        }
        if let Some(r) = room {
            sql.push_str(&format!(" AND d.room = ?{idx}"));
            params.push(SqlValue::Text(r.to_string()));
        }
        (sql, params)
    }

    /// Build date-range filter clauses. Returns (sql_fragment, params).
    fn build_date_clause(
        filed_after: Option<&str>,
        filed_before: Option<&str>,
        idx_start: usize,
    ) -> (String, Vec<SqlValue>) {
        let mut sql = String::new();
        let mut params = Vec::new();
        let mut idx = idx_start;
        if let Some(after) = filed_after {
            sql.push_str(&format!(" AND d.filed_at >= ?{idx}"));
            params.push(SqlValue::Text(after.to_string()));
            idx += 1;
        }
        if let Some(before) = filed_before {
            sql.push_str(&format!(" AND d.filed_at <= ?{idx}"));
            params.push(SqlValue::Text(before.to_string()));
        }
        (sql, params)
    }

    fn fts_search(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
        wing: Option<&str>,
        room: Option<&str>,
        filed_after: Option<&str>,
        filed_before: Option<&str>,
    ) -> Result<Value> {
        let safe_query = sanitize_fts_query(query);
        let (filter_sql, filter_params) = Self::build_filter_clause(wing, room, 2);
        let (date_sql, date_params) = Self::build_date_clause(
            filed_after,
            filed_before,
            2 + filter_params.len(),
        );

        // Total count query (before limit/offset)
        let count_sql = format!(
            "SELECT COUNT(*) FROM drawers_fts
             JOIN drawers d ON drawers_fts.rowid = d.rowid
             WHERE drawers_fts MATCH ?1{filter_sql}{date_sql}"
        );
        let mut count_params = vec![SqlValue::Text(safe_query.clone())];
        count_params.extend(filter_params.clone());
        count_params.extend(date_params.clone());
        let total: i64 = self
            .conn
            .prepare(&count_sql)
            .ok()
            .and_then(|mut s| {
                s.query_row(
                    rusqlite::params_from_iter(count_params.iter()),
                    |r| r.get(0),
                )
                .ok()
            })
            .unwrap_or(0);

        let sql = format!(
            "SELECT d.id, d.wing, d.room, d.content, d.filed_at, rank
             FROM drawers_fts
             JOIN drawers d ON drawers_fts.rowid = d.rowid
             WHERE drawers_fts MATCH ?1{filter_sql}{date_sql}
             ORDER BY rank LIMIT {limit} OFFSET {offset}"
        );

        let mut stmt = match self.conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return Ok(Value::Array(vec![])),
        };

        let mut all_params = vec![SqlValue::Text(safe_query)];
        all_params.extend(filter_params);
        all_params.extend(date_params);

        let rows_result: rusqlite::Result<Vec<(String, String, String, String, String, f64)>> =
            stmt.query_map(
                rusqlite::params_from_iter(all_params.iter()),
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                        row.get(5)?,
                    ))
                },
            )
            .and_then(|iter| iter.collect());

        let mut results = Vec::new();
        if let Ok(rows) = rows_result {
            for (id, w, r, content, filed_at, rank) in rows {
                results.push(json!({
                    "id": id,
                    "wing": w,
                    "room": r,
                    "content": content,
                    "filed_at": filed_at,
                    "rank": rank,
                }));
            }
        }

        Ok(json!({
            "results": results,
            "total": total,
            "limit": limit,
            "offset": offset,
        }))
    }

    fn fts_search_raw(
        &self,
        query: &str,
        fetch: usize,
        wing: Option<&str>,
        room: Option<&str>,
        filed_after: Option<&str>,
        filed_before: Option<&str>,
    ) -> Vec<(String, String, String, String, String)> {
        let safe_query = sanitize_fts_query(query);
        let (filter_sql, filter_params) = Self::build_filter_clause(wing, room, 2);
        let (date_sql, date_params) = Self::build_date_clause(
            filed_after,
            filed_before,
            2 + filter_params.len(),
        );
        let sql = format!(
            "SELECT d.id, d.wing, d.room, d.content, d.filed_at
             FROM drawers_fts
             JOIN drawers d ON drawers_fts.rowid = d.rowid
             WHERE drawers_fts MATCH ?1{filter_sql}{date_sql}
             ORDER BY rank LIMIT {fetch}"
        );

        let mut stmt = match self.conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };

        let mut all_params = vec![SqlValue::Text(safe_query)];
        all_params.extend(filter_params);
        all_params.extend(date_params);

        stmt.query_map(
            rusqlite::params_from_iter(all_params.iter()),
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                ))
            },
        )
        .map(|iter| iter.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    fn vector_search_raw(
        &self,
        vec_bytes: &[u8],
        fetch: usize,
        wing: Option<&str>,
        room: Option<&str>,
        filed_after: Option<&str>,
        filed_before: Option<&str>,
    ) -> Vec<(String, String, String, String, String, f64)> {
        let (filter_sql, filter_params) = Self::build_filter_clause(wing, room, 2);
        let (date_sql, date_params) = Self::build_date_clause(
            filed_after,
            filed_before,
            2 + filter_params.len(),
        );
        let sql = format!(
            "SELECT d.id, d.wing, d.room, d.content, d.filed_at, v.distance
             FROM vec_drawers v
             JOIN drawers d ON v.rowid = d.rowid
             WHERE v.embedding MATCH ?1 AND k = {fetch}{filter_sql}{date_sql}
             ORDER BY v.distance"
        );

        let mut stmt = match self.conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };

        let mut all_params = vec![SqlValue::Blob(vec_bytes.to_vec())];
        all_params.extend(filter_params);
        all_params.extend(date_params);

        stmt.query_map(
            rusqlite::params_from_iter(all_params.iter()),
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    row.get(5)?,
                ))
            },
        )
        .map(|iter| iter.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    fn search_hybrid(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
        wing: Option<&str>,
        room: Option<&str>,
        filed_after: Option<&str>,
        filed_before: Option<&str>,
        embedder: &Embedder,
        use_recency: bool,
    ) -> Result<Value> {
        use std::collections::HashMap;
        const K: f64 = 60.0;
        let fetch = limit * 8;

        let vec_hits = if let Some(vec_bytes) = embedder.embed(query) {
            self.vector_search_raw(&vec_bytes, fetch, wing, room, filed_after, filed_before)
        } else {
            vec![]
        };

        let fts_hits = self.fts_search_raw(query, fetch, wing, room, filed_after, filed_before);

        if vec_hits.is_empty() && fts_hits.is_empty() {
            return Ok(json!({
                "results": [],
                "total": 0,
                "limit": limit,
                "offset": offset,
            }));
        }

        let mut rrf_scores: HashMap<String, f64> = HashMap::new();
        let mut meta: HashMap<String, (String, String, String, String)> = HashMap::new();

        for (i, (id, w, r, c, ft, _dist)) in vec_hits.iter().enumerate() {
            *rrf_scores.entry(id.clone()).or_insert(0.0) += 1.0 / (K + i as f64 + 1.0);
            meta.entry(id.clone())
                .or_insert_with(|| (w.clone(), r.clone(), c.clone(), ft.clone()));
        }
        for (i, (id, w, r, c, ft)) in fts_hits.iter().enumerate() {
            *rrf_scores.entry(id.clone()).or_insert(0.0) += 1.0 / (K + i as f64 + 1.0);
            meta.entry(id.clone())
                .or_insert_with(|| (w.clone(), r.clone(), c.clone(), ft.clone()));
        }

        if use_recency {
            let now = Self::now_epoch_secs();
            let half_life = Self::recency_half_life();
            let weight = Self::recency_weight();

            for (id, score) in rrf_scores.iter_mut() {
                if let Some((_, _, _, filed_at)) = meta.get(id) {
                    if let Some(age_secs) = parse_filed_at_age(filed_at, now) {
                        let boost = 1.0 / (1.0 + age_secs / half_life);
                        *score *= 1.0 + weight * boost;
                    }
                }
            }
        }

        let mut ranked: Vec<(String, f64)> = rrf_scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let total = ranked.len();

        let results: Vec<Value> = ranked
            .into_iter()
            .skip(offset)
            .take(limit)
            .filter_map(|(id, score)| {
                let (w, r, c, ft) = meta.get(&id)?;
                Some(json!({
                    "id": id, "wing": w, "room": r, "content": c, "filed_at": ft, "rank": score,
                }))
            })
            .collect();

        Ok(json!({
            "results": results,
            "total": total,
            "limit": limit,
            "offset": offset,
        }))
    }

    fn search_recent(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
        wing: Option<&str>,
        room: Option<&str>,
        filed_after: Option<&str>,
        filed_before: Option<&str>,
    ) -> Result<Value> {
        let safe_query = sanitize_fts_query(query);
        let (filter_sql, filter_params) = Self::build_filter_clause(wing, room, 2);
        let (date_sql, date_params) = Self::build_date_clause(
            filed_after,
            filed_before,
            2 + filter_params.len(),
        );

        // Total count
        let count_sql = format!(
            "SELECT COUNT(*) FROM drawers_fts
             JOIN drawers d ON drawers_fts.rowid = d.rowid
             WHERE drawers_fts MATCH ?1{filter_sql}{date_sql}"
        );
        let mut count_params = vec![SqlValue::Text(safe_query.clone())];
        count_params.extend(filter_params.clone());
        count_params.extend(date_params.clone());
        let total: i64 = self
            .conn
            .prepare(&count_sql)
            .ok()
            .and_then(|mut s| {
                s.query_row(
                    rusqlite::params_from_iter(count_params.iter()),
                    |r| r.get(0),
                )
                .ok()
            })
            .unwrap_or(0);

        let sql = format!(
            "SELECT d.id, d.wing, d.room, d.content, d.filed_at
             FROM drawers_fts
             JOIN drawers d ON drawers_fts.rowid = d.rowid
             WHERE drawers_fts MATCH ?1{filter_sql}{date_sql}
             ORDER BY d.filed_at DESC LIMIT {limit} OFFSET {offset}"
        );

        let mut stmt = match self.conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return Ok(Value::Array(vec![])),
        };

        let mut all_params = vec![SqlValue::Text(safe_query)];
        all_params.extend(filter_params);
        all_params.extend(date_params);

        let rows_result: rusqlite::Result<Vec<(String, String, String, String, String)>> = stmt
            .query_map(
                rusqlite::params_from_iter(all_params.iter()),
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    ))
                },
            )
            .and_then(|iter| iter.collect());

        let mut results = Vec::new();
        if let Ok(rows) = rows_result {
            for (id, w, r, content, filed_at) in rows {
                results.push(json!({
                    "id": id,
                    "wing": w,
                    "room": r,
                    "content": content,
                    "filed_at": filed_at,
                    "rank": 0.0,
                }));
            }
        }

        Ok(json!({
            "results": results,
            "total": total,
            "limit": limit,
            "offset": offset,
        }))
    }

    /// Current unix timestamp in seconds (for recency decay).
    fn now_epoch_secs() -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
    }

    fn recency_half_life() -> f64 {
        std::env::var("MEMPALACE_RECENCY_HALF_LIFE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(86400.0) // 24 hours
    }

    fn recency_weight() -> f64 {
        std::env::var("MEMPALACE_RECENCY_WEIGHT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.3)
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
        filed_at: Option<&str>,
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

        let ft = filed_at.unwrap_or("datetime('now')");
        self.conn.execute(
            "INSERT OR REPLACE INTO drawers (id, wing, room, content, source_file, added_by, filed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, wing, room, content, source_file, added_by, ft],
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

    // ── graph tools ────────────────────────────────────────────────────────────

    pub fn traverse(&self, start_room: &str, max_hops: usize) -> Result<Value> {
        use std::collections::{HashMap, HashSet};

        #[derive(Default)]
        struct RoomData {
            wings: Vec<String>,
            count: i64,
        }
        let mut room_map: HashMap<String, RoomData> = HashMap::new();

        {
            let mut stmt = self
                .conn
                .prepare("SELECT room, wing, COUNT(*) as cnt FROM drawers GROUP BY room, wing")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let room: String = row.get(0)?;
                let wing: String = row.get(1)?;
                let cnt: i64 = row.get(2)?;
                let entry = room_map.entry(room).or_default();
                entry.wings.push(wing);
                entry.count += cnt;
            }
        }

        if !room_map.contains_key(start_room) {
            let query_lower = start_room.to_lowercase();
            let suggestions: Vec<&str> = room_map
                .keys()
                .filter(|k| k.to_lowercase().contains(&query_lower))
                .take(5)
                .map(|s| s.as_str())
                .collect();
            return Ok(json!({
                "error": format!("Room '{start_room}' not found"),
                "suggestions": suggestions,
            }));
        }

        struct ResultEntry {
            room: String,
            wings: Vec<String>,
            count: i64,
            hop: usize,
        }

        let mut results: Vec<ResultEntry> = Vec::new();
        let start_data = &room_map[start_room];
        results.push(ResultEntry {
            room: start_room.to_string(),
            wings: start_data.wings.clone(),
            count: start_data.count,
            hop: 0,
        });

        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(start_room.to_string());

        let mut frontier: Vec<(String, usize)> = vec![(start_room.to_string(), 0)];
        let mut fi = 0;

        while fi < frontier.len() {
            let (current_room, depth) = frontier[fi].clone();
            fi += 1;
            if depth >= max_hops {
                continue;
            }

            let current_wings: HashSet<&str> = room_map[current_room.as_str()]
                .wings
                .iter()
                .map(|s| s.as_str())
                .collect();

            for (candidate, data) in &room_map {
                if visited.contains(candidate) {
                    continue;
                }
                let shared = data
                    .wings
                    .iter()
                    .any(|w| current_wings.contains(w.as_str()));
                if !shared {
                    continue;
                }
                visited.insert(candidate.clone());
                results.push(ResultEntry {
                    room: candidate.clone(),
                    wings: data.wings.clone(),
                    count: data.count,
                    hop: depth + 1,
                });
                if depth + 1 < max_hops {
                    frontier.push((candidate.clone(), depth + 1));
                }
            }
        }

        results.sort_by(|a, b| a.hop.cmp(&b.hop).then_with(|| b.count.cmp(&a.count)));

        let cap = results.len().min(50);
        let connections: Vec<Value> = results[..cap]
            .iter()
            .map(|re| {
                json!({
                    "room": re.room,
                    "wings": re.wings,
                    "count": re.count,
                    "hop": re.hop,
                })
            })
            .collect();

        Ok(json!({
            "start_room": start_room,
            "connections": connections,
            "rooms_visited": results.len() as i64,
        }))
    }

    pub fn find_tunnels(&self, wing_a: Option<&str>, wing_b: Option<&str>) -> Result<Value> {
        use std::collections::{HashMap, HashSet};

        struct RoomInfo {
            wings: HashSet<String>,
            count: i64,
            recent: String,
        }
        let mut room_map: HashMap<String, RoomInfo> = HashMap::new();

        {
            let mut stmt = self.conn.prepare(
                "SELECT room, wing, COUNT(*) as cnt, MAX(filed_at) as recent
                 FROM drawers GROUP BY room, wing",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let room: String = row.get(0)?;
                let wing: String = row.get(1)?;
                let cnt: i64 = row.get(2)?;
                let recent: Option<String> = row.get(3)?;
                let recent = recent.unwrap_or_default();
                let entry = room_map.entry(room).or_insert_with(|| RoomInfo {
                    wings: HashSet::new(),
                    count: 0,
                    recent: String::new(),
                });
                entry.wings.insert(wing);
                entry.count += cnt;
                if recent > entry.recent {
                    entry.recent = recent;
                }
            }
        }

        struct TunnelEntry {
            room: String,
            wings: Vec<String>,
            count: i64,
            recent: String,
        }
        let mut tunnels: Vec<TunnelEntry> = Vec::new();

        for (room, ri) in &room_map {
            if ri.wings.len() < 2 {
                continue;
            }
            if let Some(wa) = wing_a {
                if !ri.wings.contains(wa) {
                    continue;
                }
            }
            if let Some(wb) = wing_b {
                if !ri.wings.contains(wb) {
                    continue;
                }
            }
            let mut wings_sorted: Vec<String> = ri.wings.iter().cloned().collect();
            wings_sorted.sort();
            tunnels.push(TunnelEntry {
                room: room.clone(),
                wings: wings_sorted,
                count: ri.count,
                recent: ri.recent.clone(),
            });
        }

        tunnels.sort_by_key(|t| std::cmp::Reverse(t.count));
        let cap = tunnels.len().min(50);

        let items: Vec<Value> = tunnels[..cap]
            .iter()
            .map(|t| {
                json!({
                    "room": t.room,
                    "wings": t.wings,
                    "count": t.count,
                    "recent": t.recent,
                })
            })
            .collect();

        let mut result = json!({"tunnels": items});
        if let Some(wa) = wing_a {
            result["wing_a"] = json!(wa);
        }
        if let Some(wb) = wing_b {
            result["wing_b"] = json!(wb);
        }
        Ok(result)
    }

    pub fn graph_stats(&self) -> Result<Value> {
        use std::collections::{HashMap, HashSet};

        struct RoomWings {
            wings: HashSet<String>,
            count: i64,
        }
        let mut room_map: HashMap<String, RoomWings> = HashMap::new();

        {
            let mut stmt = self.conn.prepare(
                "SELECT room, wing, COUNT(*) as cnt FROM drawers WHERE room != 'general' GROUP BY room, wing",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let room: String = row.get(0)?;
                let wing: String = row.get(1)?;
                let cnt: i64 = row.get(2)?;
                let entry = room_map.entry(room).or_insert_with(|| RoomWings {
                    wings: HashSet::new(),
                    count: 0,
                });
                entry.wings.insert(wing);
                entry.count += cnt;
            }
        }

        let total_rooms = room_map.len() as i64;

        let mut all_wings: HashSet<String> = HashSet::new();
        for rw in room_map.values() {
            for w in &rw.wings {
                all_wings.insert(w.clone());
            }
        }
        let total_wings = all_wings.len() as i64;

        let total_drawers = self.get_drawer_count();

        let mut tunnel_rooms: i64 = 0;
        let mut total_edges: i64 = 0;
        for rw in room_map.values() {
            let n = rw.wings.len() as i64;
            if n >= 2 {
                tunnel_rooms += 1;
                total_edges += n * (n - 1) / 2;
            }
        }

        let mut rooms_per_wing: HashMap<String, i64> = HashMap::new();
        for rw in room_map.values() {
            for w in &rw.wings {
                *rooms_per_wing.entry(w.clone()).or_default() += 1;
            }
        }
        let rooms_per_wing_val: serde_json::Map<String, Value> = rooms_per_wing
            .into_iter()
            .map(|(k, v)| (k, json!(v)))
            .collect();

        struct TopTunnel {
            room: String,
            wings: Vec<String>,
            count: i64,
        }
        let mut top: Vec<TopTunnel> = room_map
            .iter()
            .filter(|(_, rw)| rw.wings.len() >= 2)
            .map(|(room, rw)| {
                let mut ws: Vec<String> = rw.wings.iter().cloned().collect();
                ws.sort();
                TopTunnel {
                    room: room.clone(),
                    wings: ws,
                    count: rw.count,
                }
            })
            .collect();
        top.sort_by_key(|t| std::cmp::Reverse(t.wings.len()));
        let top_cap = top.len().min(10);
        let top_arr: Vec<Value> = top[..top_cap]
            .iter()
            .map(|tt| {
                json!({
                    "room": tt.room,
                    "wings": tt.wings,
                    "count": tt.count,
                })
            })
            .collect();

        Ok(json!({
            "total_rooms": total_rooms,
            "total_wings": total_wings,
            "total_drawers": total_drawers,
            "tunnel_rooms": tunnel_rooms,
            "total_edges": total_edges,
            "rooms_per_wing": rooms_per_wing_val,
            "top_tunnels": top_arr,
        }))
    }

    /// List recently filed drawers, ordered by filed_at descending.
    pub fn list_recent(
        &self,
        limit: usize,
        wing: Option<&str>,
        since: Option<&str>,
    ) -> Result<Value> {
        let (filter_sql, filter_params) = Self::build_filter_clause(wing, None, 1);
        let since_clause = if since.is_some() {
            let idx = filter_params.len() + 1;
            format!(" AND d.filed_at >= ?{idx}")
        } else {
            String::new()
        };

        let sql = format!(
            "SELECT d.id, d.wing, d.room, d.content, d.filed_at
             FROM drawers d
             WHERE 1=1{filter_sql}{since_clause}
             ORDER BY d.filed_at DESC LIMIT {limit}"
        );

        let mut stmt = self.conn.prepare(&sql)?;

        let mut all_params = filter_params;
        if let Some(s) = since {
            all_params.push(SqlValue::Text(s.to_string()));
        }

        let rows: Vec<(String, String, String, String, String)> = stmt
            .query_map(
                rusqlite::params_from_iter(all_params.iter()),
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    ))
                },
            )
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();

        let results: Vec<Value> = rows
            .into_iter()
            .map(|(id, w, r, c, ft)| {
                json!({"id": id, "wing": w, "room": r, "content": c, "filed_at": ft})
            })
            .collect();

        Ok(Value::Array(results))
    }

    // ── export / backup / restore ──────────────────────────────────────────────

    /// Export drawers as JSON Lines string. Filter by wing and/or room.
    pub fn export_drawers(&self, wing: Option<&str>, room: Option<&str>) -> Result<String> {
        let (sql, params): (&str, Vec<SqlValue>) = match (wing, room) {
            (Some(w), Some(r)) => (
                "SELECT id, wing, room, content, source_file, filed_at
                 FROM drawers WHERE wing = ?1 AND room = ?2 ORDER BY filed_at DESC",
                vec![SqlValue::Text(w.to_string()), SqlValue::Text(r.to_string())],
            ),
            (Some(w), None) => (
                "SELECT id, wing, room, content, source_file, filed_at
                 FROM drawers WHERE wing = ?1 ORDER BY filed_at DESC",
                vec![SqlValue::Text(w.to_string())],
            ),
            (None, Some(r)) => (
                "SELECT id, wing, room, content, source_file, filed_at
                 FROM drawers WHERE room = ?1 ORDER BY filed_at DESC",
                vec![SqlValue::Text(r.to_string())],
            ),
            (None, None) => (
                "SELECT id, wing, room, content, source_file, filed_at
                 FROM drawers ORDER BY filed_at DESC",
                vec![],
            ),
        };

        let mut stmt = self.conn.prepare(sql)?;
        let items: Vec<Value> = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "wing": r.get::<_, String>(1)?,
                    "room": r.get::<_, String>(2)?,
                    "content": r.get::<_, String>(3)?,
                    "source_file": r.get::<_, Option<String>>(4)?,
                    "filed_at": r.get::<_, Option<String>>(5)?,
                }))
            })
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();

        let lines: Vec<String> = items.into_iter().map(|v| v.to_string()).collect();
        Ok(lines.join("\n"))
    }

    /// Export all knowledge graph triples as JSON.
    pub fn export_kg(&self) -> Result<Value> {
        let mut stmt = self.conn.prepare(
            "SELECT subject, predicate, object, valid_from, valid_until, source_closet
             FROM triples ORDER BY subject",
        )?;
        let triples: Vec<Value> = stmt
            .query_map([], |r| {
                let mut j = json!({
                    "subject": r.get::<_, String>(0)?,
                    "predicate": r.get::<_, String>(1)?,
                    "object": r.get::<_, String>(2)?,
                });
                if let Ok(Some(vf)) = r.get::<_, Option<String>>(3) {
                    j["valid_from"] = json!(vf);
                }
                if let Ok(Some(vu)) = r.get::<_, Option<String>>(4) {
                    j["valid_until"] = json!(vu);
                }
                if let Ok(Some(sc)) = r.get::<_, Option<String>>(5) {
                    j["source_closet"] = json!(sc);
                }
                Ok(j)
            })
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();

        Ok(json!({"triples": triples, "count": triples.len()}))
    }

    /// Backup the palace database by copying the file.
    pub fn backup(&self, path: Option<&str>) -> Result<String> {
        let source = self.conn.path().unwrap_or("unknown").to_string();
        let dest = match path {
            Some(p) => p.to_string(),
            None => {
                let dir = std::path::Path::new(&source)
                    .parent()
                    .unwrap_or(std::path::Path::new("."))
                    .join("backups");
                std::fs::create_dir_all(&dir)?;
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                dir.join(format!("palace-backup-{ts}.db"))
                    .to_str()
                    .unwrap_or("backup.db")
                    .to_string()
            }
        };
        std::fs::copy(&source, &dest)?;
        Ok(dest)
    }

    /// Restore from a backup file. Warning: overwrites current data.
    pub fn restore(&self, backup_path: &str) -> Result<()> {
        let source = self.conn.path().unwrap_or("unknown").to_string();
        if !std::path::Path::new(backup_path).exists() {
            return Err(anyhow!("Backup file not found: {backup_path}"));
        }
        std::fs::copy(backup_path, &source)?;
        Ok(())
    }
}

/// Parse a `filed_at` string (format "YYYY-MM-DD HH:MM:SS" or ISO) into seconds
/// since epoch, then return the age relative to `now_secs`. Returns None on parse failure.
fn parse_filed_at_age(filed_at: &str, now_secs: f64) -> Option<f64> {
    if filed_at.is_empty() {
        return None;
    }
    let ts = filed_at.trim();
    // Normalize: try ISO 8601 or "YYYY-MM-DD HH:MM:SS"
    let normalized: String = ts.replace('T', " ");
    let parts: Vec<&str> = normalized.split(&[' ', '-', ':'][..]).collect();

    if parts.len() < 3 {
        return None;
    }

    let year: i32 = parts[0].parse().ok()?;
    let month: u32 = parts[1].parse().ok()?;
    let day: u32 = parts[2].parse().ok()?;
    let hour: u32 = parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    let min: u32 = parts.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
    let sec: u32 = parts.get(5).and_then(|s| s.parse().ok()).unwrap_or(0);

    // days from 1970-01-01 (simplified: assume all months are ~30.44 days)
    let days = (year - 1970) as f64 * 365.25
        + (month - 1) as f64 * 30.44
        + day as f64;
    let epoch_secs = days * 86400.0 + hour as f64 * 3600.0 + min as f64 * 60.0 + sec as f64;

    Some((now_secs - epoch_secs).max(0.0))
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
        db.upsert_drawer(
            "custom_id_1",
            "w",
            "r",
            "hello upsert",
            None,
            "test",
            None,
            None,
        )
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
        let results = db
            .search("quick fox", 5, 0, None, None, None, None, None, "relevance")
            .unwrap();
        let arr = results["results"].as_array().unwrap();
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
            .search("zzzznotpresentzzzz", 5, 0, None, None, None, None, None, "relevance")
            .unwrap();
        let arr = results["results"].as_array().unwrap();
        assert!(arr.is_empty());
    }

    #[test]
    fn test_fts_search_wing_filter() {
        let (_dir, db) = test_db();
        db.add_drawer("alpha", "r", "shared keyword", None, "test", None)
            .unwrap();
        db.add_drawer("beta", "r", "shared keyword", None, "test", None)
            .unwrap();
        let results = db.search("shared", 10, 0, Some("alpha"), None, None, None, None, "relevance").unwrap();
        let arr = results["results"].as_array().unwrap();
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
            .search("recipe", 10, 0, None, Some("kitchen"), None, None, None, "relevance")
            .unwrap();
        let arr = results["results"].as_array().unwrap();
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
        let results = db.search("common", 3, 0, None, None, None, None, None, "relevance").unwrap();
        let arr = results["results"].as_array().unwrap();
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
        let results = db.search("refreshed", 5, 0, None, None, None, None, None, "relevance").unwrap();
        let arr = results["results"].as_array().unwrap();
        assert!(!arr.is_empty());
        assert!(arr[0]["content"].as_str().unwrap().contains("refreshed"));
        // Old text should not match
        let old_results = db.search("old text", 5, 0, None, None, None, None, None, "relevance").unwrap();
        assert!(old_results["results"].as_array().unwrap().is_empty());
    }

    // ── graph tools ────────────────────────────────────────────────────────────

    #[test]
    fn test_traverse_existing_room() {
        let (_dir, db) = test_db();
        // Add drawers in the same wing, different rooms
        db.add_drawer("wing_a", "room-start", "content", None, "test", None)
            .unwrap();
        db.add_drawer("wing_a", "room-alpha", "content", None, "test", None)
            .unwrap();
        db.add_drawer("wing_a", "room-beta", "content", None, "test", None)
            .unwrap();
        let result = db.traverse("room-start", 2).unwrap();
        let connections = result["connections"].as_array().unwrap();
        assert!(!connections.is_empty());
        // Start room should be at hop 0
        assert_eq!(connections[0]["room"], "room-start");
        assert_eq!(connections[0]["hop"], 0);
    }

    #[test]
    fn test_traverse_missing_room() {
        let (_dir, db) = test_db();
        let result = db.traverse("nonexistent-room", 2).unwrap();
        assert!(result["error"].as_str().is_some());
        assert!(result["suggestions"].as_array().is_some());
    }

    #[test]
    fn test_traverse_respects_max_hops() {
        let (_dir, db) = test_db();
        // wing_a → rooms A,B,C  |  wing_b → rooms B,D  |  wing_c → rooms D,E,F
        db.add_drawer("wing_a", "room-a", "x", None, "test", None)
            .unwrap();
        db.add_drawer("wing_a", "room-b", "x", None, "test", None)
            .unwrap();
        db.add_drawer("wing_a", "room-c", "x", None, "test", None)
            .unwrap();
        db.add_drawer("wing_b", "room-b", "x", None, "test", None)
            .unwrap();
        db.add_drawer("wing_b", "room-d", "x", None, "test", None)
            .unwrap();
        db.add_drawer("wing_c", "room-d", "x", None, "test", None)
            .unwrap();
        db.add_drawer("wing_c", "room-e", "x", None, "test", None)
            .unwrap();
        db.add_drawer("wing_c", "room-f", "x", None, "test", None)
            .unwrap();

        // max_hops=1: room-a → room-b, room-c only (shared wing_a)
        let r1 = db.traverse("room-a", 1).unwrap();
        for conn in r1["connections"].as_array().unwrap() {
            assert!(conn["hop"].as_u64().unwrap() <= 1);
        }

        // max_hops=3: should reach room-e via room-a→room-b→room-d→room-e
        let r3 = db.traverse("room-a", 3).unwrap();
        let rooms: Vec<&str> = r3["connections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["room"].as_str().unwrap())
            .collect();
        assert!(rooms.contains(&"room-e"));
    }

    #[test]
    fn test_find_tunnels_between_two_wings() {
        let (_dir, db) = test_db();
        // room shared by wing_a AND wing_b
        db.add_drawer("wing_a", "bridge-room", "c", None, "test", None)
            .unwrap();
        db.add_drawer("wing_b", "bridge-room", "c", None, "test", None)
            .unwrap();
        // room only in wing_a
        db.add_drawer("wing_a", "solo-room", "c", None, "test", None)
            .unwrap();

        let result = db.find_tunnels(Some("wing_a"), Some("wing_b")).unwrap();
        let tunnels = result["tunnels"].as_array().unwrap();
        assert_eq!(tunnels.len(), 1);
        assert_eq!(tunnels[0]["room"], "bridge-room");
    }

    #[test]
    fn test_find_tunnels_no_filter() {
        let (_dir, db) = test_db();
        db.add_drawer("wing_a", "shared", "c", None, "test", None)
            .unwrap();
        db.add_drawer("wing_b", "shared", "c", None, "test", None)
            .unwrap();

        let result = db.find_tunnels(None, None).unwrap();
        let tunnels = result["tunnels"].as_array().unwrap();
        assert!(!tunnels.is_empty());
    }

    #[test]
    fn test_find_tunnels_no_matches() {
        let (_dir, db) = test_db();
        db.add_drawer("wing_a", "only-room", "c", None, "test", None)
            .unwrap();

        let result = db.find_tunnels(Some("wing_x"), Some("wing_y")).unwrap();
        let tunnels = result["tunnels"].as_array().unwrap();
        assert!(tunnels.is_empty());
    }

    #[test]
    fn test_graph_stats() {
        let (_dir, db) = test_db();
        db.add_drawer("wing_a", "room1", "c", None, "test", None)
            .unwrap();
        db.add_drawer("wing_a", "room2", "c", None, "test", None)
            .unwrap();
        db.add_drawer("wing_b", "room1", "c", None, "test", None)
            .unwrap();

        let stats = db.graph_stats().unwrap();
        assert!(stats["total_rooms"].as_i64().unwrap() > 0);
        assert!(stats["total_wings"].as_i64().unwrap() > 0);
        assert!(stats["total_drawers"].as_i64().unwrap() > 0);
        assert!(stats["tunnel_rooms"].as_i64().unwrap() >= 1);
        assert!(stats["total_edges"].as_i64().unwrap() >= 1);
        assert!(stats["rooms_per_wing"].as_object().is_some());
        assert!(stats["top_tunnels"].as_array().is_some());
    }

    // ── recency search ─────────────────────────────────────────────────────────

    #[test]
    fn test_search_sort_by_recency() {
        let (_dir, db) = test_db();
        // Insert drawers with staggered filed_at via direct SQL (bypassing datetime('now'))
        for i in 0..5 {
            db.conn
                .execute(
                    "INSERT INTO drawers (id, wing, room, content, filed_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        format!("recency_test_{i}"),
                        "w",
                        "r",
                        "a common keyword for search",
                        format!("2024-0{}-01 00:00:00", i + 1),
                    ],
                )
                .unwrap();
        }
        // Index in FTS
        db.conn
            .execute(
                "INSERT INTO drawers_fts(rowid, content, wing, room)
             SELECT rowid, content, wing, room FROM drawers",
                [],
            )
            .unwrap();

        let results = db
            .search("common keyword", 5, 0, None, None, None, None, None, "recency")
            .unwrap();
        let arr = results["results"].as_array().unwrap();
        assert_eq!(arr.len(), 5);
        // Most recent first: 2024-05-01, then 2024-04-01, ...
        assert!(arr[0]["filed_at"].as_str().unwrap().contains("2024-05"));
        assert!(arr[4]["filed_at"].as_str().unwrap().contains("2024-01"));
    }

    #[test]
    fn test_search_sort_by_relevance_preserved() {
        let (_dir, db) = test_db();
        db.add_drawer("w", "r1", "specific rare term alpha", None, "test", None)
            .unwrap();
        db.add_drawer("w", "r2", "some other unrelated text", None, "test", None)
            .unwrap();
        let results = db
            .search("alpha", 2, 0, None, None, None, None, None, "relevance")
            .unwrap();
        let arr = results["results"].as_array().unwrap();
        assert!(arr[0]["room"] == "r1");
    }

    #[test]
    fn test_search_result_includes_filed_at() {
        let (_dir, db) = test_db();
        db.add_drawer("w", "r", "test filed_at presence", None, "test", None)
            .unwrap();
        let results = db
            .search("filed_at presence", 1, 0, None, None, None, None, None, "relevance")
            .unwrap();
        let arr = results["results"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert!(arr[0]["filed_at"].as_str().is_some());
        assert!(!arr[0]["filed_at"].as_str().unwrap().is_empty());
    }

    #[test]
    fn test_parse_filed_at_age_now() {
        // A date far in the future should give large negative age, but we clamp to 0
        let age = parse_filed_at_age("2099-01-01 00:00:00", 1_000_000_000.0);
        assert!(age.is_none() || age.unwrap() == 0.0);
    }

    #[test]
    fn test_parse_filed_at_age_past() {
        // 2020-01-01 00:00:00 relative to now should be > 0
        let age = parse_filed_at_age("2020-01-01 00:00:00", 2_000_000_000.0);
        assert!(age.is_some());
        assert!(age.unwrap() > 0.0);
    }

    #[test]
    fn test_parse_filed_at_age_empty() {
        assert!(parse_filed_at_age("", 0.0).is_none());
    }

    // ── list_recent ────────────────────────────────────────────────────────────

    #[test]
    fn test_list_recent_order() {
        let (_dir, db) = test_db();
        for i in 1..=3 {
            db.conn
                .execute(
                    "INSERT INTO drawers (id, wing, room, content, filed_at) VALUES (?1, 'w', 'r', ?2, ?3)",
                    params![
                        format!("recent_{i}"),
                        format!("content {i}"),
                        format!("2024-01-0{i} 00:00:00"),
                    ],
                )
                .unwrap();
        }
        let results = db.list_recent(5, None, None).unwrap();
        let arr = results.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        // Newest first
        assert!(arr[0]["filed_at"].as_str().unwrap().contains("2024-01-03"));
    }

    #[test]
    fn test_list_recent_since() {
        let (_dir, db) = test_db();
        db.conn
            .execute(
                "INSERT INTO drawers (id, wing, room, content, filed_at) VALUES ('old', 'w', 'r', 'c', '2023-06-01 00:00:00')",
                [],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO drawers (id, wing, room, content, filed_at) VALUES ('new', 'w', 'r', 'c', '2024-06-01 00:00:00')",
                [],
            )
            .unwrap();
        let results = db
            .list_recent(5, None, Some("2024-01-01"))
            .unwrap();
        let arr = results.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "new");
    }

    // ── sync state ────────────────────────────────────────────────────────────

    #[test]
    fn test_sync_state_default() {
        let (_dir, db) = test_db();
        assert_eq!(db.get_sync_state("test_source"), 0);
    }

    #[test]
    fn test_sync_state_set_and_get() {
        let (_dir, db) = test_db();
        db.set_sync_state("test_source", 1234567890).unwrap();
        assert_eq!(db.get_sync_state("test_source"), 1234567890);
    }

    #[test]
    fn test_sync_state_replace() {
        let (_dir, db) = test_db();
        db.set_sync_state("test_source", 100).unwrap();
        db.set_sync_state("test_source", 200).unwrap();
        assert_eq!(db.get_sync_state("test_source"), 200);
    }

    // ── input validation & edge cases ──────────────────────────────────────────

    #[test]
    fn test_search_limit_clamped() {
        let (_dir, db) = test_db();
        db.add_drawer("w", "r", "test", None, "test", None)
            .unwrap();
        let r = db
            .search("test", 0, 0, None, None, None, None, None, "relevance")
            .unwrap();
        assert!(r["results"].as_array().unwrap().len() >= 1);
    }

    #[test]
    fn test_search_offset_beyond_total() {
        let (_dir, db) = test_db();
        db.add_drawer("w", "r", "test", None, "test", None)
            .unwrap();
        let r = db
            .search("test", 5, 100, None, None, None, None, None, "relevance")
            .unwrap();
        assert_eq!(r["results"].as_array().unwrap().len(), 0);
        assert!(r["total"].as_i64().unwrap() > 0);
    }

    #[test]
    fn test_fts_search_unicode_query() {
        let (_dir, db) = test_db();
        // Default FTS5 tokenizer may not segment CJK — verify it doesn't panic
        db.add_drawer("w", "r", "hello 世界 test", None, "test", None)
            .unwrap();
        let r = db
            .search("hello", 5, 0, None, None, None, None, None, "relevance")
            .unwrap();
        assert!(!r["results"].as_array().unwrap().is_empty());
    }

    // ── performance smoke test ─────────────────────────────────────────────────

    #[test]
    fn test_search_latency_budget() {
        let (_dir, db) = test_db();
        let n = 2000;
        for i in 0..n {
            db.add_drawer(
                "wing",
                &format!("r{i}"),
                &format!("unique term {i} some filler for realistic perf testing"),
                None,
                "test",
                None,
            )
            .unwrap();
        }
        let start = std::time::Instant::now();
        let _ = db
            .search("unique term", 10, 0, None, None, None, None, None, "relevance")
            .unwrap();
        let ms = start.elapsed().as_millis();
        assert!(ms < 500, "search latency {ms}ms exceeds budget");
    }

    // ── vec0 health ────────────────────────────────────────────────────────────

    #[test]
    fn test_vec0_health_on_fresh_db() {
        let (_dir, db) = test_db();
        let health = db.vec0_health();
        assert_eq!(health["drawer_count"], 0);
        assert_eq!(health["embedded_count"], 0);
    }

    #[test]
    fn test_vec0_health_after_adds() {
        let (_dir, db) = test_db();
        for i in 0..5 {
            db.add_drawer("w", &format!("r{i}"), "content", None, "test", None)
                .unwrap();
        }
        // Fresh adds don't embed (no embedder), so gap should be 100%
        let health = db.vec0_health();
        assert_eq!(health["drawer_count"].as_i64().unwrap(), 5);
        // embedded_count will be 0 without embedder in tests
        assert!(health["gap_pct"].as_f64().unwrap() >= 0.0);
    }

    #[test]
    fn test_search_falls_back_to_fts() {
        let (_dir, db) = test_db();
        db.add_drawer("w", "r", "should find this via fts", None, "test", None)
            .unwrap();
        // With vector disabled, search should still work via FTS
        let r = db
            .search("should find", 5, 0, None, None, None, None, None, "relevance")
            .unwrap();
        assert!(!r["results"].as_array().unwrap().is_empty());
    }
}
