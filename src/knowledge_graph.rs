use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use crate::db::Database;

// Type aliases (reduces type_complexity)
type TripleRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

#[allow(clippy::too_many_arguments)]
fn triple_to_json(
    subject: String,
    predicate: String,
    object: String,
    valid_from: Option<String>,
    valid_until: Option<String>,
    source_closet: Option<String>,
    source_file: Option<String>,
    source_drawer_id: Option<String>,
) -> Value {
    let mut fact = json!({
        "subject": subject,
        "predicate": predicate,
        "object": object,
    });
    if let Some(vf) = valid_from {
        fact["valid_from"] = json!(vf);
    }
    if let Some(vu) = valid_until {
        fact["valid_until"] = json!(vu);
    }
    if let Some(sc) = source_closet {
        fact["source_closet"] = json!(sc);
    }
    if let Some(sf) = source_file {
        fact["source_file"] = json!(sf);
    }
    if let Some(sd) = source_drawer_id {
        fact["source_drawer_id"] = json!(sd);
    }
    fact
}

pub struct KnowledgeGraph<'a> {
    pub db: &'a Database,
}

impl<'a> KnowledgeGraph<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn query_entity(
        &self,
        entity: &str,
        as_of: Option<&str>,
        direction: &str,
    ) -> Result<Value> {
        let sql = match direction {
            "outgoing" => {
                "SELECT subject, predicate, object, valid_from, valid_until, source_closet, source_file, source_drawer_id
                 FROM triples WHERE subject = ?1
                 AND (?2 IS NULL OR (valid_from IS NULL OR valid_from <= ?2)
                     AND (valid_until IS NULL OR valid_until > ?2))"
                    .to_string()
            }
            "incoming" => {
                "SELECT subject, predicate, object, valid_from, valid_until, source_closet, source_file, source_drawer_id
                 FROM triples WHERE object = ?1
                 AND (?2 IS NULL OR (valid_from IS NULL OR valid_from <= ?2)
                     AND (valid_until IS NULL OR valid_until > ?2))"
                    .to_string()
            }
            _ => "SELECT subject, predicate, object, valid_from, valid_until, source_closet, source_file, source_drawer_id
                 FROM triples WHERE (subject = ?1 OR object = ?1)
                 AND (?2 IS NULL OR (valid_from IS NULL OR valid_from <= ?2)
                     AND (valid_until IS NULL OR valid_until > ?2))"
                .to_string(),
        };

        let mut stmt = self.db.conn.prepare(&sql)?;
        let mut facts = Vec::new();

        let rows: Vec<TripleRow> = stmt
            .query_map(params![entity, as_of], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            })
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();

        for (
            subject,
            predicate,
            object,
            valid_from,
            valid_until,
            source_closet,
            source_file,
            source_drawer_id,
        ) in rows
        {
            let mut fact = json!({
                "subject": subject,
                "predicate": predicate,
                "object": object,
            });
            if let Some(vf) = valid_from {
                fact["valid_from"] = json!(vf);
            }
            if let Some(vu) = valid_until {
                fact["valid_until"] = json!(vu);
            }
            if let Some(sc) = source_closet {
                fact["source_closet"] = json!(sc);
            }
            if let Some(sf) = source_file {
                fact["source_file"] = json!(sf);
            }
            if let Some(sd) = source_drawer_id {
                fact["source_drawer_id"] = json!(sd);
            }
            facts.push(fact);
        }

        Ok(Value::Array(facts))
    }

    pub fn add_triple(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        valid_from: Option<&str>,
        valid_to: Option<&str>,
        source_closet: Option<&str>,
    ) -> Result<String> {
        // Reject inverted intervals: valid_to < valid_from
        if let (Some(vf), Some(vt)) = (valid_from, valid_to) {
            if vt < vf {
                return Err(anyhow::anyhow!(
                    "Invalid date range: valid_to ({vt}) precedes valid_from ({vf})"
                ));
            }
        }

        // Idempotency: return existing active triple if it exists
        let existing: Option<String> = self
            .db
            .conn
            .query_row(
                "SELECT id FROM triples WHERE subject=?1 AND predicate=?2 AND object=?3 AND valid_until IS NULL",
                params![subject, predicate, object],
                |r| r.get(0),
            )
            .optional()?;

        if let Some(id) = existing {
            return Ok(id);
        }

        // Generate triple ID
        let hash_input = format!(
            "{}{}{}{}",
            subject,
            predicate,
            object,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        );
        let digest = md5::compute(hash_input.as_bytes());
        let hex = format!("{:x}", digest);
        let triple_id = format!("triple_{}", &hex[..16]);

        self.db.conn.execute(
            "INSERT INTO triples (id, subject, predicate, object, valid_from, valid_until, source_closet, source_file, source_drawer_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![triple_id, subject, predicate, object, valid_from, valid_to, source_closet, Option::<&str>::None, Option::<&str>::None],
        )?;

        Ok(triple_id)
    }

    /// Atomically close `old_object` and open `new_object` at a shared instant.
    pub fn supersede(
        &self,
        subject: &str,
        predicate: &str,
        old_object: &str,
        new_object: &str,
        at: Option<&str>,
    ) -> Result<Value> {
        if old_object == new_object {
            return Err(anyhow::anyhow!(
                "InvalidArgument: old_object and new_object must differ"
            ));
        }
        let at =
            match at {
                Some(s) => s.to_string(),
                None => self.db.conn.query_row(
                    "SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
                    [],
                    |r| r.get::<_, String>(0),
                )?,
            };

        self.db.conn.execute("BEGIN IMMEDIATE", [])?;
        let result = (|| -> Result<Value> {
            let open: Option<String> = self
                .db
                .conn
                .query_row(
                    "SELECT id FROM triples WHERE subject=?1 AND predicate=?2 AND object=?3 AND valid_until IS NULL",
                    params![subject, predicate, old_object],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(old_id) = open else {
                let ended: Option<String> = self
                    .db
                    .conn
                    .query_row(
                        "SELECT id FROM triples WHERE subject=?1 AND predicate=?2 AND object=?3",
                        params![subject, predicate, old_object],
                        |r| r.get(0),
                    )
                    .optional()?;
                if ended.is_some() {
                    return Err(anyhow::anyhow!("FactAlreadyEnded"));
                }
                return Err(anyhow::anyhow!("FactNotFound"));
            };

            self.db.conn.execute(
                "UPDATE triples SET valid_until = ?1 WHERE id = ?2",
                params![&at, &old_id],
            )?;

            let new_id = self.add_triple(subject, predicate, new_object, Some(&at), None, None)?;

            Ok(json!({
                "success": true,
                "triple_id": new_id,
                "fact": format!("{subject} → {predicate} → {new_object}"),
                "superseded": old_id,
                "at": at,
            }))
        })();

        match &result {
            Ok(_) => {
                self.db.conn.execute("COMMIT", [])?;
            }
            Err(_) => {
                let _ = self.db.conn.execute("ROLLBACK", []);
            }
        }
        result
    }

    pub fn set_triple_provenance(
        &self,
        triple_id: &str,
        source_file: Option<&str>,
        source_drawer_id: Option<&str>,
    ) -> Result<()> {
        self.db.conn.execute(
            "UPDATE triples SET source_file=?2, source_drawer_id=?3 WHERE id=?1",
            params![triple_id, source_file, source_drawer_id],
        )?;
        Ok(())
    }

    pub fn invalidate(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        ended: Option<&str>,
    ) -> Result<()> {
        self.db.conn.execute(
            "UPDATE triples SET valid_until = COALESCE(?4, date('now'))
             WHERE subject = ?1 AND predicate = ?2 AND object = ?3
             AND valid_until IS NULL",
            params![subject, predicate, object, ended],
        )?;
        Ok(())
    }

    pub fn get_timeline(&self, entity: Option<&str>) -> Result<Value> {
        let mut facts = Vec::new();

        if let Some(e) = entity {
            let mut stmt = self.db.conn.prepare(
                "SELECT subject, predicate, object, valid_from, valid_until, source_closet, source_file, source_drawer_id
                 FROM triples WHERE subject = ?1 OR object = ?1
                 ORDER BY COALESCE(valid_from, '0000-00-00')",
            )?;
            let rows: Vec<TripleRow> = stmt
                .query_map(params![e], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                })
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
                .unwrap_or_default();

            for (
                subject,
                predicate,
                object,
                valid_from,
                valid_until,
                source_closet,
                source_file,
                source_drawer_id,
            ) in rows
            {
                facts.push(triple_to_json(
                    subject,
                    predicate,
                    object,
                    valid_from,
                    valid_until,
                    source_closet,
                    source_file,
                    source_drawer_id,
                ));
            }
        } else {
            let mut stmt = self.db.conn.prepare(
                "SELECT subject, predicate, object, valid_from, valid_until, source_closet, source_file, source_drawer_id
                 FROM triples ORDER BY COALESCE(valid_from, '0000-00-00') LIMIT 100",
            )?;
            let rows: Vec<TripleRow> = stmt
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                })
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
                .unwrap_or_default();

            for (
                subject,
                predicate,
                object,
                valid_from,
                valid_until,
                source_closet,
                source_file,
                source_drawer_id,
            ) in rows
            {
                facts.push(triple_to_json(
                    subject,
                    predicate,
                    object,
                    valid_from,
                    valid_until,
                    source_closet,
                    source_file,
                    source_drawer_id,
                ));
            }
        }

        Ok(Value::Array(facts))
    }

    pub fn get_stats(&self) -> Result<Value> {
        let unique_entities: i64 = self.db.conn.query_row(
            "SELECT COUNT(*) FROM (SELECT subject AS e FROM triples UNION SELECT object FROM triples)",
            [],
            |r| r.get(0),
        )?;

        let total_triples: i64 =
            self.db
                .conn
                .query_row("SELECT COUNT(*) FROM triples", [], |r| r.get(0))?;

        let current_facts: i64 = self.db.conn.query_row(
            "SELECT COUNT(*) FROM triples WHERE valid_until IS NULL",
            [],
            |r| r.get(0),
        )?;

        let expired_facts = total_triples - current_facts;

        let mut stmt = self
            .db
            .conn
            .prepare("SELECT DISTINCT predicate FROM triples ORDER BY predicate")?;
        let predicates: Vec<Value> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map(|iter| iter.filter_map(|r| r.ok()).map(|s| json!(s)).collect())
            .unwrap_or_default();

        Ok(json!({
            "unique_entities": unique_entities,
            "total_triples": total_triples,
            "current_facts": current_facts,
            "expired_facts": expired_facts,
            "relationship_types": predicates,
        }))
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use tempfile::TempDir;

    fn test_db() -> (TempDir, Database) {
        let dir = TempDir::new().unwrap();
        let db = Database::open(dir.path().to_str().unwrap()).unwrap();
        (dir, db)
    }

    #[test]
    fn test_add_triple() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        let id = kg
            .add_triple("Alice", "loves", "chess", None, None, None)
            .unwrap();
        assert!(id.starts_with("triple_"));
        // Verify in DB
        let (s, p, o): (String, String, String) = db
            .conn
            .query_row(
                "SELECT subject, predicate, object FROM triples WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(s, "Alice");
        assert_eq!(p, "loves");
        assert_eq!(o, "chess");
    }

    #[test]
    fn test_add_triple_idempotent() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        let id1 = kg
            .add_triple("Alice", "loves", "chess", None, None, None)
            .unwrap();
        let id2 = kg
            .add_triple("Alice", "loves", "chess", None, None, None)
            .unwrap();
        assert_eq!(id1, id2); // Same active triple returns same ID
    }

    #[test]
    fn test_add_triple_with_valid_from() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        let id = kg
            .add_triple(
                "Max",
                "started_school",
                "Year 7",
                Some("2026-09-01"),
                None,
                None,
            )
            .unwrap();
        let vf: String = db
            .conn
            .query_row(
                "SELECT valid_from FROM triples WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(vf, "2026-09-01");
    }

    #[test]
    fn test_add_triple_with_source_closet() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        let id = kg
            .add_triple("X", "relates_to", "Y", None, None, Some("closet_42"))
            .unwrap();
        let sc: String = db
            .conn
            .query_row(
                "SELECT source_closet FROM triples WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sc, "closet_42");
    }

    #[test]
    fn test_query_entity_outgoing() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("Alice", "child_of", "Bob", None, None, None)
            .unwrap();
        kg.add_triple("Bob", "child_of", "Charlie", None, None, None)
            .unwrap();
        let facts = kg.query_entity("Alice", None, "outgoing").unwrap();
        let arr = facts.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["subject"], "Alice");
        assert_eq!(arr[0]["predicate"], "child_of");
        assert_eq!(arr[0]["object"], "Bob");
    }

    #[test]
    fn test_query_entity_incoming() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("Alice", "child_of", "Bob", None, None, None)
            .unwrap();
        let facts = kg.query_entity("Bob", None, "incoming").unwrap();
        let arr = facts.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["subject"], "Alice");
    }

    #[test]
    fn test_query_entity_both() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("Alice", "child_of", "Bob", None, None, None)
            .unwrap();
        kg.add_triple("Bob", "child_of", "Charlie", None, None, None)
            .unwrap();
        let facts = kg.query_entity("Bob", None, "both").unwrap();
        let arr = facts.as_array().unwrap();
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn test_query_as_of() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("Alice", "works_at", "ACME", Some("2020-01-01"), None, None)
            .unwrap();
        // Should be valid at 2021-01-01
        let facts = kg
            .query_entity("Alice", Some("2021-01-01"), "both")
            .unwrap();
        assert_eq!(facts.as_array().unwrap().len(), 1);
        // Should NOT match before valid_from
        let facts = kg
            .query_entity("Alice", Some("2019-01-01"), "both")
            .unwrap();
        assert_eq!(facts.as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_invalidate() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("Alice", "works_at", "ACME", Some("2020-01-01"), None, None)
            .unwrap();
        kg.invalidate("Alice", "works_at", "ACME", Some("2024-01-01"))
            .unwrap();
        let vu: String = db
            .conn
            .query_row(
                "SELECT valid_until FROM triples WHERE subject='Alice' AND predicate='works_at'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(vu, "2024-01-01");
    }

    #[test]
    fn test_invalidate_defaults_to_today() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("Alice", "works_at", "ACME", None, None, None)
            .unwrap();
        kg.invalidate("Alice", "works_at", "ACME", None).unwrap();
        let vu: Option<String> = db
            .conn
            .query_row(
                "SELECT valid_until FROM triples WHERE subject='Alice'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(vu.is_some());
    }

    #[test]
    fn test_timeline_entity() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("Alice", "born", "1990", Some("1990-01-01"), None, None)
            .unwrap();
        kg.add_triple("Alice", "graduated", "2012", Some("2012-06-01"), None, None)
            .unwrap();
        let timeline = kg.get_timeline(Some("Alice")).unwrap();
        let arr = timeline.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        // Should be ordered by valid_from
        assert_eq!(arr[0]["valid_from"], "1990-01-01");
        assert_eq!(arr[1]["valid_from"], "2012-06-01");
    }

    #[test]
    fn test_stats_with_expired_facts() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("A", "p", "B", None, None, None).unwrap();
        kg.invalidate("A", "p", "B", Some("2025-01-01")).unwrap();
        let stats = kg.get_stats().unwrap();
        assert_eq!(stats["total_triples"], 1);
        assert_eq!(stats["current_facts"], 0);
        assert_eq!(stats["expired_facts"], 1);
    }

    #[test]
    fn test_add_triple_with_valid_to() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        let id = kg
            .add_triple(
                "Alice",
                "worked_at",
                "ACME",
                Some("2020-01-01"),
                Some("2023-12-31"),
                None,
            )
            .unwrap();
        // Verify valid_until is set
        let vu: Option<String> = db
            .conn
            .query_row(
                "SELECT valid_until FROM triples WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(vu.unwrap(), "2023-12-31");
    }

    #[test]
    fn test_add_triple_rejects_inverted_interval() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        let result = kg.add_triple("X", "p", "Y", Some("2024-12-31"), Some("2020-01-01"), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("precedes"));
    }

    #[test]
    fn test_stats() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("Alice", "loves", "chess", None, None, None)
            .unwrap();
        kg.add_triple("Bob", "loves", "go", None, None, None)
            .unwrap();
        let stats = kg.get_stats().unwrap();
        assert_eq!(stats["total_triples"], 2);
        assert_eq!(stats["current_facts"], 2);
        assert_eq!(stats["expired_facts"], 0);
        // unique_entities: Alice, chess, Bob, go = 4
        assert_eq!(stats["unique_entities"], 4);
        // relationship_types: ["loves"]
        let rels = stats["relationship_types"].as_array().unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0], "loves");
    }

    #[test]
    fn test_query_as_of_excludes_valid_until_instant() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple(
            "Alice",
            "works_at",
            "ACME",
            Some("2020-01-01"),
            Some("2024-01-01"),
            None,
        )
        .unwrap();
        let before = kg
            .query_entity("Alice", Some("2023-12-31"), "both")
            .unwrap();
        assert_eq!(before.as_array().unwrap().len(), 1);
        let at_end = kg
            .query_entity("Alice", Some("2024-01-01"), "both")
            .unwrap();
        assert_eq!(at_end.as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_supersede_closes_old_opens_new() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("Alice", "lives_in", "Boston", None, None, None)
            .unwrap();
        let result = kg
            .supersede("Alice", "lives_in", "Boston", "Austin", Some("2026-03-01"))
            .unwrap();
        assert_eq!(result["success"], true);
        assert_eq!(result["at"], "2026-03-01");
        let old_until: String = db
            .conn
            .query_row(
                "SELECT valid_until FROM triples WHERE object='Boston'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_until, "2026-03-01");
        let new_from: String = db
            .conn
            .query_row(
                "SELECT valid_from FROM triples WHERE object='Austin'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(new_from, "2026-03-01");
        let at_boundary = kg
            .query_entity("Alice", Some("2026-03-01"), "outgoing")
            .unwrap();
        let arr = at_boundary.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["object"], "Austin");
    }

    #[test]
    fn test_supersede_missing_old_errors() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        let err = kg
            .supersede("Alice", "lives_in", "Boston", "Austin", None)
            .unwrap_err();
        assert!(err.to_string().contains("FactNotFound"));
    }

    #[test]
    fn test_supersede_already_ended_errors() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("Alice", "lives_in", "Boston", None, None, None)
            .unwrap();
        kg.invalidate("Alice", "lives_in", "Boston", Some("2025-01-01"))
            .unwrap();
        let err = kg
            .supersede("Alice", "lives_in", "Boston", "Austin", None)
            .unwrap_err();
        assert!(err.to_string().contains("FactAlreadyEnded"));
    }

    #[test]
    fn test_add_triple_stores_source_file() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        let id = kg.add_triple("A", "p", "B", None, None, None).unwrap();
        kg.set_triple_provenance(&id, Some("/notes.md"), Some("drawer_1"))
            .unwrap();
        let (sf, sd): (String, String) = db
            .conn
            .query_row(
                "SELECT source_file, source_drawer_id FROM triples WHERE id=?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(sf, "/notes.md");
        assert_eq!(sd, "drawer_1");
    }

    #[test]
    fn test_supersede_same_object_rejected() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("Alice", "lives_in", "Boston", None, None, None)
            .unwrap();
        let err = kg
            .supersede("Alice", "lives_in", "Boston", "Boston", None)
            .unwrap_err();
        assert!(err.to_string().contains("must differ"));
    }

    #[test]
    fn test_query_as_of_at_boundary_returns_only_successor() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple(
            "Alice",
            "lives_in",
            "Boston",
            Some("2020-01-01"),
            Some("2026-06-01"),
            None,
        )
        .unwrap();
        kg.add_triple(
            "Alice",
            "lives_in",
            "Austin",
            Some("2026-06-01"),
            None,
            None,
        )
        .unwrap();
        let facts = kg
            .query_entity("Alice", Some("2026-06-01"), "outgoing")
            .unwrap();
        let arr = facts.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["object"], "Austin");
    }

    #[test]
    fn test_query_as_of_day_before_returns_predecessor() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple(
            "Alice",
            "lives_in",
            "Boston",
            Some("2020-01-01"),
            Some("2026-06-01"),
            None,
        )
        .unwrap();
        kg.add_triple(
            "Alice",
            "lives_in",
            "Austin",
            Some("2026-06-01"),
            None,
            None,
        )
        .unwrap();
        let facts = kg
            .query_entity("Alice", Some("2026-05-31"), "outgoing")
            .unwrap();
        let arr = facts.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["object"], "Boston");
    }
}
