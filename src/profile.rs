//! Derived profile and one-call session context. Pure composition over
//! existing KG triples, FTS search, recent drawers, and diary entries.

use anyhow::Result;
use rusqlite::params;
use serde_json::{json, Value};

use crate::db::Database;

pub fn profile(db: &Database, entity: &str, dynamic_limit: usize) -> Result<Value> {
    let dynamic_limit = dynamic_limit.clamp(1, 50);
    let static_facts = static_profile(db, entity)?;
    let dynamic = dynamic_profile(db, entity, dynamic_limit)?;
    Ok(json!({
        "success": true,
        "entity": entity,
        "static": static_facts,
        "dynamic": dynamic,
    }))
}

pub fn context(
    db: &Database,
    entity: &str,
    agent_name: Option<&str>,
    recent_limit: usize,
    diary_limit: usize,
) -> Result<Value> {
    let recent_limit = recent_limit.clamp(1, 50);
    let diary_limit = diary_limit.clamp(1, 50);
    let profile_val = profile(db, entity, 10)?;
    let recent = db.list_recent(recent_limit, None, None)?;
    let recent_drawers = match recent {
        Value::Array(a) => Value::Array(a),
        _ => json!([]),
    };
    let diary_tail = if let Some(name) = agent_name {
        let wing = format!("wing_{}", normalize_agent_name(name));
        let data = db.get_diary_entries(&wing, diary_limit)?;
        data.get("entries").cloned().unwrap_or(json!([]))
    } else {
        json!([])
    };
    Ok(json!({
        "success": true,
        "profile": profile_val,
        "recent_drawers": recent_drawers,
        "diary_tail": diary_tail,
    }))
}

