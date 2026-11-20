use crate::error::{DbError, DbResult};
use crate::repo::{bool_to_int, to_sql_err, with_savepoint};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use truongphoto_core::models::{
    ChatHitRef, ChatRole, ChatSessionDetail, ChatSessionSummary, PlannerMode, RouterPlan,
};

/// Raw persisted chat message row prior to hydrating `SearchHit` asset summaries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredChatMessage {
    pub id: String,
    pub session_id: String,
    pub role: ChatRole,
    pub content: String,
    pub plan: Option<RouterPlan>,
    pub hit_refs: Vec<ChatHitRef>,
    pub reranked: bool,
    pub planner_mode: Option<PlannerMode>,
    pub elapsed_ms: Option<u64>,
    pub created_at_ms: i64,
}

fn row_to_chat_session_summary(row: &Row<'_>) -> rusqlite::Result<ChatSessionSummary> {
    Ok(ChatSessionSummary {
        id: row.get(0)?,
        title: row.get(1)?,
        created_at_ms: row.get(2)?,
        updated_at_ms: row.get(3)?,
    })
}

fn row_to_chat_session_detail(row: &Row<'_>) -> rusqlite::Result<ChatSessionDetail> {
    let filters_json: String = row.get(2)?;
    let active_filters: RouterPlan =
        serde_json::from_str(&filters_json).map_err(|e| to_sql_err(e, 2))?;
    Ok(ChatSessionDetail {
        id: row.get(0)?,
        title: row.get(1)?,
        active_filters,
        created_at_ms: row.get(3)?,
        updated_at_ms: row.get(4)?,
    })
}

fn row_to_stored_chat_message(row: &Row<'_>) -> rusqlite::Result<StoredChatMessage> {
    let role_str: String = row.get(2)?;
    let role = role_str.parse::<ChatRole>().map_err(|e| to_sql_err(e, 2))?;

    let plan_json: Option<String> = row.get(4)?;
    let plan = plan_json
        .map(|raw| serde_json::from_str::<RouterPlan>(&raw))
        .transpose()
        .map_err(|e| to_sql_err(e, 4))?;

    let refs_json: Option<String> = row.get(5)?;
    let hit_refs = match refs_json {
        Some(raw) => serde_json::from_str::<Vec<ChatHitRef>>(&raw).map_err(|e| to_sql_err(e, 5))?,
        None => Vec::new(),
    };

    let planner_str: Option<String> = row.get(7)?;
    let planner_mode = planner_str
        .map(|s| s.parse::<PlannerMode>())
        .transpose()
        .map_err(|e| to_sql_err(e, 7))?;

    let elapsed_i64: Option<i64> = row.get(8)?;
    let elapsed_ms = elapsed_i64.and_then(|v| u64::try_from(v).ok());

    Ok(StoredChatMessage {
        id: row.get(0)?,
        session_id: row.get(1)?,
        role,
        content: row.get(3)?,
        plan,
        hit_refs,
        reranked: row.get::<_, i64>(6)? != 0,
        planner_mode,
        elapsed_ms,
        created_at_ms: row.get(9)?,
    })
}

/// Creates a new conversational search session strictly owned by `user_id`.
pub fn create_chat_session(
    conn: &Connection,
    user_id: &str,
    title: &str,
    now_ms: i64,
) -> DbResult<ChatSessionSummary> {
    let id = uuid::Uuid::new_v4().to_string();
    let trimmed = title.trim();
    let effective_title = if trimmed.is_empty() {
        "New Chat"
    } else {
        trimmed
    };

    conn.execute(
        "INSERT INTO chat_sessions (
            id, user_id, title, active_filters_json, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, '{}', ?4, ?4)",
        params![id, user_id, effective_title, now_ms],
    )?;

    Ok(ChatSessionSummary {
        id,
        title: effective_title.to_string(),
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    })
}

/// Lists conversational search sessions owned by `user_id`, newest updated first.
pub fn list_chat_sessions(conn: &Connection, user_id: &str) -> DbResult<Vec<ChatSessionSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, created_at_ms, updated_at_ms
         FROM chat_sessions
         WHERE user_id = ?1
         ORDER BY updated_at_ms DESC, id DESC",
    )?;
    let rows = stmt.query_map(params![user_id], row_to_chat_session_summary)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Loads a single chat session if and only if it belongs to `user_id`.
