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
