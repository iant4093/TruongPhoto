use crate::error::{DbError, DbResult};
use crate::rbac_sql;
use crate::repo::assets::{row_to_asset_summary, ASSET_SUMMARY_COLUMNS};
use crate::repo::{now_ms, to_sql_err, with_savepoint};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};
use std::collections::HashSet;
use truongphoto_core::models::{
    AlbumDetail, AlbumPermission, AlbumSharePermission, AlbumShareView, AlbumSortOrder,
    AlbumSummary, AssetSummary, ShareableUser,
};
use truongphoto_core::rbac::{RbacScope, Role};

/// Lists albums owned by `user_id` or shared with `user_id`, with counts and cover assets
/// strictly scoped to `scope`.
pub fn list_albums(
    conn: &Connection,
    user_id: &str,
    scope: &RbacScope,
) -> DbResult<Vec<AlbumSummary>> {
    let mut params: Vec<Value> = vec![Value::Text(user_id.to_string())];
    let cover_clause = rbac_sql::folder_clause("ca", scope, &mut params);
    let count_clause = rbac_sql::folder_clause("a", scope, &mut params);

    let sql = format!(
        "SELECT
            al.id,
            al.owner_user_id,
            al.title,
            al.description,
            CASE
                WHEN al.cover_asset_id IS NOT NULL AND EXISTS (
                    SELECT 1
                    FROM assets ca
                    WHERE ca.id = al.cover_asset_id
                      AND ca.deleted_at_ms IS NULL
                      AND ca.is_hidden = 0
                      AND {cover_clause}
                ) THEN al.cover_asset_id
                ELSE NULL
            END AS effective_cover_asset_id,
            al.sort_order,
            (
                SELECT COUNT(*)
                FROM album_assets aa
                JOIN assets a ON a.id = aa.asset_id
                WHERE aa.album_id = al.id
                  AND a.deleted_at_ms IS NULL
                  AND a.is_hidden = 0
                  AND {count_clause}
            ) AS asset_count,
            al.created_at_ms,
            al.updated_at_ms
         FROM albums al
         WHERE al.owner_user_id = ?1
            OR EXISTS (
                SELECT 1
                FROM album_shares s
                WHERE s.album_id = al.id
                  AND s.shared_with_user_id = ?1
            )
         ORDER BY al.updated_at_ms DESC, al.id ASC"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params), |row| {
        let count: i64 = row.get(6)?;
        Ok(AlbumSummary {
            id: row.get(0)?,
            owner_user_id: row.get(1)?,
            title: row.get(2)?,
            description: row.get(3)?,
            cover_asset_id: row.get(4)?,
            sort_order: row.get(5)?,
            asset_count: u64::try_from(count).unwrap_or(0),
            created_at_ms: row.get(7)?,
            updated_at_ms: row.get(8)?,
        })
    })?;

    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Resolves the caller's effective permission on `album_id`, or `None` if no access / missing.
