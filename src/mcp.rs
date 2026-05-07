use std::io::{BufRead, Write};

use serde_json::{json, Value};

use crate::db::Database;
use crate::embed::Embedder;
use crate::import_sessions;
use crate::knowledge_graph::KnowledgeGraph;
use crate::validate;
use crate::wal;

// ── Protocol / AAAK strings ───────────────────────────────────────────────────

const PALACE_PROTOCOL: &str = "\
IMPORTANT — MemPalace Memory Protocol:
1. ON WAKE-UP: Call mempalace_status to load palace overview + AAAK spec.
2. BEFORE RESPONDING about any person, project, or past event: call mempalace_kg_query or mempalace_search FIRST. Never guess — verify.
3. IF UNSURE about a fact (name, gender, age, relationship): say \"let me check\" and query the palace. Wrong is worse than slow.
4. AFTER EACH SESSION: call mempalace_diary_write to record what happened, what you learned, what matters.
5. WHEN FACTS CHANGE: call mempalace_kg_invalidate on the old fact, mempalace_kg_add for the new one.

This protocol ensures the AI KNOWS before it speaks. Storage is not memory — but storage + this protocol = memory.";

const AAAK_SPEC: &str = "\
AAAK is a compressed memory dialect that MemPalace uses for efficient storage.
It is designed to be readable by both humans and LLMs without decoding.

FORMAT:
  ENTITIES: 3-letter uppercase codes. ALC=Alice, JOR=Jordan, RIL=Riley, MAX=Max, BEN=Ben.
  EMOTIONS: *action markers* before/during text. *warm*=joy, *fierce*=determined, *raw*=vulnerable, *bloom*=tenderness.
  STRUCTURE: Pipe-separated fields. FAM: family | PROJ: projects | ⚠: warnings/reminders.
  DATES: ISO format (2026-03-31). COUNTS: Nx = N mentions (e.g., 570x).
  IMPORTANCE: ★ to ★★★★★ (1-5 scale).
  HALLS: hall_facts, hall_events, hall_discoveries, hall_preferences, hall_advice.
  WINGS: wing_user, wing_agent, wing_team, wing_code, wing_myproject, wing_hardware, wing_ue5, wing_ai_research.
  ROOMS: Hyphenated slugs representing named ideas (e.g., chromadb-setup, gpu-pricing).

EXAMPLE:
  FAM: ALC→♡JOR | 2D(kids): RIL(18,sports) MAX(11,chess+swimming) | BEN(contributor)

Read AAAK naturally — expand codes mentally, treat *markers* as emotional context.
When WRITING AAAK: use entity codes, mark emotions, keep structure tight.";

// ── Tools JSON ────────────────────────────────────────────────────────────────

