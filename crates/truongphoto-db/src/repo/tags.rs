use crate::error::{DbError, DbResult};
use crate::rbac_sql;
use crate::repo::{now_ms, to_sql_err, with_savepoint};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};
use truongphoto_core::models::{MediaTagView, TagSource, TagState};
use truongphoto_core::rbac::RbacScope;

fn asset_visible_in_scope(conn: &Connection, scope: &RbacScope, asset_id: &str) -> DbResult<bool> {
    let mut params: Vec<Value> = vec![Value::Text(asset_id.to_string())];
    let rbac_clause = rbac_sql::folder_clause("a", scope, &mut params);
    let sql = format!(
        "SELECT 1 FROM assets a WHERE a.id = ?1 AND a.is_hidden = 0 AND {rbac_clause} LIMIT 1"
    );
    let exists: Option<i64> = conn
        .query_row(&sql, params_from_iter(params), |row| row.get(0))
        .optional()?;
    Ok(exists.is_some())
}

/// Lists all active (`state = 'active'`) tags for `asset_id`, ordered alphabetically.
pub fn list_active_tags(conn: &Connection, asset_id: &str) -> DbResult<Vec<MediaTagView>> {
    let mut stmt = conn.prepare(
        "SELECT tag, source, state, confidence
         FROM media_tags
         WHERE asset_id = ?1 AND state = 'active'
         ORDER BY tag COLLATE NOCASE ASC",
    )?;
    let rows = stmt.query_map(params![asset_id], |row| {
        let source_str: String = row.get(1)?;
        let state_str: String = row.get(2)?;
        let source = source_str
            .parse::<TagSource>()
            .map_err(|e| to_sql_err(e, 1))?;
        let state = state_str
            .parse::<TagState>()
            .map_err(|e| to_sql_err(e, 2))?;
        Ok(MediaTagView {
            tag: row.get(0)?,
            source,
            state,
            confidence: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Upserts a user tag (`source = 'user'`, `state = 'active'`) on an asset visible under `scope`.
///
/// Returns `Ok(None)` if the asset does not exist or is outside `scope`.
pub fn add_user_tag(
    conn: &Connection,
    scope: &RbacScope,
    asset_id: &str,
    actor_user_id: &str,
    tag: &str,
) -> DbResult<Option<Vec<MediaTagView>>> {
    let trimmed = tag.trim();
    if trimmed.is_empty() {
        return Err(DbError::InvalidArgument(
            "tag must not be empty".to_string(),
        ));
    }
    if !asset_visible_in_scope(conn, scope, asset_id)? {
        return Ok(None);
    }

    let ts = now_ms();
    with_savepoint(conn, "add_user_tag", || {
        conn.execute(
            "INSERT INTO media_tags (
                asset_id, tag, source, state, confidence, model_id,
                created_by, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, 'user', 'active', NULL, NULL, ?3, ?4, ?4)
             ON CONFLICT(asset_id, tag) DO UPDATE SET
                tag = excluded.tag,
                source = 'user',
                state = 'active',
                confidence = NULL,
                model_id = NULL,
                created_by = COALESCE(excluded.created_by, media_tags.created_by),
                updated_at_ms = excluded.updated_at_ms",
            params![asset_id, trimmed, actor_user_id, ts],
        )?;
        conn.execute(
            "UPDATE assets SET updated_at_ms = ?1 WHERE id = ?2",
            params![ts, asset_id],
        )?;
        Ok(())
    })?;

    Ok(Some(list_active_tags(conn, asset_id)?))
}

/// Removes or rejects a tag on an asset visible under `scope`.
///
/// If the existing tag has `source = 'ai'`, marks `state = 'rejected'` so future AI passes do not
/// re-add it. If `source = 'user'`, deletes the row. Returns `Ok(None)` if the asset is not visible.
pub fn remove_or_reject_tag(
    conn: &Connection,
    scope: &RbacScope,
    asset_id: &str,
    tag: &str,
) -> DbResult<Option<Vec<MediaTagView>>> {
    let trimmed = tag.trim();
    if trimmed.is_empty() {
        return Err(DbError::InvalidArgument(
            "tag must not be empty".to_string(),
        ));
    }
    if !asset_visible_in_scope(conn, scope, asset_id)? {
        return Ok(None);
    }

    let existing: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, source FROM media_tags WHERE asset_id = ?1 AND tag = ?2",
            params![asset_id, trimmed],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    if let Some((row_id, source)) = existing {
        let ts = now_ms();
        with_savepoint(conn, "remove_or_reject_tag", || {
            if source == TagSource::Ai.as_str() {
                conn.execute(
                    "UPDATE media_tags SET state = 'rejected', updated_at_ms = ?1 WHERE id = ?2",
                    params![ts, row_id],
                )?;
            } else {
                conn.execute("DELETE FROM media_tags WHERE id = ?1", params![row_id])?;
            }
            conn.execute(
                "UPDATE assets SET updated_at_ms = ?1 WHERE id = ?2",
                params![ts, asset_id],
            )?;
            Ok(())
        })?;
    }

    Ok(Some(list_active_tags(conn, asset_id)?))
}