pub fn get_user_album_permission(
    conn: &Connection,
    album_id: &str,
    user_id: &str,
    role: Role,
) -> DbResult<Option<AlbumPermission>> {
    let owner_user_id: Option<String> = conn
        .query_row(
            "SELECT owner_user_id FROM albums WHERE id = ?1",
            params![album_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(owner_user_id) = owner_user_id else {
        return Ok(None);
    };

    if owner_user_id == user_id {
        return Ok(Some(AlbumPermission::Owner));
    }
    if role.is_admin() {
        return Ok(Some(AlbumPermission::Admin));
    }

    let share_perm: Option<String> = conn
        .query_row(
            "SELECT permission
             FROM album_shares
             WHERE album_id = ?1 AND shared_with_user_id = ?2",
            params![album_id, user_id],
            |row| row.get(0),
        )
        .optional()?;

    match share_perm.as_deref() {
        Some("contribute") => Ok(Some(AlbumPermission::Contribute)),
        Some("view") => Ok(Some(AlbumPermission::View)),
        Some(other) => Err(DbError::InvalidArgument(format!(
            "unknown album_shares.permission: {other}"
        ))),
        None => Ok(None),
    }
}

fn filter_allowed_asset_ids(
    conn: &Connection,
    asset_ids: &[String],
    scope: &RbacScope,
) -> DbResult<Vec<String>> {
    if asset_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut seen = HashSet::with_capacity(asset_ids.len());
    let mut ordered_unique = Vec::with_capacity(asset_ids.len());
    for id in asset_ids {
        if seen.insert(id.as_str()) {
            ordered_unique.push(id.clone());
        }
    }

    const CHUNK_SIZE: usize = 400;
    let mut allowed_set: HashSet<String> = HashSet::with_capacity(ordered_unique.len());
    for chunk in ordered_unique.chunks(CHUNK_SIZE) {
        let mut params: Vec<Value> = Vec::with_capacity(chunk.len());
        let mut placeholders = Vec::with_capacity(chunk.len());
        for (idx, id) in chunk.iter().enumerate() {
            params.push(Value::Text(id.clone()));
            placeholders.push(format!("?{}", idx + 1));
        }
        let rbac_clause = rbac_sql::folder_clause("a", scope, &mut params);
        let sql = format!(
            "SELECT a.id
             FROM assets a
             WHERE a.id IN ({})
               AND a.deleted_at_ms IS NULL
               AND a.is_hidden = 0
               AND {rbac_clause}",
            placeholders.join(", ")
        );

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), |row| row.get::<_, String>(0))?;
        for r in rows {
            allowed_set.insert(r?);
        }
    }

    Ok(ordered_unique
        .into_iter()
        .filter(|id| allowed_set.contains(id))
        .collect())
}

/// Creates a new album owned by `owner_user_id` and attaches any allowed `asset_ids`.
pub fn create_album(
    conn: &Connection,
    owner_user_id: &str,
    title: &str,
    description: Option<&str>,
    asset_ids: &[String],
    scope: &RbacScope,
) -> DbResult<AlbumSummary> {
    let trimmed_title = title.trim();
    if trimmed_title.is_empty() {
        return Err(DbError::InvalidArgument(
            "album title must not be empty".to_string(),
        ));
    }

    let allowed_ids = filter_allowed_asset_ids(conn, asset_ids, scope)?;
    let album_id = uuid::Uuid::new_v4().to_string();
    let cover_asset_id = allowed_ids.first().cloned();
    let ts = now_ms();
    let desc_owned = description.map(str::trim).filter(|s| !s.is_empty());

    with_savepoint(conn, "create_album", || {
        conn.execute(
            "INSERT INTO albums (
                id, owner_user_id, title, description, cover_asset_id,
                sort_order, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'capture_desc', ?6, ?6)",
            params![
                album_id,
                owner_user_id,
                trimmed_title,
                desc_owned,
                cover_asset_id,
                ts,
            ],
        )?;

        let mut stmt = conn.prepare(
            "INSERT INTO album_assets (album_id, asset_id, position, added_by, added_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (pos, asset_id) in allowed_ids.iter().enumerate() {
            stmt.execute(params![album_id, asset_id, pos as i64, owner_user_id, ts,])?;
        }
        Ok(())
    })?;

    Ok(AlbumSummary {
        id: album_id,
        owner_user_id: owner_user_id.to_string(),
        title: trimmed_title.to_string(),
        description: desc_owned.map(ToString::to_string),
        cover_asset_id,
        sort_order: AlbumSortOrder::CaptureDesc.as_str().to_string(),
        asset_count: allowed_ids.len() as u64,
        created_at_ms: ts,
        updated_at_ms: ts,
    })
}