const TOOLS_JSON: &str = concat!(
    "[",
    r#"{"name":"mempalace_status","description":"Palace overview \u2014 total drawers, wing and room counts, vector health","inputSchema":{"type":"object","properties":{}}},"#,
    r#"{"name":"mempalace_list_wings","description":"List all wings with drawer counts","inputSchema":{"type":"object","properties":{}}},"#,
    r#"{"name":"mempalace_list_rooms","description":"List rooms within a wing (or all rooms if no wing given)","inputSchema":{"type":"object","properties":{"wing":{"type":"string","description":"Wing to list rooms for (optional)"}}}},"#,
    r#"{"name":"mempalace_get_taxonomy","description":"Full taxonomy: wing \u2192 room \u2192 drawer count","inputSchema":{"type":"object","properties":{}}},"#,
    r#"{"name":"mempalace_get_aaak_spec","description":"Get the AAAK dialect specification \u2014 the compressed memory format MemPalace uses. Call this if you need to read or write AAAK-compressed memories.","inputSchema":{"type":"object","properties":{}}},"#,
    r#"{"name":"mempalace_kg_query","description":"Query the knowledge graph for an entity's relationships. Returns typed facts with temporal validity. E.g. 'Max' \u2192 child_of Alice, loves chess, does swimming. Filter by date with as_of to see what was true at a point in time.","inputSchema":{"type":"object","properties":{"entity":{"type":"string","description":"Entity to query (e.g. 'Max', 'MyProject', 'Alice')"},"as_of":{"type":"string","description":"Date filter \u2014 only facts valid at this date (YYYY-MM-DD, optional)"},"direction":{"type":"string","description":"outgoing (entity\u2192?), incoming (?\u2192entity), or both (default: both)"}},"required":["entity"]}},"#,
    r#"{"name":"mempalace_kg_add","description":"Add a fact to the knowledge graph. Subject \u2192 predicate \u2192 object with optional time window. E.g. ('Max', 'started_school', 'Year 7', valid_from='2026-09-01'). valid_to sets an end date without needing a separate invalidate call.","inputSchema":{"type":"object","properties":{"subject":{"type":"string","description":"The entity doing/being something"},"predicate":{"type":"string","description":"The relationship type (e.g. 'loves', 'works_on', 'daughter_of')"},"object":{"type":"string","description":"The entity being connected to"},"valid_from":{"type":"string","description":"When this became true (YYYY-MM-DD, optional)"},"valid_to":{"type":"string","description":"When this stopped being true (YYYY-MM-DD, optional)"},"source_closet":{"type":"string","description":"Closet ID where this fact appears (optional)"}},"required":["subject","predicate","object"]}},"#,
    r#"{"name":"mempalace_kg_invalidate","description":"Mark a fact as no longer true. E.g. ankle injury resolved, job ended, moved house.","inputSchema":{"type":"object","properties":{"subject":{"type":"string","description":"Entity"},"predicate":{"type":"string","description":"Relationship"},"object":{"type":"string","description":"Connected entity"},"ended":{"type":"string","description":"When it stopped being true (YYYY-MM-DD, default: today)"}},"required":["subject","predicate","object"]}},"#,
    r#"{"name":"mempalace_kg_timeline","description":"Chronological timeline of facts. Shows the story of an entity (or everything) in order.","inputSchema":{"type":"object","properties":{"entity":{"type":"string","description":"Entity to get timeline for (optional \u2014 omit for full timeline)"}}}},"#,
    r#"{"name":"mempalace_kg_stats","description":"Knowledge graph overview: entities, triples, current vs expired facts, relationship types.","inputSchema":{"type":"object","properties":{}}},"#,
    r#"{"name":"mempalace_traverse","description":"Walk the palace graph from a room. Shows connected ideas across wings \u2014 the tunnels. Like following a thread through the palace: start at 'chromadb-setup' in wing_code, discover it connects to wing_myproject (planning) and wing_user (feelings about it).","inputSchema":{"type":"object","properties":{"start_room":{"type":"string","description":"Room to start from (e.g. 'chromadb-setup', 'riley-school')"},"max_hops":{"type":"integer","description":"How many connections to follow (default: 2)"}},"required":["start_room"]}},"#,
    r#"{"name":"mempalace_find_tunnels","description":"Find rooms that bridge two wings \u2014 the hallways connecting different domains. E.g. what topics connect wing_code to wing_team?","inputSchema":{"type":"object","properties":{"wing_a":{"type":"string","description":"First wing (optional)"},"wing_b":{"type":"string","description":"Second wing (optional)"}}}},"#,
    r#"{"name":"mempalace_graph_stats","description":"Palace graph overview: total rooms, tunnel connections, edges between wings.","inputSchema":{"type":"object","properties":{}}},"#,
    r#"{"name":"mempalace_search","description":"Semantic search with pagination. Returns results array, total count, limit, offset. Use sort_by=relevance (default), recency (newest first), or hybrid (relevance + recency boost).","inputSchema":{"type":"object","properties":{"query":{"type":"string","description":"What to search for"},"limit":{"type":"integer","description":"Max results (default 5)"},"offset":{"type":"integer","description":"Offset for pagination (default 0)"},"wing":{"type":"string","description":"Filter by wing (optional)"},"room":{"type":"string","description":"Filter by room (optional)"},"filed_after":{"type":"string","description":"Only results filed after this ISO datetime (optional)"},"filed_before":{"type":"string","description":"Only results filed before this ISO datetime (optional)"},"sort_by":{"type":"string","description":"Sort mode: relevance, recency, or hybrid (default: relevance)"}},"required":["query"]}},"#,
    r#"{"name":"mempalace_check_duplicate","description":"Check if content already exists in the palace before filing","inputSchema":{"type":"object","properties":{"content":{"type":"string","description":"Content to check"},"threshold":{"type":"number","description":"Similarity threshold 0-1 (default 0.9)"}},"required":["content"]}},"#,
    r#"{"name":"mempalace_add_drawer","description":"File verbatim content into the palace. Checks for duplicates first.","inputSchema":{"type":"object","properties":{"wing":{"type":"string","description":"Wing (project name)"},"room":{"type":"string","description":"Room (aspect: backend, decisions, meetings...)"},"content":{"type":"string","description":"Verbatim content to store \u2014 exact words, never summarized"},"source_file":{"type":"string","description":"Where this came from (optional)"},"added_by":{"type":"string","description":"Who is filing this (default: mcp)"}},"required":["wing","room","content"]}},"#,
    r#"{"name":"mempalace_delete_drawer","description":"Delete a drawer by ID. Irreversible.","inputSchema":{"type":"object","properties":{"drawer_id":{"type":"string","description":"ID of the drawer to delete"}},"required":["drawer_id"]}},"#,
    r#"{"name":"mempalace_update_drawer","description":"Update the content (and optionally wing/room) of an existing drawer by ID. Re-embeds and re-indexes automatically. Use this to correct facts, update paths, or revise stored text without deleting and re-adding.","inputSchema":{"type":"object","properties":{"drawer_id":{"type":"string","description":"ID of the drawer to update"},"content":{"type":"string","description":"New content to store"},"wing":{"type":"string","description":"New wing (optional — keeps existing if omitted)"},"room":{"type":"string","description":"New room (optional — keeps existing if omitted)"}},"required":["drawer_id","content"]}},"#,
    r#"{"name":"mempalace_bulk_replace","description":"Find-and-replace a string across ALL drawer content in the palace. Returns count of updated drawers. Useful for bulk corrections like renamed paths, people, or projects.","inputSchema":{"type":"object","properties":{"find":{"type":"string","description":"Exact string to find"},"replace":{"type":"string","description":"String to replace it with"},"wing":{"type":"string","description":"Limit to this wing only (optional)"}},"required":["find","replace"]}},"#,
    r#"{"name":"mempalace_diary_write","description":"Write to your personal agent diary in AAAK format. Your observations, thoughts, what you worked on, what matters. Each agent has their own diary with full history. Write in AAAK for compression \u2014 e.g. 'SESSION:2026-04-04|built.palace.graph+diary.tools|ALC.req:agent.diaries.in.aaak|\u2605\u2605\u2605'. Use entity codes from the AAAK spec.","inputSchema":{"type":"object","properties":{"agent_name":{"type":"string","description":"Your name \u2014 each agent gets their own diary wing"},"entry":{"type":"string","description":"Your diary entry in AAAK format \u2014 compressed, entity-coded, emotion-marked"},"topic":{"type":"string","description":"Topic tag (optional, default: general)"}},"required":["agent_name","entry"]}},"#,
    r#"{"name":"mempalace_diary_read","description":"Read your recent diary entries (in AAAK). See what past versions of yourself recorded \u2014 your journal across sessions.","inputSchema":{"type":"object","properties":{"agent_name":{"type":"string","description":"Your name \u2014 each agent gets their own diary wing"},"last_n":{"type":"integer","description":"Number of recent entries to read (default: 10)"}},"required":["agent_name"]}},"#,
    r#"{"name":"mempalace_import_sessions","description":"Import sessions from an opencode.db into the palace. Run this to sync recent session data into mempalace so it's searchable. Defaults to incremental (only new sessions). Use full=true to re-import all.","inputSchema":{"type":"object","properties":{"oc_db_path":{"type":"string","description":"Path to opencode.db (default: ~/.local/share/opencode/opencode.db)"},"full":{"type":"boolean","description":"Re-import all sessions instead of incremental (default: false)"}}}},"#,
    r#"{"name":"mempalace_list_recent","description":"List recently filed content, ordered by filed_at descending. Use this when you need to know what's new.","inputSchema":{"type":"object","properties":{"limit":{"type":"integer","description":"Max results (default 20)"},"wing":{"type":"string","description":"Filter by wing (optional)"},"since":{"type":"string","description":"Only entries filed after this ISO datetime"}}}},"#,
    r#"{"name":"mempalace_export","description":"Export drawers as JSON Lines. Filter by wing and/or room.","inputSchema":{"type":"object","properties":{"wing":{"type":"string","description":"Filter by wing (optional)"},"room":{"type":"string","description":"Filter by room (optional)"}}}},"#,
    r#"{"name":"mempalace_export_kg","description":"Export knowledge graph triples as JSON.","inputSchema":{"type":"object","properties":{}}},"#,
    r#"{"name":"mempalace_backup","description":"Backup the entire palace database by copying the DB file. Returns the backup path.","inputSchema":{"type":"object","properties":{"path":{"type":"string","description":"Destination path for the backup (default: ~/.local/share/mempalace/backups/<timestamp>.db)"}}}},"#,
    r#"{"name":"mempalace_restore","description":"Restore the palace from a backup file. WARNING: replaces current data.","inputSchema":{"type":"object","properties":{"path":{"type":"string","description":"Path to the backup .db file to restore from"}},"required":["path"]}},"#,
    r#"{"name":"mempalace_repair","description":"Reindex all drawers (backfill missing embeddings). Use when vector health shows divergence or after importing many sessions.","inputSchema":{"type":"object","properties":{}}},"#,
    r#"{"name":"mempalace_reconnect","description":"Rebuild FTS index and re-probe vector health. Use after external writes or when search returns unexpected results.","inputSchema":{"type":"object","properties":{}}},"#,
    r#"{"name":"mempalace_wal_log","description":"Read write-ahead log entries (audit trail). Returns last N entries newest-first.","inputSchema":{"type":"object","properties":{"limit":{"type":"integer","description":"Max entries to return (default 20)"}}}},"#,
    r#"{"name":"mempalace_create_tunnel","description":"Create an explicit cross-wing tunnel linking ideas in different wings.","inputSchema":{"type":"object","properties":{"source_wing":{"type":"string","description":"Source wing"},"source_room":{"type":"string","description":"Source room"},"target_wing":{"type":"string","description":"Target wing"},"target_room":{"type":"string","description":"Target room"},"label":{"type":"string","description":"Optional label for this tunnel"}},"required":["source_wing","source_room","target_wing","target_room"]}},"#,
    r#"{"name":"mempalace_list_tunnels","description":"List explicit cross-wing tunnels, optionally filtered by wing.","inputSchema":{"type":"object","properties":{"wing":{"type":"string","description":"Filter by wing (optional)"}}}},"#,
    r#"{"name":"mempalace_delete_tunnel","description":"Delete an explicit cross-wing tunnel by ID.","inputSchema":{"type":"object","properties":{"tunnel_id":{"type":"string","description":"Tunnel ID to delete"}},"required":["tunnel_id"]}},"#,
    r#"{"name":"mempalace_follow_tunnels","description":"Follow explicit tunnels from a wing/room to see connected ideas in other wings.","inputSchema":{"type":"object","properties":{"wing":{"type":"string","description":"Wing name"},"room":{"type":"string","description":"Room name"}},"required":["wing","room"]}},"#,
    r#"{"name":"mempalace_get_drawer","description":"Fetch a single drawer by ID with full content and metadata.","inputSchema":{"type":"object","properties":{"drawer_id":{"type":"string","description":"ID of the drawer to fetch"}},"required":["drawer_id"]}},"#,
    r#"{"name":"mempalace_list_drawers","description":"Paginated drawer listing with wing/room filters. Returns content previews (first 200 chars) with total count.","inputSchema":{"type":"object","properties":{"wing":{"type":"string","description":"Filter by wing (optional)"},"room":{"type":"string","description":"Filter by room (optional)"},"limit":{"type":"integer","description":"Max results (default 20, max 100)"},"offset":{"type":"integer","description":"Offset for pagination (default 0)"}}}},"#,
    r#"{"name":"mempalace_integrity","description":"Run integrity checks across all indices (FTS, vectors, triples, tunnels). Returns health report with any issues found.","inputSchema":{"type":"object","properties":{}}}"#,
    "]"
);