fn static_profile(db: &Database, entity: &str) -> Result<Vec<Value>> {
    let mut stmt = db.conn.prepare(
        "SELECT predicate, object, valid_from FROM triples
         WHERE subject = ?1 AND valid_until IS NULL
         ORDER BY predicate ASC, object ASC",
    )?;
    let rows = stmt.query_map(params![entity], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows.filter_map(|r| r.ok()) {
        let (predicate, object, valid_from) = row;
        let mut fact = json!({
            "predicate": predicate,
            "object": object,
        });
        if let Some(vf) = valid_from {
            fact["valid_from"] = json!(vf);
        }
        out.push(fact);
    }
    Ok(out)
}

fn dynamic_profile(db: &Database, entity: &str, limit: usize) -> Result<Vec<Value>> {
    let results = db.search(entity, limit, 0, None, None, None, None, None, "recency")?;
    let arr = results
        .get("results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(arr
        .into_iter()
        .take(limit)
        .map(|hit| {
            let content = hit.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let snippet: String = content.chars().take(200).collect();
            json!({
                "id": hit.get("id"),
                "wing": hit.get("wing"),
                "room": hit.get("room"),
                "filed_at": hit.get("filed_at"),
                "snippet": snippet,
            })
        })
        .collect())
}

fn normalize_agent_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == ' ' {
                '_'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::knowledge_graph::KnowledgeGraph;
    use tempfile::TempDir;

    fn test_db() -> (TempDir, Database) {
        let dir = TempDir::new().unwrap();
        let db = Database::open(dir.path().to_str().unwrap()).unwrap();
        (dir, db)
    }

    #[test]
    fn test_profile_static_from_open_triples() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("user", "prefers", "dark mode", None, None, None)
            .unwrap();
        kg.add_triple("user", "uses", "vim", None, None, None)
            .unwrap();
        let p = profile(&db, "user", 10).unwrap();
        assert_eq!(p["success"], json!(true));
        let statics = p["static"].as_array().unwrap();
        assert_eq!(statics.len(), 2);
        let preds: Vec<&str> = statics
            .iter()
            .filter_map(|f| f["predicate"].as_str())
            .collect();
        assert_eq!(preds, vec!["prefers", "uses"]);
        assert_eq!(statics[0]["object"], "dark mode");
        assert_eq!(statics[1]["object"], "vim");
    }

    #[test]
    fn test_profile_excludes_closed_facts() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("user", "prefers", "dark mode", None, None, None)
            .unwrap();
        kg.add_triple("user", "uses", "vim", None, None, None)
            .unwrap();
        kg.invalidate("user", "uses", "vim", Some("2026-01-01"))
            .unwrap();
        let p = profile(&db, "user", 10).unwrap();
        let statics = p["static"].as_array().unwrap();
        assert_eq!(statics.len(), 1);
        assert_eq!(statics[0]["predicate"], "prefers");
    }

    #[test]
    fn test_profile_dynamic_from_recent_matching_drawers() {
        let (_dir, db) = test_db();
        db.add_drawer("w", "r", "unrelated gardening notes", None, "t", None)
            .unwrap();
        let older = db
            .add_drawer("w", "r", "alice likes tea", None, "t", None)
            .unwrap();
        let newer = db
            .add_drawer("w", "r", "alice switched to coffee", None, "t", None)
            .unwrap();
        db.set_authored_at(&older, "2020-01-01 00:00:00").unwrap();
        db.set_authored_at(&newer, "2025-01-01 00:00:00").unwrap();
        let p = profile(&db, "alice", 10).unwrap();
        let dynm = p["dynamic"].as_array().unwrap();
        assert_eq!(dynm.len(), 2);
        assert_eq!(dynm[0]["id"], newer);
        assert_eq!(dynm[1]["id"], older);
        assert!(dynm[0]["snippet"].as_str().unwrap().contains("coffee"));
    }

    #[test]
    fn test_profile_dynamic_respects_limit() {
        let (_dir, db) = test_db();
        db.add_drawer("w", "r1", "alice one", None, "t", None)
            .unwrap();
        db.add_drawer("w", "r2", "alice two", None, "t", None)
            .unwrap();
        db.add_drawer("w", "r3", "alice three", None, "t", None)
            .unwrap();
        let p = profile(&db, "alice", 1).unwrap();
        assert_eq!(p["dynamic"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_profile_unknown_entity_returns_success_empty() {
        let (_dir, db) = test_db();
        let p = profile(&db, "nobody", 10).unwrap();
        assert_eq!(p["success"], json!(true));
        assert_eq!(p["entity"], "nobody");
        assert!(p["static"].as_array().unwrap().is_empty());
        assert!(p["dynamic"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_profile_excludes_expired_drawers() {
        let (_dir, db) = test_db();
        let live = db
            .add_drawer("w", "r", "alice lives here", None, "t", None)
            .unwrap();
        let dead = db
            .add_drawer("w", "r", "alice expired note", None, "t", None)
            .unwrap();
        db.conn
            .execute(
                "UPDATE drawers SET expires_at = datetime('now', '-1 day') WHERE id = ?1",
                params![dead],
            )
            .unwrap();
        let p = profile(&db, "alice", 10).unwrap();
        let ids: Vec<&str> = p["dynamic"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|h| h["id"].as_str())
            .collect();
        assert!(ids.contains(&live.as_str()));
        assert!(!ids.contains(&dead.as_str()));
    }

    #[test]
    fn test_context_includes_profile_recent_and_diary() {
        let (_dir, db) = test_db();
        let kg = KnowledgeGraph::new(&db);
        kg.add_triple("user", "uses", "vim", None, None, None)
            .unwrap();
        db.add_drawer("w", "r", "recent note about work", None, "t", None)
            .unwrap();
        db.add_drawer(
            "wing_tester",
            "diary",
            "[general] session notes",
            None,
            "tester",
            None,
        )
        .unwrap();
        let ctx = context(&db, "user", Some("tester"), 5, 3).unwrap();
        assert_eq!(ctx["success"], json!(true));
        assert_eq!(ctx["profile"]["static"].as_array().unwrap().len(), 1);
        assert!(!ctx["recent_drawers"].as_array().unwrap().is_empty());
        assert_eq!(ctx["diary_tail"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_context_respects_limits() {
        let (_dir, db) = test_db();
        for i in 0..6 {
            db.add_drawer("w", &format!("r{i}"), &format!("item {i}"), None, "t", None)
                .unwrap();
            db.add_drawer(
                "wing_bot",
                "diary",
                &format!("[t] diary {i}"),
                None,
                "bot",
                None,
            )
            .unwrap();
        }
        let ctx = context(&db, "user", Some("bot"), 2, 1).unwrap();
        assert_eq!(ctx["recent_drawers"].as_array().unwrap().len(), 2);
        assert_eq!(ctx["diary_tail"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_context_empty_palace_succeeds() {
        let (_dir, db) = test_db();
        let ctx = context(&db, "user", Some("ghost"), 5, 3).unwrap();
        assert_eq!(ctx["success"], json!(true));
        assert!(ctx["profile"]["static"].as_array().unwrap().is_empty());
        assert!(ctx["recent_drawers"].as_array().unwrap().is_empty());
        assert!(ctx["diary_tail"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_context_diary_absent_when_agent_unknown() {
        let (_dir, db) = test_db();
        db.add_drawer("w", "r", "something", None, "t", None)
            .unwrap();
        let ctx = context(&db, "user", Some("unknown_agent"), 5, 3).unwrap();
        assert!(ctx["diary_tail"].as_array().unwrap().is_empty());
        let ctx2 = context(&db, "user", None, 5, 3).unwrap();
        assert!(ctx2["diary_tail"].as_array().unwrap().is_empty());
    }
}
