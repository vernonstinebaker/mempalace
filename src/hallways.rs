use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use crate::db::Database;

const ENTITY_STOP: &[&str] = &[
    "a", "an", "the", "tmp", "and", "or", "for", "to", "of", "in", "on", "at", "by", "is", "it",
    "this", "that", "with", "from",
];

/// Extract structural entities: URLs, paths, qualified identifiers, CamelCase.
pub fn extract_entities(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in content.split_whitespace() {
        let t = token.trim_matches(|c: char| {
            matches!(
                c,
                ',' | '.' | ';' | ':' | ')' | '(' | '[' | ']' | '"' | '\''
            )
        });
        if t.is_empty() {
            continue;
        }
        if let Some(url) = extract_url(t) {
            push_entity(&mut out, &url);
            continue;
        }
        if looks_like_path(t) {
            push_entity(&mut out, t);
            continue;
        }
        if looks_like_qualified(t) {
            push_entity(&mut out, t);
            continue;
        }
        if looks_like_camel(t) {
            push_entity(&mut out, t);
        }
    }
    out.sort();
    out.dedup();
    out
}

fn extract_url(token: &str) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        Some(token.to_string())
    } else {
        None
    }
}

fn looks_like_path(token: &str) -> bool {
    if token.len() < 4 {
        return false;
    }
    token.starts_with('/')
        || token.starts_with("src/")
        || token.starts_with("./")
        || (token.contains('/') && token.contains('.'))
}

fn looks_like_qualified(token: &str) -> bool {
    (token.contains("::") || (token.contains('.') && token.chars().any(|c| c.is_ascii_uppercase())))
        && token.len() >= 4
}

fn looks_like_camel(token: &str) -> bool {
    if token.len() < 4 {
        return false;
    }
    let mut has_upper = false;
    let mut has_lower = false;
    for c in token.chars() {
        if !c.is_ascii_alphanumeric() {
            return false;
        }
        if c.is_ascii_uppercase() {
            has_upper = true;
        }
        if c.is_ascii_lowercase() {
            has_lower = true;
        }
    }
    has_upper && has_lower
}

fn push_entity(out: &mut Vec<String>, raw: &str) {
    let mut e: String = raw.chars().take(64).collect();
    e.make_ascii_lowercase();
    if e.len() < 3 {
        return;
    }
    if ENTITY_STOP.contains(&e.as_str()) {
        return;
    }
    out.push(e);
}

pub fn record_hallways(db: &Database, wing: &str, room: &str, content: &str) -> Result<()> {
    let entities = extract_entities(content);
    if entities.len() < 2 {
        maybe_auto_tunnels(db, wing, room, &entities)?;
        return Ok(());
    }
    for i in 0..entities.len() {
        for j in (i + 1)..entities.len() {
            let (a, b) = if entities[i] < entities[j] {
                (entities[i].as_str(), entities[j].as_str())
            } else {
                (entities[j].as_str(), entities[i].as_str())
            };
            upsert_pair(db, wing, room, a, b)?;
        }
    }
    maybe_auto_tunnels(db, wing, room, &entities)?;
    Ok(())
}

