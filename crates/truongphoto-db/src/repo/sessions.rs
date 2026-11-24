use crate::error::DbResult;
use crate::repo::{now_ms, to_sql_err};
use rusqlite::{params, Connection, OptionalExtension, Row};
use truongphoto_core::models::{ClientKind, SessionRecord};

/// Parameters for creating an authenticated session row.
#[derive(Debug, Clone)]
pub struct CreateSession {
    pub id: String,
    pub token_hash: String,
    pub user_id: String,
    pub client_kind: ClientKind,
    pub user_agent: Option<String>,
    pub ip_addr: Option<String>,
    pub expires_at_ms: i64,
}

const SESSION_COLUMNS: &str = "
    id, token_hash, user_id, client_kind, user_agent, ip_addr,
    created_at_ms, last_seen_at_ms, expires_at_ms, revoked_at_ms
";

fn row_to_session_record(row: &Row<'_>) -> rusqlite::Result<SessionRecord> {
    let kind_str: String = row.get(3)?;
    let client_kind = kind_str
        .parse::<ClientKind>()
        .map_err(|e| to_sql_err(e, 3))?;

    Ok(SessionRecord {
        id: row.get(0)?,
        token_hash: row.get(1)?,
        user_id: row.get(2)?,
        client_kind,
        user_agent: row.get(4)?,
        ip_addr: row.get(5)?,
        created_at_ms: row.get(6)?,
        last_seen_at_ms: row.get(7)?,
        expires_at_ms: row.get(8)?,
        revoked_at_ms: row.get(9)?,
    })
}

/// Inserts a new session row and returns the persisted `SessionRecord`.
pub fn create_session(conn: &Connection, input: &CreateSession) -> DbResult<SessionRecord> {
    let ts = now_ms();
    conn.execute(
        "INSERT INTO sessions (
            id, token_hash, user_id, client_kind, user_agent, ip_addr,
            created_at_ms, last_seen_at_ms, expires_at_ms, revoked_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, NULL)",
        params![
            input.id,
            input.token_hash,
            input.user_id,
            input.client_kind.as_str(),
            input.user_agent,
            input.ip_addr,
            ts,
            input.expires_at_ms,
        ],
    )?;

    Ok(SessionRecord {
        id: input.id.clone(),
        token_hash: input.token_hash.clone(),
        user_id: input.user_id.clone(),
        client_kind: input.client_kind,
        user_agent: input.user_agent.clone(),
        ip_addr: input.ip_addr.clone(),
        created_at_ms: ts,
        last_seen_at_ms: ts,
        expires_at_ms: input.expires_at_ms,
        revoked_at_ms: None,
    })
}

/// Looks up a non-revoked, non-expired session by SHA-256 `token_hash`.
pub fn find_active_session_by_token_hash(
    conn: &Connection,
    token_hash: &str,
    now_ms: i64,
) -> DbResult<Option<SessionRecord>> {
    let sql = format!(
        "SELECT {SESSION_COLUMNS}
         FROM sessions
         WHERE token_hash = ?1
           AND revoked_at_ms IS NULL
           AND expires_at_ms > ?2"
    );
    let session = conn
        .query_row(&sql, params![token_hash, now_ms], row_to_session_record)
        .optional()?;
    Ok(session)
}

/// Updates `last_seen_at_ms` for an active session.
pub fn touch_session(conn: &Connection, session_id: &str, now_ms: i64) -> DbResult<()> {
    conn.execute(
        "UPDATE sessions
         SET last_seen_at_ms = ?2
         WHERE id = ?1 AND revoked_at_ms IS NULL",
        params![session_id, now_ms],
    )?;
    Ok(())
}

/// Revokes a session by primary key `id`, returning true if an active row was revoked.
pub fn revoke_session_by_id(conn: &Connection, session_id: &str, now_ms: i64) -> DbResult<bool> {
    let affected = conn.execute(
        "UPDATE sessions
         SET revoked_at_ms = ?2
         WHERE id = ?1 AND revoked_at_ms IS NULL",
        params![session_id, now_ms],
    )?;
    Ok(affected > 0)
}

/// Revokes a session by SHA-256 `token_hash`, returning true if an active row was revoked.
pub fn revoke_session_by_token_hash(
    conn: &Connection,
    token_hash: &str,
    now_ms: i64,
) -> DbResult<bool> {
    let affected = conn.execute(
        "UPDATE sessions
         SET revoked_at_ms = ?2
         WHERE token_hash = ?1 AND revoked_at_ms IS NULL",
        params![token_hash, now_ms],
    )?;
    Ok(affected > 0)
}

/// Revokes all active sessions belonging to `user_id`.
pub fn revoke_sessions_for_user(conn: &Connection, user_id: &str, now_ms: i64) -> DbResult<usize> {
    let affected = conn.execute(
        "UPDATE sessions
         SET revoked_at_ms = ?2
         WHERE user_id = ?1 AND revoked_at_ms IS NULL",
        params![user_id, now_ms],
    )?;
    Ok(affected)
}

/// Revokes all active sessions belonging to `user_id` except `keep_session_id`.
pub fn revoke_other_sessions_for_user(
    conn: &Connection,
    user_id: &str,
    keep_session_id: &str,
    now_ms: i64,
) -> DbResult<usize> {
    let affected = conn.execute(
        "UPDATE sessions
         SET revoked_at_ms = ?3
         WHERE user_id = ?1 AND id != ?2 AND revoked_at_ms IS NULL",
        params![user_id, keep_session_id, now_ms],
    )?;
    Ok(affected)
}

/// Deletes expired or already-revoked sessions as of `now_ms`.
pub fn prune_expired_sessions(conn: &Connection, now_ms: i64) -> DbResult<usize> {
    let affected = conn.execute(
        "DELETE FROM sessions
         WHERE expires_at_ms <= ?1 OR revoked_at_ms IS NOT NULL",
        params![now_ms],
    )?;
    Ok(affected)
}
