use crate::error::{DbError, DbResult};
use crate::rbac_sql;
use crate::repo::assets::{parse_cursor, row_to_asset_summary, ASSET_SUMMARY_COLUMNS};
use crate::repo::{ai, bool_to_int, to_sql_err, with_savepoint};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension, Row};
use std::collections::HashSet;
use truongphoto_core::models::{
    AssetSummary, FaceView, PersonSummary, ReassignFaceResponse, UpdatePersonRequest,
};
use truongphoto_core::rbac::RbacScope;
use uuid::Uuid;

const DEFAULT_PERSON_ASSETS_LIMIT: usize = 100;
const MAX_PERSON_ASSETS_LIMIT: usize = 500;

fn person_summary_select_sql(
    cover_pin_clause: &str,
    cover_fallback_clause: &str,
    count_clause: &str,
) -> String {
    format!(
        "SELECT
            p.id,
            p.name,
            p.is_hidden,
            p.is_pinned,
            COALESCE(
                (
                    SELECT cf.id
                    FROM faces cf
                    JOIN assets ca ON ca.id = cf.asset_id
                    WHERE cf.id = p.cover_face_id
                      AND cf.person_id = p.id
                      AND ca.deleted_at_ms IS NULL
                      AND ca.is_hidden = 0
                      AND {cover_pin_clause}
                    LIMIT 1
                ),
                (
                    SELECT ff.id
                    FROM faces ff
                    JOIN assets fa ON fa.id = ff.asset_id
                    WHERE ff.person_id = p.id
                      AND fa.deleted_at_ms IS NULL
                      AND fa.is_hidden = 0
                      AND {cover_fallback_clause}
                    ORDER BY ff.det_score DESC, ff.created_at_ms ASC, ff.id ASC
                    LIMIT 1
                )
            ) AS effective_cover_face_id,
            (
                SELECT COUNT(*)
                FROM faces fc
                JOIN assets fca ON fca.id = fc.asset_id
                WHERE fc.person_id = p.id
                  AND fca.deleted_at_ms IS NULL
                  AND fca.is_hidden = 0
                  AND {count_clause}
            ) AS face_count,
            p.created_at_ms,
            p.updated_at_ms"
    )
}

fn row_to_person_summary(row: &Row<'_>) -> rusqlite::Result<PersonSummary> {
    let face_count: i64 = row.get(5)?;
    Ok(PersonSummary {
        id: row.get(0)?,
        name: row.get(1)?,
        is_hidden: row.get::<_, i64>(2)? != 0,
        is_pinned: row.get::<_, i64>(3)? != 0,
        cover_face_id: row.get(4)?,
        face_count: u64::try_from(face_count).unwrap_or(0),
        created_at_ms: row.get(6)?,
        updated_at_ms: row.get(7)?,
    })
}

fn get_person_admin_unfiltered(
    conn: &Connection,
    person_id: &str,
) -> DbResult<Option<PersonSummary>> {
    let select_head = person_summary_select_sql("1 = 1", "1 = 1", "1 = 1");
    let sql = format!("{select_head} FROM people p WHERE p.id = ?1");
    let row = conn
        .query_row(&sql, params![person_id], row_to_person_summary)
        .optional()?;
    Ok(row)
}

/// Lists non-hidden people visible under `scope` with strict allowed-folder invariant enforcement:
/// - Only returns people having at least one face on a non-deleted, non-hidden asset in `scope`.
/// - `face_count` counts strictly faces on non-deleted, non-hidden assets in `scope`.
/// - `cover_face_id` uses `p.cover_face_id` ONLY when its asset is in `scope` and active;
///   otherwise falls back to the highest-`det_score` face for `p.id` in `scope`.
pub fn list_people(conn: &Connection, scope: &RbacScope) -> DbResult<Vec<PersonSummary>> {
    list_people_filtered(conn, scope, false)
}