// ── Server ────────────────────────────────────────────────────────────────────

pub struct Server<'a> {
    db: &'a Database,
    embedder: Option<Embedder>,
}

impl<'a> Server<'a> {
    pub fn new(db: &'a Database, embedder: Option<Embedder>) -> Self {
        Self { db, embedder }
    }

    pub fn run_stdio(&self) {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let mut out = std::io::BufWriter::new(stdout.lock());

        for line in stdin.lock().lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if let Some(response) = self.handle_message(trimmed) {
                let _ = out.write_all(response.as_bytes());
                let _ = out.write_all(b"\n");
                let _ = out.flush();
            }
        }
    }

    fn handle_message(&self, line: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return Some(
                r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}"#
                    .to_string(),
            ),
        };

        if !msg.is_object() {
            return None;
        }

        // Notifications have no "id" — do not respond
        let id_val = msg.get("id")?;
        let id_str = json_value_to_id_str(id_val);

        let method = msg.get("method")?.as_str()?;

        match method {
            "initialize" => Some(format!(
                r#"{{"jsonrpc":"2.0","id":{id_str},"result":{{"protocolVersion":"2024-11-05","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"mempalace","version":"3.0.0"}}}}}}"#
            )),
            "initialized" | "notifications/initialized" => None,
            "tools/list" => Some(format!(
                r#"{{"jsonrpc":"2.0","id":{id_str},"result":{{"tools":{TOOLS_JSON}}}}}"#
            )),
            "tools/call" => {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                let result = self.handle_tool_call(&params);
                let is_error = result
                    .get("isError")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let _ = is_error; // field already embedded in result
                let text = match result.get("text") {
                    Some(t) => t.as_str().unwrap_or("").to_string(),
                    None => serde_json::to_string(&result).unwrap_or_default(),
                };
                let text_escaped = serde_json::to_string(&text).unwrap_or_default();
                let is_err_field = if result
                    .get("isError")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    r#","isError":true"#
                } else {
                    ""
                };
                Some(format!(
                    r#"{{"jsonrpc":"2.0","id":{id_str},"result":{{"content":[{{"type":"text","text":{text_escaped}}}]{is_err_field}}}}}"#
                ))
            }
            _ => Some(format!(
                r#"{{"jsonrpc":"2.0","id":{id_str},"error":{{"code":-32601,"message":"Method not found"}}}}"#
            )),
        }
    }

    fn handle_tool_call(&self, params: &Value) -> Value {
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or(Value::Object(serde_json::Map::new()));

        match self.execute_tool(name, &args) {
            Ok(result_str) => json!({"text": result_str}),
            Err(e) => json!({"text": e.to_string(), "isError": true}),
        }
    }

    fn execute_tool(&self, name: &str, args: &Value) -> anyhow::Result<String> {
        let kg = KnowledgeGraph::new(self.db);

        match name {
            // ── mempalace_status ─────────────────────────────────────────────
            "mempalace_status" => {
                let count = self.db.get_drawer_count();
                let wings = self.db.get_wing_counts()?;
                let rooms = self.db.get_room_counts(None)?;
                let health = self.db.vec0_health();
                let mut result = json!({
                    "total_drawers": count,
                    "wings": wings,
                    "rooms": rooms,
                    "protocol": PALACE_PROTOCOL,
                    "aaak_dialect": AAAK_SPEC,
                    "vector_health": health,
                });
                if self.db.vector_disabled {
                    result["vector_disabled"] = json!(true);
                    result["hint"] = json!("Run mempalace_repair to re-index and restore vector search");
                }
                Ok(serde_json::to_string(&result)?)
            }

            // ── mempalace_list_wings ─────────────────────────────────────────
            "mempalace_list_wings" => {
                let wings = self.db.get_wing_counts()?;
                Ok(serde_json::to_string(&json!({"wings": wings}))?)
            }

            // ── mempalace_list_rooms ─────────────────────────────────────────
            "mempalace_list_rooms" => {
                let wing_filter = get_str(args, "wing");
                let rooms = self.db.get_room_counts(wing_filter)?;
                Ok(serde_json::to_string(&json!({
                    "wing": wing_filter.unwrap_or("all"),
                    "rooms": rooms,
                }))?)
            }

            // ── mempalace_get_taxonomy ────────────────────────────────────────
            "mempalace_get_taxonomy" => {
                let taxonomy = self.db.get_taxonomy()?;
                Ok(serde_json::to_string(&json!({"taxonomy": taxonomy}))?)
            }

            // ── mempalace_get_aaak_spec ───────────────────────────────────────
            "mempalace_get_aaak_spec" => {
                Ok(serde_json::to_string(&json!({"aaak_spec": AAAK_SPEC}))?)
            }

            // ── mempalace_kg_query ────────────────────────────────────────────
            "mempalace_kg_query" => {
                let entity = get_str(args, "entity")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: entity"))?;
                let as_of = validate::sanitize_iso_date(get_str(args, "as_of"))?;
                let direction = get_str(args, "direction").unwrap_or("both");
                let facts = kg.query_entity(entity, as_of, direction)?;
                let count = facts.as_array().map(|a| a.len()).unwrap_or(0);
                let mut result = json!({
                    "entity": entity,
                    "facts": facts,
                    "count": count,
                });
                if let Some(d) = as_of {
                    result["as_of"] = json!(d);
                }
                Ok(serde_json::to_string(&result)?)
            }

            // ── mempalace_kg_add ──────────────────────────────────────────────
            "mempalace_kg_add" => {
                let subject = get_str(args, "subject")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: subject"))?;
                let predicate = get_str(args, "predicate")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: predicate"))?;
                let object = get_str(args, "object")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: object"))?;
                let valid_from = validate::sanitize_iso_date(get_str(args, "valid_from"))?;
                let valid_to = validate::sanitize_iso_date(get_str(args, "valid_to"))?;
                let source_closet = get_str(args, "source_closet");
                let triple_id =
                    kg.add_triple(subject, predicate, object, valid_from, valid_to, source_closet)?;
                wal::log_write("kg_add", json!({
                    "subject": subject, "predicate": predicate, "object": object,
                    "triple_id": triple_id,
                }));
                let fact_str = format!("{subject} \u{2192} {predicate} \u{2192} {object}");
                Ok(serde_json::to_string(&json!({
                    "success": true,
                    "triple_id": triple_id,
                    "fact": fact_str,
                }))?)
            }

            // ── mempalace_kg_invalidate ───────────────────────────────────────
            "mempalace_kg_invalidate" => {
                let subject = get_str(args, "subject")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: subject"))?;
                let predicate = get_str(args, "predicate")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: predicate"))?;
                let object = get_str(args, "object")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: object"))?;
                let ended = validate::sanitize_iso_date(get_str(args, "ended"))?;
                kg.invalidate(subject, predicate, object, ended)?;
                wal::log_write("kg_invalidate", json!({
                    "subject": subject, "predicate": predicate, "object": object, "ended": ended,
                }));
                let fact_str = format!("{subject} \u{2192} {predicate} \u{2192} {object}");
                Ok(serde_json::to_string(&json!({
                    "success": true,
                    "fact": fact_str,
                    "ended": ended.unwrap_or("today"),
                }))?)
            }

            // ── mempalace_kg_timeline ─────────────────────────────────────────
            "mempalace_kg_timeline" => {
                let entity = get_str(args, "entity");
                let timeline = kg.get_timeline(entity)?;
                let count = timeline.as_array().map(|a| a.len()).unwrap_or(0);
                Ok(serde_json::to_string(&json!({
                    "entity": entity.unwrap_or("all"),
                    "timeline": timeline,
                    "count": count,
                }))?)
            }

            // ── mempalace_kg_stats ────────────────────────────────────────────
            "mempalace_kg_stats" => {
                let stats = kg.get_stats()?;
                Ok(serde_json::to_string(&stats)?)
            }

            // ── mempalace_traverse ────────────────────────────────────────────
            "mempalace_traverse" => {
                let start_room = get_str(args, "start_room")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: start_room"))?;
                let max_hops = get_i64(args, "max_hops").unwrap_or(2) as usize;
                let result = self.db.traverse(start_room, max_hops)?;
                Ok(serde_json::to_string(&result)?)
            }

            // ── mempalace_find_tunnels ────────────────────────────────────────
            "mempalace_find_tunnels" => {
                let wing_a = get_str(args, "wing_a");
                let wing_b = get_str(args, "wing_b");
                let result = self.db.find_tunnels(wing_a, wing_b)?;
                Ok(serde_json::to_string(&result)?)
            }

            // ── mempalace_graph_stats ─────────────────────────────────────────
            "mempalace_graph_stats" => {
                let result = self.db.graph_stats()?;
                Ok(serde_json::to_string(&result)?)
            }

            // ── mempalace_search ──────────────────────────────────────────────
            "mempalace_search" => {
                let query = get_str(args, "query")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: query"))?;
                let limit = get_i64(args, "limit").unwrap_or(5) as usize;
                let offset = get_i64(args, "offset").unwrap_or(0) as usize;
                let wing = validate::sanitize_name(get_str(args, "wing"), "wing")?;
                let room = validate::sanitize_name(get_str(args, "room"), "room")?;
                let filed_after = validate::sanitize_iso_date(get_str(args, "filed_after"))?;
                let filed_before = validate::sanitize_iso_date(get_str(args, "filed_before"))?;
                let sort_by = get_str(args, "sort_by").unwrap_or("relevance");
                let results = self.db.search(
                    query, limit, offset, wing, room, filed_after, filed_before,
                    self.embedder.as_ref(), sort_by,
                )?;
                Ok(serde_json::to_string(&results)?)
            }

            // ── mempalace_check_duplicate ─────────────────────────────────────
            "mempalace_check_duplicate" => {
                let content = get_str(args, "content")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: content"))?;
                let threshold = get_f64(args, "threshold").unwrap_or(0.9);
                let result = self
                    .db
                    .check_duplicate(content, threshold, self.embedder.as_ref())?;
                Ok(serde_json::to_string(&result)?)
            }

            // ── mempalace_add_drawer ──────────────────────────────────────────
            "mempalace_add_drawer" => {
                let wing = validate::sanitize_name_required(
                    get_str(args, "wing")
                        .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: wing"))?,
                    "wing",
                )?;
                let room = validate::sanitize_name_required(
                    get_str(args, "room")
                        .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: room"))?,
                    "room",
                )?;
                let content_raw = get_str(args, "content")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: content"))?;
                let content = validate::sanitize_content(content_raw)?;
                let source_file = get_str(args, "source_file");
                let added_by = get_str(args, "added_by").unwrap_or("mcp");

                // Duplicate check at threshold 0.9
                let dup = self
                    .db
                    .check_duplicate(content, 0.9, self.embedder.as_ref())?;
                let is_dup = dup
                    .get("is_duplicate")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if is_dup {
                    let matches = dup.get("matches").cloned().unwrap_or(json!([]));
                    return Ok(serde_json::to_string(&json!({
                        "success": false,
                        "error": "Duplicate content detected",
                        "matches": matches,
                    }))?);
                }

                let drawer_id = self.db.add_drawer(
                    wing,
                    room,
                    content,
                    source_file,
                    added_by,
                    self.embedder.as_ref(),
                )?;
                wal::log_write("add_drawer", json!({
                    "wing": wing, "room": room, "content": content,
                    "drawer_id": drawer_id, "added_by": added_by,
                }));
                Ok(serde_json::to_string(&json!({
                    "success": true,
                    "drawer_id": drawer_id,
                    "wing": wing,
                    "room": room,
                }))?)
            }

            // ── mempalace_delete_drawer ───────────────────────────────────────
            "mempalace_delete_drawer" => {
                let drawer_id = get_str(args, "drawer_id")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: drawer_id"))?;
                match self.db.delete_drawer(drawer_id) {
                    Ok(()) => {
                        wal::log_write(
                            "delete_drawer",
                            json!({"drawer_id": drawer_id}),
                        );
                        Ok(serde_json::to_string(&json!({
                            "success": true,
                            "drawer_id": drawer_id,
                        }))?)
                    }
                    Err(e) if e.to_string().contains("DrawerNotFound") => {
                        Ok(serde_json::to_string(&json!({
                            "success": false,
                            "error": format!("Drawer not found: {drawer_id}"),
                        }))?)
                    }
                    Err(e) => Err(e),
                }
            }

            // ── mempalace_update_drawer ───────────────────────────────────────
            "mempalace_update_drawer" => {
                let drawer_id = get_str(args, "drawer_id")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: drawer_id"))?;
                let new_content = get_str(args, "content")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: content"))?;
                let new_wing = get_str(args, "wing");
                let new_room = get_str(args, "room");

                match self.db.update_drawer(
                    drawer_id,
                    new_content,
                    new_wing,
                    new_room,
                    self.embedder.as_ref(),
                ) {
                    Ok(()) => {
                        wal::log_write("update_drawer", json!({
                            "drawer_id": drawer_id, "wing": new_wing, "room": new_room,
                        }));
                        Ok(serde_json::to_string(&json!({
                            "success": true,
                            "drawer_id": drawer_id,
                        }))?)
                    }
                    Err(e) if e.to_string().contains("DrawerNotFound") => {
                        Ok(serde_json::to_string(&json!({
                            "success": false,
                            "error": format!("Drawer not found: {drawer_id}"),
                        }))?)
                    }
                    Err(e) => Err(e),
                }
            }

            // ── mempalace_bulk_replace ────────────────────────────────────────
            "mempalace_bulk_replace" => {
                let find = get_str(args, "find")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: find"))?;
                let replace = get_str(args, "replace")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: replace"))?;
                let wing = get_str(args, "wing");

                let count = self
                    .db
                    .bulk_replace(find, replace, wing, self.embedder.as_ref())?;
                wal::log_write("bulk_replace", json!({
                    "find": find, "replace": replace, "wing": wing, "updated": count,
                }));
                Ok(serde_json::to_string(&json!({
                    "success": true,
                    "updated": count,
                    "find": find,
                    "replace": replace,
                }))?)
            }

            // ── mempalace_diary_write ─────────────────────────────────────────
            "mempalace_diary_write" => {
                let agent_name = get_str(args, "agent_name")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: agent_name"))?;
                let entry = get_str(args, "entry")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: entry"))?;
                let topic = get_str(args, "topic").unwrap_or("general");

                let normalized = normalize_agent_name(agent_name);
                let wing = format!("wing_{normalized}");
                let full_content = format!("[{topic}] {entry}");

                let drawer_id = self.db.add_drawer(
                    &wing,
                    "diary",
                    &full_content,
                    None,
                    agent_name,
                    self.embedder.as_ref(),
                )?;
                wal::log_write("diary_write", json!({
                    "agent": agent_name, "topic": topic, "entry_id": drawer_id,
                }));
                Ok(serde_json::to_string(&json!({
                    "success": true,
                    "entry_id": drawer_id,
                    "agent": agent_name,
                    "topic": topic,
                }))?)
            }

            // ── mempalace_diary_read ──────────────────────────────────────────
            "mempalace_diary_read" => {
                let agent_name = get_str(args, "agent_name")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: agent_name"))?;
                let last_n = get_i64(args, "last_n").unwrap_or(10) as usize;
                let last_n = last_n.clamp(1, 100);
                let normalized = normalize_agent_name(agent_name);
                let wing = format!("wing_{normalized}");
                let data = self.db.get_diary_entries(&wing, last_n)?;
                let entries = data.get("entries").cloned().unwrap_or(json!([]));
                let total = data.get("total").cloned().unwrap_or(json!(0));
                let showing = data.get("showing").cloned().unwrap_or(json!(0));
                Ok(serde_json::to_string(&json!({
                    "agent": agent_name,
                    "entries": entries,
                    "total": total,
                    "showing": showing,
                }))?)
            }

            // ── mempalace_import_sessions ─────────────────────────────────────
            "mempalace_import_sessions" => {
                let oc_db_path = get_str(args, "oc_db_path").unwrap_or("");
                let full = args
                    .get("full")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let path = if oc_db_path.is_empty() {
                    let home = std::env::var("HOME").unwrap_or_default();
                    format!("{home}/.local/share/opencode/opencode.db")
                } else {
                    oc_db_path.to_string()
                };
                let count = import_sessions::import_sessions(
                    self.db,
                    &path,
                    self.embedder.as_ref(),
                    full,
                )?;
                Ok(serde_json::to_string(&json!({
                    "success": true,
                    "imported": count,
                }))?)
            }

            // ── mempalace_list_recent ─────────────────────────────────────────
            "mempalace_list_recent" => {
                let limit = get_i64(args, "limit").unwrap_or(20) as usize;
                let limit = limit.clamp(1, 100);
                let wing = validate::sanitize_name(get_str(args, "wing"), "wing")?;
                let since = validate::sanitize_iso_date(get_str(args, "since"))?;
                let results = self.db.list_recent(limit, wing, since)?;
                Ok(serde_json::to_string(&results)?)
            }

            // ── mempalace_export ──────────────────────────────────────────────
            "mempalace_export" => {
                let wing = validate::sanitize_name(get_str(args, "wing"), "wing")?;
                let room = validate::sanitize_name(get_str(args, "room"), "room")?;
                let result = self.db.export_drawers(wing, room)?;
                Ok(serde_json::to_string(&json!({
                    "success": true,
                    "data": result,
                }))?)
            }

            // ── mempalace_export_kg ────────────────────────────────────────────
            "mempalace_export_kg" => {
                let result = self.db.export_kg()?;
                Ok(serde_json::to_string(&result)?)
            }

            // ── mempalace_backup ───────────────────────────────────────────────
            "mempalace_backup" => {
                let path = get_str(args, "path").map(|s| s.to_string());
                let backup_path = self.db.backup(path.as_deref())?;
                Ok(serde_json::to_string(&json!({
                    "success": true,
                    "path": backup_path,
                }))?)
            }

            // ── mempalace_restore ──────────────────────────────────────────────
            "mempalace_restore" => {
                let path = get_str(args, "path")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: path"))?;
                self.db.restore(path)?;
                Ok(serde_json::to_string(&json!({
                    "success": true,
                    "restored_from": path,
                }))?)
            }

            // ── mempalace_repair ────────────────────────────────────────────────
            "mempalace_repair" => {
                let (total, embedded, _failed) = self
                    .db
                    .backfill_embeddings(
                        self.embedder
                            .as_ref()
                            .ok_or_else(|| anyhow::anyhow!("Embedder required for repair"))?,
                    )?;
                // Re-probe health after repair
                let health = self.db.vec0_health();
                Ok(serde_json::to_string(&json!({
                    "success": true,
                    "reindexed": embedded,
                    "total": total,
                    "vector_health": health,
                }))?)
            }

            // ── mempalace_reconnect ─────────────────────────────────────────────
            "mempalace_reconnect" => {
                let _ = self.db.conn.execute_batch(
                    "INSERT INTO drawers_fts(drawers_fts) VALUES('rebuild');",
                );
                let health = self.db.vec0_health();
                Ok(serde_json::to_string(&json!({
                    "success": true,
                    "fts_rebuilt": true,
                    "vector_health": health,
                }))?)
            }

            // ── mempalace_wal_log ───────────────────────────────────────────────
            "mempalace_wal_log" => {
                let limit = get_i64(args, "limit").unwrap_or(20) as usize;
                let limit = limit.clamp(1, 100);
                let entries = wal::read_entries(limit);
                Ok(serde_json::to_string(&json!({
                    "entries": entries,
                    "count": entries.len(),
                }))?)
            }

            // ── mempalace_create_tunnel ────────────────────────────────────────
            "mempalace_create_tunnel" => {
                let source_wing = validate::sanitize_name_required(
                    get_str(args, "source_wing")
                        .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: source_wing"))?,
                    "source_wing",
                )?;
                let source_room = validate::sanitize_name_required(
                    get_str(args, "source_room")
                        .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: source_room"))?,
                    "source_room",
                )?;
                let target_wing = validate::sanitize_name_required(
                    get_str(args, "target_wing")
                        .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: target_wing"))?,
                    "target_wing",
                )?;
                let target_room = validate::sanitize_name_required(
                    get_str(args, "target_room")
                        .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: target_room"))?,
                    "target_room",
                )?;
                let label = get_str(args, "label").unwrap_or("");
                let id = self.db.create_tunnel(
                    source_wing,
                    source_room,
                    target_wing,
                    target_room,
                    label,
                )?;
                Ok(serde_json::to_string(&json!({
                    "success": true, "tunnel_id": id,
                }))?)
            }

            // ── mempalace_list_tunnels ─────────────────────────────────────────
            "mempalace_list_tunnels" => {
                let wing = validate::sanitize_name(get_str(args, "wing"), "wing")?;
                let result = self.db.list_tunnels(wing)?;
                Ok(serde_json::to_string(&result)?)
            }

            // ── mempalace_delete_tunnel ────────────────────────────────────────
            "mempalace_delete_tunnel" => {
                let tunnel_id = get_str(args, "tunnel_id")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: tunnel_id"))?;
                self.db.delete_tunnel(tunnel_id)?;
                Ok(serde_json::to_string(&json!({
                    "success": true, "tunnel_id": tunnel_id,
                }))?)
            }

            // ── mempalace_follow_tunnels ───────────────────────────────────────
            "mempalace_follow_tunnels" => {
                let wing = validate::sanitize_name_required(
                    get_str(args, "wing")
                        .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: wing"))?,
                    "wing",
                )?;
                let room = validate::sanitize_name_required(
                    get_str(args, "room")
                        .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: room"))?,
                    "room",
                )?;
                let result = self.db.follow_tunnels(wing, room)?;
                Ok(serde_json::to_string(&result)?)
            }

            // ── mempalace_get_drawer ───────────────────────────────────────────
            "mempalace_get_drawer" => {
                let drawer_id = get_str(args, "drawer_id")
                    .ok_or_else(|| anyhow::anyhow!("MissingRequiredArg: drawer_id"))?;
                let result = self.db.get_drawer(drawer_id)?;
                Ok(serde_json::to_string(&result)?)
            }

            // ── mempalace_list_drawers ─────────────────────────────────────────
            "mempalace_list_drawers" => {
                let wing = validate::sanitize_name(get_str(args, "wing"), "wing")?;
                let room = validate::sanitize_name(get_str(args, "room"), "room")?;
                let limit = get_i64(args, "limit").unwrap_or(20) as usize;
                let limit = limit.clamp(1, 100);
                let offset = get_i64(args, "offset").unwrap_or(0) as usize;
                let result = self.db.list_drawers(wing, room, limit, offset)?;
                Ok(serde_json::to_string(&result)?)
            }

            // ── mempalace_integrity ────────────────────────────────────────────
            "mempalace_integrity" => {
                let result = self.db.integrity_check()?;
                Ok(serde_json::to_string(&result)?)
            }

            _ => Err(anyhow::anyhow!("UnknownTool: {name}")),
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn get_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)?.as_str()
}

fn get_i64(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(|v| match v {
        Value::Number(n) => n.as_i64(),
        _ => None,
    })
}

fn get_f64(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(|v| match v {
        Value::Number(n) => n.as_f64(),
        _ => None,
    })
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

fn json_value_to_id_str(val: &Value) -> String {
    match val {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("\"{}\"", s.replace('"', "\\\"")),
        _ => "null".to_string(),
    }
}