fn list_album_shares(conn: &Connection, album_id: &str) -> DbResult<Vec<AlbumShareView>> {
    let mut stmt = conn.prepare(
        "SELECT s.shared_with_user_id, u.display_name, u.avatar_color, s.permission
         FROM album_shares s
         JOIN users u ON u.id = s.shared_with_user_id
         WHERE s.album_id = ?1
           AND u.disabled_at_ms IS NULL
         ORDER BY u.display_name COLLATE NOCASE ASC, u.id ASC",
    )?;
    let rows = stmt.query_map(params![album_id], |row| {
        let perm_str: String = row.get(3)?;
        let permission = perm_str
            .parse::<AlbumSharePermission>()
            .map_err(|e| to_sql_err(e, 3))?;
        Ok(AlbumShareView {
            user_id: row.get(0)?,
            display_name: row.get(1)?,
            avatar_color: row.get(2)?,
            permission,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Fetches full album details with assets strictly filtered by caller's `RbacScope`.
pub fn get_album_detail(
    conn: &Connection,
    album_id: &str,
    user_id: &str,
    role: Role,
    scope: &RbacScope,
) -> DbResult<Option<AlbumDetail>> {
    let Some(permission) = get_user_album_permission(conn, album_id, user_id, role)? else {
        return Ok(None);
    };

    let mut summary_params: Vec<Value> = vec![Value::Text(album_id.to_string())];
    let cover_clause = rbac_sql::folder_clause("ca", scope, &mut summary_params);
    let summary_sql = format!(
        "SELECT
            al.id,
            al.owner_user_id,
            al.title,
            al.description,
            CASE
                WHEN al.cover_asset_id IS NOT NULL AND EXISTS (
                    SELECT 1
                    FROM assets ca
                    WHERE ca.id = al.cover_asset_id
                      AND ca.deleted_at_ms IS NULL
                      AND ca.is_hidden = 0
                      AND {cover_clause}
                ) THEN al.cover_asset_id
                ELSE NULL
            END AS effective_cover_asset_id,
            al.sort_order,
            al.created_at_ms,
            al.updated_at_ms
         FROM albums al
         WHERE al.id = ?1"
    );

    let Some(mut album) = conn
        .query_row(&summary_sql, params_from_iter(summary_params), |row| {
            Ok(AlbumSummary {
                id: row.get(0)?,
                owner_user_id: row.get(1)?,
                title: row.get(2)?,
                description: row.get(3)?,
                cover_asset_id: row.get(4)?,
                sort_order: row.get(5)?,
                asset_count: 0,
                created_at_ms: row.get(6)?,
                updated_at_ms: row.get(7)?,
            })
        })
        .optional()?
    else {
        return Ok(None);
    };

    let order_by = match album.sort_order.as_str() {
        "capture_asc" => "v.effective_capture_at_ms ASC, v.id ASC",
        "added_desc" => "aa.added_at_ms DESC, v.id DESC",
        "custom" => "aa.position ASC, aa.added_at_ms ASC, v.id ASC",
        _ => "v.effective_capture_at_ms DESC, v.id DESC",
    };

    let mut asset_params: Vec<Value> = vec![Value::Text(album_id.to_string())];
    let asset_clause = rbac_sql::folder_clause("v", scope, &mut asset_params);
    let assets_sql = format!(
        "SELECT {ASSET_SUMMARY_COLUMNS}
         FROM album_assets aa
         JOIN v_assets_effective v ON v.id = aa.asset_id
         WHERE aa.album_id = ?1
           AND v.deleted_at_ms IS NULL
           AND v.is_hidden = 0
           AND {asset_clause}
         ORDER BY {order_by}"
    );

    let mut stmt = conn.prepare(&assets_sql)?;
    let assets: Vec<AssetSummary> = stmt
        .query_map(params_from_iter(asset_params), row_to_asset_summary)?
        .collect::<Result<Vec<_>, _>>()?;

    album.asset_count = assets.len() as u64;
    let shares = list_album_shares(conn, album_id)?;

    Ok(Some(AlbumDetail {
        album,
        permission,
        assets,
        shares,
    }))
}

struct ExistingAlbumRow {
    owner_user_id: String,
    title: String,
    description: Option<String>,
    cover_asset_id: Option<String>,
    sort_order: String,
    created_at_ms: i64,
}

/// Updates mutable fields on an album (`title`, `description`, `cover_asset_id`, `sort_order`),
/// enforcing `scope` on cover asset validation and returned cover/counts.
pub fn update_album(
    conn: &Connection,
    album_id: &str,
    title: Option<&str>,
    description: Option<&str>,
    cover_asset_id: Option<&str>,
    sort_order: Option<AlbumSortOrder>,
    scope: &RbacScope,
) -> DbResult<Option<AlbumSummary>> {
    let existing: Option<ExistingAlbumRow> = conn
        .query_row(
            "SELECT owner_user_id, title, description, cover_asset_id, sort_order, created_at_ms
             FROM albums
             WHERE id = ?1",
            params![album_id],
            |row| {
                Ok(ExistingAlbumRow {
                    owner_user_id: row.get(0)?,
                    title: row.get(1)?,
                    description: row.get(2)?,
                    cover_asset_id: row.get(3)?,
                    sort_order: row.get(4)?,
                    created_at_ms: row.get(5)?,
                })
            },
        )
        .optional()?;

    let Some(ExistingAlbumRow {
        owner_user_id,
        title: cur_title,
        description: cur_desc,
        cover_asset_id: cur_cover,
        sort_order: cur_sort_order,
        created_at_ms,
    }) = existing
    else {
        return Ok(None);
    };

    let new_title = match title {
        Some(t) => {
            let trimmed = t.trim();
            if trimmed.is_empty() {
                return Err(DbError::InvalidArgument(
                    "album title must not be empty".to_string(),
                ));
            }
            trimmed.to_string()
        }
        None => cur_title,
    };

    let new_desc = match description {
        Some(d) => {
            let trimmed = d.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        None => cur_desc,
    };

    let new_cover = match cover_asset_id {
        Some(c) => {
            let trimmed = c.trim();
            if trimmed.is_empty() {
                None
            } else {
                let mut cover_params: Vec<Value> = vec![Value::Text(trimmed.to_string())];
                let rbac_clause = rbac_sql::folder_clause("a", scope, &mut cover_params);
                let cover_sql = format!(
                    "SELECT 1 FROM assets a WHERE a.id = ?1 AND a.deleted_at_ms IS NULL AND a.is_hidden = 0 AND {rbac_clause}"
                );
                let exists: Option<i64> = conn
                    .query_row(&cover_sql, params_from_iter(cover_params), |row| row.get(0))
                    .optional()?;
                if exists.is_none() {
                    return Err(DbError::NotFound(format!(
                        "cover asset not found: {trimmed}"
                    )));
                }
                Some(trimmed.to_string())
            }
        }
        None => cur_cover,
    };

    let new_sort_order = sort_order
        .map(|s| s.as_str().to_string())
        .unwrap_or(cur_sort_order);
    let ts = now_ms();

    conn.execute(
        "UPDATE albums
         SET title = ?1,
             description = ?2,
             cover_asset_id = ?3,
             sort_order = ?4,
             updated_at_ms = ?5
         WHERE id = ?6",
        params![new_title, new_desc, new_cover, new_sort_order, ts, album_id],
    )?;

    let mut count_params: Vec<Value> = vec![Value::Text(album_id.to_string())];
    let count_rbac = rbac_sql::folder_clause("a", scope, &mut count_params);
    let count_sql = format!(
        "SELECT COUNT(*)
         FROM album_assets aa
         JOIN assets a ON a.id = aa.asset_id
         WHERE aa.album_id = ?1
           AND a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND {count_rbac}"
    );
    let asset_count: i64 =
        conn.query_row(&count_sql, params_from_iter(count_params), |row| row.get(0))?;

    let visible_cover = match new_cover.as_deref() {
        Some(cid) => {
            let mut vc_params: Vec<Value> = vec![Value::Text(cid.to_string())];
            let vc_rbac = rbac_sql::folder_clause("a", scope, &mut vc_params);
            let vc_sql = format!(
                "SELECT 1 FROM assets a WHERE a.id = ?1 AND a.deleted_at_ms IS NULL AND a.is_hidden = 0 AND {vc_rbac}"
            );
            let ok: Option<i64> = conn
                .query_row(&vc_sql, params_from_iter(vc_params), |row| row.get(0))
                .optional()?;
            if ok.is_some() {
                Some(cid.to_string())
            } else {
                None
            }
        }
        None => None,
    };

    Ok(Some(AlbumSummary {
        id: album_id.to_string(),
        owner_user_id,
        title: new_title,
        description: new_desc,
        cover_asset_id: visible_cover,
        sort_order: new_sort_order,
        asset_count: u64::try_from(asset_count).unwrap_or(0),
        created_at_ms,
        updated_at_ms: ts,
    }))
}

/// Deletes an album by ID, returning true if a row was removed.
pub fn delete_album(conn: &Connection, album_id: &str) -> DbResult<bool> {
    let affected = conn.execute("DELETE FROM albums WHERE id = ?1", params![album_id])?;
    Ok(affected > 0)
}

/// Adds allowed, non-deleted assets to an album and returns the number of newly added entries.
pub fn add_album_assets(
    conn: &Connection,
    album_id: &str,
    actor_user_id: &str,
    asset_ids: &[String],
    scope: &RbacScope,
) -> DbResult<usize> {
    let existing_cover: Option<Option<String>> = conn
        .query_row(
            "SELECT cover_asset_id FROM albums WHERE id = ?1",
            params![album_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(current_cover) = existing_cover else {
        return Err(DbError::NotFound(format!("album not found: {album_id}")));
    };

    let allowed = filter_allowed_asset_ids(conn, asset_ids, scope)?;
    if allowed.is_empty() {
        return Ok(0);
    }

    let ts = now_ms();
    with_savepoint(conn, "add_album_assets", || {
        let max_pos: i64 = conn.query_row(
            "SELECT COALESCE(MAX(position), -1) FROM album_assets WHERE album_id = ?1",
            params![album_id],
            |row| row.get(0),
        )?;

        let mut added = 0usize;
        let mut stmt = conn.prepare(
            "INSERT OR IGNORE INTO album_assets (album_id, asset_id, position, added_by, added_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (idx, asset_id) in allowed.iter().enumerate() {
            let changed = stmt.execute(params![
                album_id,
                asset_id,
                max_pos + 1 + idx as i64,
                actor_user_id,
                ts,
            ])?;
            added += changed;
        }
        drop(stmt);

        if current_cover.is_none() {
            if let Some(first) = allowed.first() {
                conn.execute(
                    "UPDATE albums SET cover_asset_id = ?1, updated_at_ms = ?2 WHERE id = ?3",
                    params![first, ts, album_id],
                )?;
            }
        } else if added > 0 {
            conn.execute(
                "UPDATE albums SET updated_at_ms = ?1 WHERE id = ?2",
                params![ts, album_id],
            )?;
        }

        Ok(added)
    })
}

/// Removes RBAC-visible assets from an album and returns the number of removed rows.
pub fn remove_album_assets(
    conn: &Connection,
    album_id: &str,
    asset_ids: &[String],
    scope: &RbacScope,
) -> DbResult<usize> {
    if asset_ids.is_empty() {
        return Ok(0);
    }

    let existing_cover: Option<Option<String>> = conn
        .query_row(
            "SELECT cover_asset_id FROM albums WHERE id = ?1",
            params![album_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(current_cover) = existing_cover else {
        return Err(DbError::NotFound(format!("album not found: {album_id}")));
    };

    let mut seen = HashSet::with_capacity(asset_ids.len());
    let unique_ids: Vec<&String> = asset_ids
        .iter()
        .filter(|id| seen.insert(id.as_str()))
        .collect();
    if unique_ids.is_empty() {
        return Ok(0);
    }

    const CHUNK_SIZE: usize = 400;
    let ts = now_ms();
    with_savepoint(conn, "remove_album_assets", || {
        let mut removed = 0usize;
        for chunk in unique_ids.chunks(CHUNK_SIZE) {
            let mut params: Vec<Value> = vec![Value::Text(album_id.to_string())];
            let mut placeholders = Vec::with_capacity(chunk.len());
            for id in chunk {
                let idx = params.len() + 1;
                params.push(Value::Text((*id).clone()));
                placeholders.push(format!("?{idx}"));
            }
            let rbac_clause = rbac_sql::folder_clause("a", scope, &mut params);
            let del_sql = format!(
                "DELETE FROM album_assets
                 WHERE album_id = ?1
                   AND asset_id IN ({})
                   AND EXISTS (SELECT 1 FROM assets a WHERE a.id = album_assets.asset_id AND {rbac_clause})",
                placeholders.join(", ")
            );
            removed += conn.execute(&del_sql, params_from_iter(params))?;
        }

        if removed > 0 {
            let cover_removed = match current_cover.as_deref() {
                Some(c) => conn
                    .query_row(
                        "SELECT 1 FROM album_assets WHERE album_id = ?1 AND asset_id = ?2 LIMIT 1",
                        params![album_id, c],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?
                    .is_none(),
                None => false,
            };
            if cover_removed {
                let next_cover: Option<String> = conn
                    .query_row(
                        "SELECT aa.asset_id
                         FROM album_assets aa
                         JOIN assets a ON a.id = aa.asset_id
                         WHERE aa.album_id = ?1
                           AND a.deleted_at_ms IS NULL
                           AND a.is_hidden = 0
                         ORDER BY aa.position ASC, aa.added_at_ms ASC
                         LIMIT 1",
                        params![album_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                conn.execute(
                    "UPDATE albums SET cover_asset_id = ?1, updated_at_ms = ?2 WHERE id = ?3",
                    params![next_cover, ts, album_id],
                )?;
            } else {
                conn.execute(
                    "UPDATE albums SET updated_at_ms = ?1 WHERE id = ?2",
                    params![ts, album_id],
                )?;
            }
        }

        Ok(removed)
    })
}

/// Atomically replaces all `album_shares` for `album_id` and returns the resulting share list.
pub fn replace_album_shares(
    conn: &Connection,
    album_id: &str,
    actor_user_id: &str,
    shares: &[(String, AlbumSharePermission)],
) -> DbResult<Vec<AlbumShareView>> {
    let owner_user_id: Option<String> = conn
        .query_row(
            "SELECT owner_user_id FROM albums WHERE id = ?1",
            params![album_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(owner_user_id) = owner_user_id else {
        return Err(DbError::NotFound(format!("album not found: {album_id}")));
    };

    let ts = now_ms();
    with_savepoint(conn, "replace_album_shares", || {
        conn.execute(
            "DELETE FROM album_shares WHERE album_id = ?1",
            params![album_id],
        )?;

        let mut seen = HashSet::with_capacity(shares.len());
        let mut insert_stmt = conn.prepare(
            "INSERT INTO album_shares (
                album_id, shared_with_user_id, permission, shared_by, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        let mut check_user =
            conn.prepare("SELECT 1 FROM users WHERE id = ?1 AND disabled_at_ms IS NULL")?;

        for (target_user_id, permission) in shares {
            if target_user_id == &owner_user_id || !seen.insert(target_user_id.as_str()) {
                continue;
            }
            let active: Option<i64> = check_user
                .query_row(params![target_user_id], |row| row.get(0))
                .optional()?;
            if active.is_none() {
                continue;
            }
            insert_stmt.execute(params![
                album_id,
                target_user_id,
                permission.as_str(),
                actor_user_id,
                ts,
            ])?;
        }

        conn.execute(
            "UPDATE albums SET updated_at_ms = ?1 WHERE id = ?2",
            params![ts, album_id],
        )?;
        Ok(())
    })?;

    list_album_shares(conn, album_id)
}

/// Lists all active (non-disabled) users for the album sharing picker.
pub fn list_shareable_users(conn: &Connection) -> DbResult<Vec<ShareableUser>> {
    let mut stmt = conn.prepare(
        "SELECT id, display_name, avatar_color
         FROM users
         WHERE disabled_at_ms IS NULL
         ORDER BY display_name COLLATE NOCASE ASC, id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ShareableUser {
            id: row.get(0)?,
            display_name: row.get(1)?,
            avatar_color: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}