/// Lists people visible under `scope`, optionally including hidden clusters (`include_hidden`).
pub fn list_people_filtered(
    conn: &Connection,
    scope: &RbacScope,
    include_hidden: bool,
) -> DbResult<Vec<PersonSummary>> {
    let mut params: Vec<Value> = Vec::new();
    let cover_pin_clause = rbac_sql::folder_clause("ca", scope, &mut params);
    let cover_fallback_clause = rbac_sql::folder_clause("fa", scope, &mut params);
    let count_clause = rbac_sql::folder_clause("fca", scope, &mut params);
    let exists_clause = rbac_sql::folder_clause("ea", scope, &mut params);

    let select_head =
        person_summary_select_sql(&cover_pin_clause, &cover_fallback_clause, &count_clause);
    let hidden_clause = if include_hidden {
        "1 = 1"
    } else {
        "p.is_hidden = 0"
    };

    let sql = format!(
        "{select_head}
         FROM people p
         WHERE {hidden_clause}
           AND EXISTS (
               SELECT 1
               FROM faces ef
               JOIN assets ea ON ea.id = ef.asset_id
               WHERE ef.person_id = p.id
                 AND ea.deleted_at_ms IS NULL
                 AND ea.is_hidden = 0
                 AND {exists_clause}
           )
         ORDER BY p.is_pinned DESC, face_count DESC, p.created_at_ms ASC, p.id ASC"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params), row_to_person_summary)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Fetches a single person by ID strictly under `scope`.
///
/// Returns `Ok(None)` if the person does not exist or has 0 active faces in `scope`.
pub fn get_person_by_id(
    conn: &Connection,
    scope: &RbacScope,
    person_id: &str,
) -> DbResult<Option<PersonSummary>> {
    let mut params: Vec<Value> = vec![Value::Text(person_id.to_string())];
    let cover_pin_clause = rbac_sql::folder_clause("ca", scope, &mut params);
    let cover_fallback_clause = rbac_sql::folder_clause("fa", scope, &mut params);
    let count_clause = rbac_sql::folder_clause("fca", scope, &mut params);
    let exists_clause = rbac_sql::folder_clause("ea", scope, &mut params);

    let select_head =
        person_summary_select_sql(&cover_pin_clause, &cover_fallback_clause, &count_clause);
    let sql = format!(
        "{select_head}
         FROM people p
         WHERE p.id = ?1
           AND EXISTS (
               SELECT 1
               FROM faces ef
               JOIN assets ea ON ea.id = ef.asset_id
               WHERE ef.person_id = p.id
                 AND ea.deleted_at_ms IS NULL
                 AND ea.is_hidden = 0
                 AND {exists_clause}
           )"
    );

    let row = conn
        .query_row(&sql, params_from_iter(params), row_to_person_summary)
        .optional()?;
    Ok(row)
}

/// Lists active, non-deleted assets containing `person_id` strictly within `scope`.
///
/// Returns `(items, next_cursor, total_count)`.
pub fn list_person_assets(
    conn: &Connection,
    scope: &RbacScope,
    person_id: &str,
    cursor: Option<&str>,
    limit: usize,
) -> DbResult<(Vec<AssetSummary>, Option<String>, i64)> {
    let eff_limit = if limit == 0 {
        DEFAULT_PERSON_ASSETS_LIMIT
    } else {
        limit.min(MAX_PERSON_ASSETS_LIMIT)
    };

    let mut base_params: Vec<Value> = vec![Value::Text(person_id.to_string())];
    let rbac_clause = rbac_sql::folder_clause("v", scope, &mut base_params);
    let base_where = format!(
        "v.deleted_at_ms IS NULL
         AND v.is_hidden = 0
         AND {rbac_clause}
         AND EXISTS (
             SELECT 1
             FROM faces f
             WHERE f.asset_id = v.id
               AND f.person_id = ?1
         )"
    );

    let count_sql = format!("SELECT COUNT(*) FROM v_assets_effective v WHERE {base_where}");
    let total_count: i64 =
        conn.query_row(&count_sql, params_from_iter(base_params.clone()), |row| {
            row.get(0)
        })?;

    if total_count == 0 {
        return Ok((Vec::new(), None, 0));
    }

    let mut page_params = base_params;
    let mut page_where = base_where;
    if let Some(cursor_str) = cursor {
        let (cursor_ts, cursor_id) = parse_cursor(cursor_str)?;
        let ts_idx = page_params.len() + 1;
        let id_idx = page_params.len() + 2;
        page_params.push(Value::Integer(cursor_ts));
        page_params.push(Value::Text(cursor_id));
        page_where = format!(
            "{page_where} AND (v.effective_capture_at_ms < ?{ts_idx} OR (v.effective_capture_at_ms = ?{ts_idx} AND v.id < ?{id_idx}))"
        );
    }

    let limit_idx = page_params.len() + 1;
    page_params.push(Value::Integer((eff_limit + 1) as i64));

    let list_sql = format!(
        "SELECT {ASSET_SUMMARY_COLUMNS}
         FROM v_assets_effective v
         WHERE {page_where}
         ORDER BY v.effective_capture_at_ms DESC, v.id DESC
         LIMIT ?{limit_idx}"
    );

    let mut stmt = conn.prepare(&list_sql)?;
    let mut items = stmt
        .query_map(params_from_iter(page_params), row_to_asset_summary)?
        .collect::<Result<Vec<_>, _>>()?;

    let next_cursor = if items.len() > eff_limit {
        items.truncate(eff_limit);
        items
            .last()
            .map(|item| format!("{}:{}", item.capture_at_ms, item.id))
    } else {
        None
    };

    Ok((items, next_cursor, total_count))
}

/// Applies partial admin updates (`name`, `is_hidden`, `is_pinned`, `cover_face_id`) to `person_id`.
pub fn update_person(
    conn: &Connection,
    person_id: &str,
    req: &UpdatePersonRequest,
    now_ms: i64,
) -> DbResult<Option<PersonSummary>> {
    let existing: Option<(Option<String>, i64, i64, Option<String>)> = conn
        .query_row(
            "SELECT name, is_hidden, is_pinned, cover_face_id FROM people WHERE id = ?1",
            params![person_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;

    let Some((cur_name, cur_hidden, cur_pinned, cur_cover)) = existing else {
        return Ok(None);
    };

    let next_name = match &req.name {
        Some(n) => {
            let trimmed = n.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        None => cur_name,
    };
    let next_hidden = req.is_hidden.map(bool_to_int).unwrap_or(cur_hidden);
    let next_pinned = req.is_pinned.map(bool_to_int).unwrap_or(cur_pinned);

    let next_cover = match &req.cover_face_id {
        Some(fid) => {
            let belongs: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM faces WHERE id = ?1 AND person_id = ?2 LIMIT 1",
                    params![fid, person_id],
                    |row| row.get(0),
                )
                .optional()?;
            if belongs.is_none() {
                return Err(DbError::InvalidArgument(format!(
                    "face '{fid}' does not belong to person '{person_id}'"
                )));
            }
            Some(fid.clone())
        }
        None => cur_cover,
    };

    conn.execute(
        "UPDATE people
         SET name = ?1, is_hidden = ?2, is_pinned = ?3, cover_face_id = ?4, updated_at_ms = ?5
         WHERE id = ?6",
        params![
            next_name,
            next_hidden,
            next_pinned,
            next_cover,
            now_ms,
            person_id
        ],
    )?;

    get_person_admin_unfiltered(conn, person_id)
}

/// Merges `source_ids` into `target_id`, reassigning all faces and deleting source people & centroids.
///
/// Returns `Ok(None)` if `target_id` does not exist.
pub fn merge_people(
    conn: &Connection,
    source_ids: &[String],
    target_id: &str,
    now_ms: i64,
) -> DbResult<Option<(PersonSummary, u64)>> {
    let target_exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM people WHERE id = ?1 LIMIT 1",
            params![target_id],
            |row| row.get(0),
        )
        .optional()?;
    if target_exists.is_none() {
        return Ok(None);
    }

    let mut clean_sources = Vec::new();
    let mut seen = HashSet::new();
    for sid in source_ids {
        if sid != target_id && seen.insert(sid.as_str()) {
            clean_sources.push(sid.clone());
        }
    }

    let merged_faces = with_savepoint(conn, "merge_people", || {
        let mut total_moved = 0u64;
        if !clean_sources.is_empty() {
            for chunk in clean_sources.chunks(400) {
                let mut move_params: Vec<Value> =
                    vec![Value::Text(target_id.to_string()), Value::Integer(now_ms)];
                let mut placeholders = Vec::with_capacity(chunk.len());
                for (idx, sid) in chunk.iter().enumerate() {
                    move_params.push(Value::Text(sid.clone()));
                    placeholders.push(format!("?{}", idx + 3));
                }
                let ph_sql = placeholders.join(", ");

                let move_sql = format!(
                    "UPDATE faces
                     SET person_id = ?1, updated_at_ms = ?2
                     WHERE person_id IN ({ph_sql})"
                );
                let moved = conn.execute(&move_sql, params_from_iter(move_params))?;
                total_moved += u64::try_from(moved).unwrap_or(0);

                let del_params: Vec<Value> = chunk.iter().cloned().map(Value::Text).collect();
                let mut del_ph = Vec::with_capacity(chunk.len());
                for idx in 0..chunk.len() {
                    del_ph.push(format!("?{}", idx + 1));
                }
                let del_ph_sql = del_ph.join(", ");

                let del_cent_sql =
                    format!("DELETE FROM person_centroids WHERE person_id IN ({del_ph_sql})");
                conn.execute(&del_cent_sql, params_from_iter(del_params.clone()))?;

                let del_people_sql = format!("DELETE FROM people WHERE id IN ({del_ph_sql})");
                conn.execute(&del_people_sql, params_from_iter(del_params))?;
            }
        }

        conn.execute(
            "UPDATE people SET updated_at_ms = ?1 WHERE id = ?2",
            params![now_ms, target_id],
        )?;
        Ok(total_moved)
    })?;

    let Some(summary) = get_person_admin_unfiltered(conn, target_id)? else {
        return Ok(None);
    };
    Ok(Some((summary, merged_faces)))
}

/// Reassigns `face_id` to an existing person (`target_person_id`), a newly created person
/// (`new_person_name`), or unassigns it (`None`/`None`).
///
/// Returns `Ok(None)` if `face_id` does not exist.
pub fn reassign_face(
    conn: &Connection,
    face_id: &str,
    target_person_id: Option<&str>,
    new_person_name: Option<&str>,
    now_ms: i64,
) -> DbResult<Option<ReassignFaceResponse>> {
    let existing: Option<Option<String>> = conn
        .query_row(
            "SELECT person_id FROM faces WHERE id = ?1",
            params![face_id],
            |row| row.get(0),
        )
        .optional()?;

    let Some(old_person_id) = existing else {
        return Ok(None);
    };

    let resolved_person_id = with_savepoint(conn, "reassign_face", || {
        if let Some(old_pid) = old_person_id.as_deref() {
            conn.execute(
                "UPDATE people
                 SET cover_face_id = NULL, updated_at_ms = ?1
                 WHERE id = ?2 AND cover_face_id = ?3",
                params![now_ms, old_pid, face_id],
            )?;
        }

        if let Some(target_pid) = target_person_id {
            let exists: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM people WHERE id = ?1 LIMIT 1",
                    params![target_pid],
                    |row| row.get(0),
                )
                .optional()?;
            if exists.is_none() {
                return Err(DbError::NotFound(format!(
                    "target person '{target_pid}' not found"
                )));
            }
            conn.execute(
                "UPDATE faces
                 SET person_id = ?1, assigned_by = 'manual', updated_at_ms = ?2
                 WHERE id = ?3",
                params![target_pid, now_ms, face_id],
            )?;
            return Ok(Some(target_pid.to_string()));
        }

        if let Some(raw_name) = new_person_name {
            let trimmed = raw_name.trim();
            if !trimmed.is_empty() {
                let new_person_id = Uuid::new_v4().to_string();
                conn.execute(
                    "INSERT INTO people (
                        id, name, is_hidden, is_pinned, cover_face_id, created_at_ms, updated_at_ms
                     ) VALUES (?1, ?2, 0, 0, ?3, ?4, ?4)",
                    params![new_person_id, trimmed, face_id, now_ms],
                )?;
                conn.execute(
                    "UPDATE faces
                     SET person_id = ?1, assigned_by = 'manual', updated_at_ms = ?2
                     WHERE id = ?3",
                    params![new_person_id, now_ms, face_id],
                )?;
                return Ok(Some(new_person_id));
            }
        }

        conn.execute(
            "UPDATE faces
             SET person_id = NULL, assigned_by = 'unassigned', updated_at_ms = ?1
             WHERE id = ?2",
            params![now_ms, face_id],
        )?;
        Ok(None)
    })?;

    Ok(Some(ReassignFaceResponse {
        face_id: face_id.to_string(),
        person_id: resolved_person_id,
    }))
}

/// Lists detected faces on `asset_id` strictly within `scope`.
pub fn list_asset_faces(
    conn: &Connection,
    scope: &RbacScope,
    asset_id: &str,
) -> DbResult<Vec<FaceView>> {
    let mut params: Vec<Value> = vec![Value::Text(asset_id.to_string())];
    let rbac_clause = rbac_sql::folder_clause("a", scope, &mut params);

    let sql = format!(
        "SELECT
            f.id,
            f.asset_id,
            f.person_id,
            p.name,
            f.bbox_x,
            f.bbox_y,
            f.bbox_w,
            f.bbox_h,
            f.det_score,
            f.assigned_by
         FROM faces f
         JOIN assets a ON a.id = f.asset_id
         LEFT JOIN people p ON p.id = f.person_id
         WHERE f.asset_id = ?1
           AND a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND {rbac_clause}
         ORDER BY f.det_score DESC, f.created_at_ms ASC, f.id ASC"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params), |row| {
        Ok(FaceView {
            id: row.get(0)?,
            asset_id: row.get(1)?,
            person_id: row.get(2)?,
            person_name: row.get(3)?,
            bbox_x: row.get(4)?,
            bbox_y: row.get(5)?,
            bbox_w: row.get(6)?,
            bbox_h: row.get(7)?,
            det_score: row.get(8)?,
            assigned_by: row.get(9)?,
        })
    })?;

    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Creates a new unnamed person cluster, assigns `face_ids` (`assigned_by = 'auto'`),
/// and stores its L2-normalized centroid in `person_centroids`.
pub fn create_person_for_cluster(
    conn: &Connection,
    face_ids: &[String],
    centroid_blob: &[u8],
    now_ms: i64,
) -> DbResult<String> {
    if face_ids.is_empty() {
        return Err(DbError::InvalidArgument(
            "face_ids must not be empty when creating a cluster".to_string(),
        ));
    }

    let person_id = Uuid::new_v4().to_string();
    with_savepoint(conn, "create_person_for_cluster", || {
        // Pick the highest-det_score face in the cluster as the default cover face.
        let mut best_face: Option<(String, f64)> = None;
        for chunk in face_ids.chunks(400) {
            let mut q_params: Vec<Value> = Vec::with_capacity(chunk.len());
            let mut ph = Vec::with_capacity(chunk.len());
            for (idx, fid) in chunk.iter().enumerate() {
                q_params.push(Value::Text(fid.clone()));
                ph.push(format!("?{}", idx + 1));
            }
            let q_sql = format!(
                "SELECT id, det_score FROM faces WHERE id IN ({}) ORDER BY det_score DESC, id ASC LIMIT 1",
                ph.join(", ")
            );
            if let Some((fid, score)) = conn
                .query_row(&q_sql, params_from_iter(q_params), |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
                })
                .optional()?
            {
                if best_face
                    .as_ref()
                    .map(|(_, cur)| score > *cur)
                    .unwrap_or(true)
                {
                    best_face = Some((fid, score));
                }
            }
        }
        let cover_face_id = best_face
            .map(|(id, _)| id)
            .or_else(|| face_ids.first().cloned());

        conn.execute(
            "INSERT INTO people (
                id, name, is_hidden, is_pinned, cover_face_id, created_at_ms, updated_at_ms
             ) VALUES (?1, NULL, 0, 0, ?2, ?3, ?3)",
            params![person_id, cover_face_id, now_ms],
        )?;

        for chunk in face_ids.chunks(400) {
            let mut u_params: Vec<Value> =
                vec![Value::Text(person_id.clone()), Value::Integer(now_ms)];
            let mut ph = Vec::with_capacity(chunk.len());
            for (idx, fid) in chunk.iter().enumerate() {
                u_params.push(Value::Text(fid.clone()));
                ph.push(format!("?{}", idx + 3));
            }
            let u_sql = format!(
                "UPDATE faces
                 SET person_id = ?1, assigned_by = 'auto', updated_at_ms = ?2
                 WHERE id IN ({})",
                ph.join(", ")
            );
            conn.execute(&u_sql, params_from_iter(u_params))?;
        }

        ai::upsert_person_centroid(
            conn,
            &person_id,
            face_ids.len() as u64,
            centroid_blob,
            now_ms,
        )?;
        Ok(())
    })?;

    Ok(person_id)
}

/// Lists all face embedding `vector_id`s belonging to `person_id` on active, non-deleted assets.
pub fn list_face_vector_ids_for_person(conn: &Connection, person_id: &str) -> DbResult<Vec<u32>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(fe.vector_id, f.vector_id) AS vid
         FROM faces f
         JOIN assets a ON a.id = f.asset_id
         LEFT JOIN face_embeddings fe ON fe.face_id = f.id
         WHERE f.person_id = ?1
           AND a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND COALESCE(fe.vector_id, f.vector_id) IS NOT NULL
         ORDER BY vid ASC",
    )?;
    let rows = stmt.query_map(params![person_id], |row| {
        let vid: i64 = row.get(0)?;
        u32::try_from(vid).map_err(|e| to_sql_err(e, 0))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}