pub fn get_chat_session(
    conn: &Connection,
    user_id: &str,
    session_id: &str,
) -> DbResult<Option<ChatSessionDetail>> {
    let session = conn
        .query_row(
            "SELECT id, title, active_filters_json, created_at_ms, updated_at_ms
             FROM chat_sessions
             WHERE id = ?1 AND user_id = ?2",
            params![session_id, user_id],
            row_to_chat_session_detail,
        )
        .optional()?;
    Ok(session)
}

/// Deletes a chat session (and its cascaded messages) if and only if it belongs to `user_id`.
pub fn delete_chat_session(conn: &Connection, user_id: &str, session_id: &str) -> DbResult<bool> {
    let deleted = conn.execute(
        "DELETE FROM chat_sessions WHERE id = ?1 AND user_id = ?2",
        params![session_id, user_id],
    )?;
    Ok(deleted > 0)
}

/// Updates carried-forward `active_filters_json` and optional `title` for a session owned by `user_id`.
pub fn update_chat_session_filters(
    conn: &Connection,
    user_id: &str,
    session_id: &str,
    active_filters: &RouterPlan,
    title: Option<&str>,
    now_ms: i64,
) -> DbResult<bool> {
    let filters_json = serde_json::to_string(active_filters)
        .map_err(|e| DbError::InvalidArgument(format!("serialize active_filters: {e}")))?;
    let clean_title = title.map(str::trim).filter(|s| !s.is_empty());

    let updated = conn.execute(
        "UPDATE chat_sessions
         SET active_filters_json = ?1,
             title = COALESCE(?2, title),
             updated_at_ms = ?3
         WHERE id = ?4 AND user_id = ?5",
        params![filters_json, clean_title, now_ms, session_id, user_id],
    )?;
    Ok(updated > 0)
}

/// Appends a user or assistant turn to `chat_messages` and updates the parent session timestamp.
#[allow(clippy::too_many_arguments)]
pub fn append_chat_message(
    conn: &Connection,
    session_id: &str,
    role: ChatRole,
    content: &str,
    plan: Option<&RouterPlan>,
    hit_refs: Option<&[ChatHitRef]>,
    reranked: bool,
    planner_mode: Option<PlannerMode>,
    elapsed_ms: Option<u64>,
    now_ms: i64,
) -> DbResult<StoredChatMessage> {
    let id = uuid::Uuid::new_v4().to_string();
    let plan_json = plan
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| DbError::InvalidArgument(format!("serialize plan: {e}")))?;
    let refs_json = hit_refs
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| DbError::InvalidArgument(format!("serialize hit_refs: {e}")))?;
    let elapsed_i64 = elapsed_ms
        .map(|v| {
            i64::try_from(v).map_err(|_| DbError::InvalidArgument("elapsed_ms overflow".into()))
        })
        .transpose()?;

    with_savepoint(conn, "append_chat_message", || {
        conn.execute(
            "INSERT INTO chat_messages (
                id, session_id, role, content, plan_json, result_asset_ids,
                reranked, planner_mode, elapsed_ms, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                session_id,
                role.as_str(),
                content,
                plan_json,
                refs_json,
                bool_to_int(reranked),
                planner_mode.map(PlannerMode::as_str),
                elapsed_i64,
                now_ms,
            ],
        )?;
        conn.execute(
            "UPDATE chat_sessions
             SET updated_at_ms = MAX(updated_at_ms, ?1)
             WHERE id = ?2",
            params![now_ms, session_id],
        )?;
        Ok(())
    })?;

    Ok(StoredChatMessage {
        id,
        session_id: session_id.to_string(),
        role,
        content: content.to_string(),
        plan: plan.cloned(),
        hit_refs: hit_refs.map(|s| s.to_vec()).unwrap_or_default(),
        reranked,
        planner_mode,
        elapsed_ms,
        created_at_ms: now_ms,
    })
}

/// Lists all persisted messages in `session_id` ordered chronologically.
pub fn list_chat_messages(conn: &Connection, session_id: &str) -> DbResult<Vec<StoredChatMessage>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, role, content, plan_json, result_asset_ids,
                reranked, planner_mode, elapsed_ms, created_at_ms
         FROM chat_messages
         WHERE session_id = ?1
         ORDER BY created_at_ms ASC, rowid ASC",
    )?;
    let rows = stmt.query_map(params![session_id], row_to_stored_chat_message)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}