fn upsert_pair(db: &Database, wing: &str, room: &str, a: &str, b: &str) -> Result<()> {
    let id = format!("hall_{}_{}_{}", wing, a, b);
    let existing: Option<String> = db
        .conn
        .query_row(
            "SELECT rooms FROM hallways WHERE wing=?1 AND entity_a=?2 AND entity_b=?3",
            params![wing, a, b],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(rooms_json) = existing {
        let mut rooms: Vec<String> = serde_json::from_str(&rooms_json).unwrap_or_default();
        if !rooms.iter().any(|r| r == room) {
            rooms.push(room.to_string());
        }
        let rooms_s = serde_json::to_string(&rooms).unwrap_or_else(|_| "[]".into());
        db.conn.execute(
            "UPDATE hallways SET co_occurrence_count = co_occurrence_count + 1, rooms=?1, updated_at=datetime('now')
             WHERE wing=?2 AND entity_a=?3 AND entity_b=?4",
            params![rooms_s, wing, a, b],
        )?;
    } else {
        let rooms_s = serde_json::to_string(&vec![room]).unwrap_or_else(|_| "[]".into());
        db.conn.execute(
            "INSERT INTO hallways (id, wing, entity_a, entity_b, co_occurrence_count, rooms)
             VALUES (?1, ?2, ?3, ?4, 1, ?5)",
            params![id, wing, a, b, rooms_s],
        )?;
    }
    Ok(())
}

/// If this entity also appears in another wing's hallways, link the top rooms.
fn maybe_auto_tunnels(db: &Database, wing: &str, room: &str, entities: &[String]) -> Result<()> {
    for ent in entities {
        let other: Option<(String, String)> = db
            .conn
            .query_row(
                "SELECT wing, rooms FROM hallways
                 WHERE (entity_a=?1 OR entity_b=?1) AND wing != ?2
                 ORDER BY co_occurrence_count DESC LIMIT 1",
                params![ent, wing],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((other_wing, rooms_json)) = other {
            let rooms: Vec<String> = serde_json::from_str(&rooms_json).unwrap_or_default();
            let other_room = rooms.first().map(|s| s.as_str()).unwrap_or("general");
            let _ = db.create_tunnel(
                wing,
                room,
                &other_wing,
                other_room,
                &format!("entity:{ent}"),
            );
        }
    }
    Ok(())
}

pub fn list_hallways(db: &Database, wing: Option<&str>) -> Result<Value> {
    let sql = match wing {
        Some(_) => {
            "SELECT id, wing, entity_a, entity_b, co_occurrence_count, rooms FROM hallways WHERE wing=?1 ORDER BY co_occurrence_count DESC"
        }
        None => {
            "SELECT id, wing, entity_a, entity_b, co_occurrence_count, rooms FROM hallways ORDER BY co_occurrence_count DESC"
        }
    };
    let mut stmt = db.conn.prepare(sql)?;
    let rows: Vec<Value> = match wing {
        Some(w) => stmt
            .query_map(params![w], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "wing": r.get::<_, String>(1)?,
                    "entity_a": r.get::<_, String>(2)?,
                    "entity_b": r.get::<_, String>(3)?,
                    "co_occurrence_count": r.get::<_, i64>(4)?,
                    "rooms": r.get::<_, Option<String>>(5)?,
                }))
            })?
            .filter_map(|r| r.ok())
            .collect(),
        None => stmt
            .query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "wing": r.get::<_, String>(1)?,
                    "entity_a": r.get::<_, String>(2)?,
                    "entity_b": r.get::<_, String>(3)?,
                    "co_occurrence_count": r.get::<_, i64>(4)?,
                    "rooms": r.get::<_, Option<String>>(5)?,
                }))
            })?
            .filter_map(|r| r.ok())
            .collect(),
    };
    Ok(json!({"hallways": rows, "count": rows.len()}))
}

