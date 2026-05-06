use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use crate::db::Database;

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
                "SELECT subject, predicate, object, valid_from, valid_until, source_closet
                 FROM triples WHERE subject = ?1
                 AND (?2 IS NULL OR (valid_from IS NULL OR valid_from <= ?2)
                     AND (valid_until IS NULL OR valid_until >= ?2))"
                    .to_string()
            }
            "incoming" => {
                "SELECT subject, predicate, object, valid_from, valid_until, source_closet
                 FROM triples WHERE object = ?1
                 AND (?2 IS NULL OR (valid_from IS NULL OR valid_from <= ?2)
                     AND (valid_until IS NULL OR valid_until >= ?2))"
                    .to_string()
            }
            _ => "SELECT subject, predicate, object, valid_from, valid_until, source_closet
                 FROM triples WHERE (subject = ?1 OR object = ?1)
                 AND (?2 IS NULL OR (valid_from IS NULL OR valid_from <= ?2)
                     AND (valid_until IS NULL OR valid_until >= ?2))"
                .to_string(),
        };

        let mut stmt = self.db.conn.prepare(&sql)?;
        let mut facts = Vec::new();

        let rows: Vec<(
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = stmt
            .query_map(params![entity, as_of], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();

        for (subject, predicate, object, valid_from, valid_until, source_closet) in rows {
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
        source_closet: Option<&str>,
    ) -> Result<String> {
        // Idempotency: return existing active triple if it exists
        let existing: Option<String> = self
            .db
            .conn
            .query_row(
                "SELECT id FROM triples
                 WHERE subject=?1 AND predicate=?2 AND object=?3 AND valid_until IS NULL",
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
            "INSERT INTO triples (id, subject, predicate, object, valid_from, valid_until, source_closet)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
            params![triple_id, subject, predicate, object, valid_from, source_closet],
        )?;

        Ok(triple_id)
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
                "SELECT subject, predicate, object, valid_from, valid_until, source_closet
                 FROM triples WHERE subject = ?1 OR object = ?1
                 ORDER BY COALESCE(valid_from, '0000-00-00')",
            )?;
            let rows: Vec<(
                String,
                String,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
            )> = stmt
                .query_map(params![e], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                })
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
                .unwrap_or_default();

            for (subject, predicate, object, valid_from, valid_until, source_closet) in rows {
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
                facts.push(fact);
            }
        } else {
            let mut stmt = self.db.conn.prepare(
                "SELECT subject, predicate, object, valid_from, valid_until, source_closet
                 FROM triples ORDER BY COALESCE(valid_from, '0000-00-00') LIMIT 100",
            )?;
            let rows: Vec<(
                String,
                String,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
            )> = stmt
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                })
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
                .unwrap_or_default();

            for (subject, predicate, object, valid_from, valid_until, source_closet) in rows {
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
                facts.push(fact);
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
            .add_triple("Alice", "loves", "chess", None, None)
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
            .add_triple("Alice", "loves", "chess", None, None)
            .unwrap();
        let id2 = kg
            .add_triple("Alice", "loves", "chess", None, None)
            .unwrap();
        assert_eq!(id1, id2); // Same active triple returns same ID
    }

    #[test]
    fn test_add_triple_with_valid_from() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        let id = kg
            .add_triple("Max", "started_school", "Year 7", Some("2026-09-01"), None)
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
            .add_triple("X", "relates_to", "Y", None, Some("closet_42"))
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
        kg.add_triple("Alice", "child_of", "Bob", None, None)
            .unwrap();
        kg.add_triple("Bob", "child_of", "Charlie", None, None)
            .unwrap();
        let facts = kg
            .query_entity("Alice", None, "outgoing")
            .unwrap();
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
        kg.add_triple("Alice", "child_of", "Bob", None, None)
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
        kg.add_triple("Alice", "child_of", "Bob", None, None)
            .unwrap();
        kg.add_triple("Bob", "child_of", "Charlie", None, None)
            .unwrap();
        let facts = kg.query_entity("Bob", None, "both").unwrap();
        let arr = facts.as_array().unwrap();
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn test_query_as_of() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("Alice", "works_at", "ACME", Some("2020-01-01"), None)
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
        kg.add_triple("Alice", "works_at", "ACME", Some("2020-01-01"), None)
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
        kg.add_triple("Alice", "works_at", "ACME", None, None)
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
        kg.add_triple("Alice", "born", "1990", Some("1990-01-01"), None)
            .unwrap();
        kg.add_triple("Alice", "graduated", "2012", Some("2012-06-01"), None)
            .unwrap();
        let timeline = kg.get_timeline(Some("Alice")).unwrap();
        let arr = timeline.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        // Should be ordered by valid_from
        assert_eq!(arr[0]["valid_from"], "1990-01-01");
        assert_eq!(arr[1]["valid_from"], "2012-06-01");
    }

    #[test]
    fn test_timeline_all() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("X", "a", "Y", None, None).unwrap();
        let timeline = kg.get_timeline(None).unwrap();
        let arr = timeline.as_array().unwrap();
        assert!(!arr.is_empty());
    }

    #[test]
    fn test_stats() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("Alice", "loves", "chess", None, None)
            .unwrap();
        kg.add_triple("Bob", "loves", "go", None, None)
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
    fn test_stats_with_expired_facts() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("A", "p", "B", None, None).unwrap();
        // Invalidate it
        kg.invalidate("A", "p", "B", Some("2025-01-01"))
            .unwrap();
        let stats = kg.get_stats().unwrap();
        assert_eq!(stats["total_triples"], 1);
        assert_eq!(stats["current_facts"], 0);
        assert_eq!(stats["expired_facts"], 1);
    }
}