pub fn delete_hallway(db: &Database, id: &str) -> Result<Value> {
    let n = db
        .conn
        .execute("DELETE FROM hallways WHERE id=?1", params![id])?;
    if n == 0 {
        return Err(anyhow::anyhow!("HallwayNotFound"));
    }
    Ok(json!({"success": true, "deleted": id}))
}

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
    fn test_extract_urls() {
        let e = extract_entities("see https://example.com/a for docs");
        assert!(e.iter().any(|x| x.contains("example.com")));
    }

    #[test]
    fn test_extract_paths() {
        let e = extract_entities("edit src/db.rs please");
        assert!(e
            .iter()
            .any(|x| x.contains("src/db.rs") || x.contains("src/db")));
    }

    #[test]
    fn test_extract_qualified_idents() {
        let e = extract_entities("call foo::bar and com.example.Foo");
        assert!(e
            .iter()
            .any(|x| x.contains("foo::bar") || x.contains("foo::")));
        assert!(e.iter().any(|x| x.contains("com.example.foo")));
    }

    #[test]
    fn test_extract_ignores_short_noise() {
        let e = extract_entities("a the tmp is it");
        assert!(e.is_empty());
    }

    #[test]
    fn test_hallway_created_on_add_drawer() {
        let (_dir, db) = test_db();
        db.add_drawer(
            "code",
            "notes",
            "https://ex.com/a src/db.rs",
            None,
            "test",
            None,
        )
        .unwrap();
        let list = list_hallways(&db, Some("code")).unwrap();
        assert!(list["count"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn test_hallway_increments_on_second_drawer() {
        let (_dir, db) = test_db();
        db.add_drawer("code", "r1", "https://ex.com/a src/db.rs", None, "t", None)
            .unwrap();
        db.add_drawer("code", "r2", "https://ex.com/a src/db.rs", None, "t", None)
            .unwrap();
        let list = list_hallways(&db, Some("code")).unwrap();
        let arr = list["hallways"].as_array().unwrap();
        assert!(arr[0]["co_occurrence_count"].as_i64().unwrap() >= 2);
    }

    #[test]
    fn test_hallway_pair_is_canonical_order() {
        let (_dir, db) = test_db();
        db.add_drawer("code", "r", "https://z.com src/a.rs", None, "t", None)
            .unwrap();
        let list = list_hallways(&db, None).unwrap();
        let h = &list["hallways"].as_array().unwrap()[0];
        let a = h["entity_a"].as_str().unwrap();
        let b = h["entity_b"].as_str().unwrap();
        assert!(a <= b);
    }

    #[test]
    fn test_list_hallways_filter_wing() {
        let (_dir, db) = test_db();
        db.add_drawer("wa", "r", "https://a.com src/a.rs", None, "t", None)
            .unwrap();
        db.add_drawer("wb", "r", "https://b.com src/b.rs", None, "t", None)
            .unwrap();
        let list = list_hallways(&db, Some("wa")).unwrap();
        for h in list["hallways"].as_array().unwrap() {
            assert_eq!(h["wing"], "wa");
        }
    }

    #[test]
    fn test_delete_hallway() {
        let (_dir, db) = test_db();
        db.add_drawer("code", "r", "https://a.com src/a.rs", None, "t", None)
            .unwrap();
        let list = list_hallways(&db, None).unwrap();
        let id = list["hallways"][0]["id"].as_str().unwrap();
        delete_hallway(&db, id).unwrap();
        let list2 = list_hallways(&db, None).unwrap();
        assert_eq!(list2["count"], 0);
    }

    #[test]
    fn test_auto_tunnel_when_entity_in_two_wings() {
        let (_dir, db) = test_db();
        db.add_drawer("wa", "ra", "https://shared.com src/a.rs", None, "t", None)
            .unwrap();
        db.add_drawer("wb", "rb", "https://shared.com src/b.rs", None, "t", None)
            .unwrap();
        let tunnels = db.list_tunnels(None).unwrap();
        assert!(tunnels["count"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn test_auto_tunnel_idempotent() {
        let (_dir, db) = test_db();
        db.add_drawer("wa", "ra", "https://shared.com src/a.rs", None, "t", None)
            .unwrap();
        db.add_drawer("wb", "rb", "https://shared.com src/b.rs", None, "t", None)
            .unwrap();
        db.add_drawer(
            "wb",
            "rb",
            "https://shared.com extra FooBar",
            None,
            "t",
            None,
        )
        .unwrap();
        let tunnels = db.list_tunnels(None).unwrap();
        assert_eq!(tunnels["count"].as_u64().unwrap(), 1);
    }

    #[test]
    fn test_auto_tunnel_not_created_for_single_wing() {
        let (_dir, db) = test_db();
        db.add_drawer("wa", "ra", "https://only.com src/a.rs", None, "t", None)
            .unwrap();
        let tunnels = db.list_tunnels(None).unwrap();
        assert_eq!(tunnels["count"], 0);
    }
}
