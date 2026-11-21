use crate::error::{DbError, DbResult};
use crate::rbac_sql;
use crate::repo::{bool_to_int, to_sql_err, with_savepoint};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use truongphoto_core::models::{AiIndexStateView, AiModelStatus, AiQueueStats, AiTier, ChatTier};
use truongphoto_core::rbac::RbacScope;

/// Database projection for a row in `ai_models`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiModelRow {
    pub model_id: String,
    pub status: AiModelStatus,
    pub local_dir: Option<String>,
    pub bytes_total: u64,
    pub bytes_downloaded: u64,
    pub sha256_manifest: Option<String>,
    pub error_message: Option<String>,
    pub updated_at_ms: i64,
}

/// RBAC-verified source image and bounding box metadata for generating a cropped face thumbnail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceThumbSource {
    pub face_id: String,
    pub asset_id: String,
    pub folder_root_path: String,
    pub primary_rel_path: String,
    pub bbox_x: f64,
    pub bbox_y: f64,
    pub bbox_w: f64,
    pub bbox_h: f64,
}

fn row_to_ai_index_state(row: &Row<'_>) -> rusqlite::Result<AiIndexStateView> {
    let active_str: String = row.get(0)?;
    let active_vision_tier = active_str.parse::<AiTier>().map_err(|e| to_sql_err(e, 0))?;
    let staging_str: Option<String> = row.get(1)?;
    let staging_vision_tier = staging_str
        .map(|s| s.parse::<AiTier>())
        .transpose()
        .map_err(|e| to_sql_err(e, 1))?;
    let idle_secs: i64 = row.get(3)?;
    let chat_tier_str: String = row.get(5)?;
    let active_chat_tier = chat_tier_str
        .parse::<ChatTier>()
        .map_err(|e| to_sql_err(e, 5))?;
    Ok(AiIndexStateView {
        active_vision_tier,
        staging_vision_tier,
        paused: row.get::<_, i64>(2)? != 0,
        idle_unload_secs: u64::try_from(idle_secs).unwrap_or(300),
        updated_at_ms: row.get(4)?,
        active_chat_tier,
        vlm_rerank_enabled: row.get::<_, i64>(6)? != 0,
    })
}

fn row_to_ai_model(row: &Row<'_>) -> rusqlite::Result<AiModelRow> {
    let status_str: String = row.get(1)?;
    let status = status_str
        .parse::<AiModelStatus>()
        .map_err(|e| to_sql_err(e, 1))?;
    let bytes_total: i64 = row.get(3)?;
    let bytes_downloaded: i64 = row.get(4)?;
    Ok(AiModelRow {
        model_id: row.get(0)?,
        status,
        local_dir: row.get(2)?,
        bytes_total: u64::try_from(bytes_total).unwrap_or(0),
        bytes_downloaded: u64::try_from(bytes_downloaded).unwrap_or(0),
        sha256_manifest: row.get(5)?,
        error_message: row.get(6)?,
        updated_at_ms: row.get(7)?,
    })
}

/// Reads the singleton `ai_index_state` row tracking active/staging vision tiers, chat tier, and pause state.
pub fn get_ai_index_state(conn: &Connection) -> DbResult<AiIndexStateView> {
    conn.query_row(
        "SELECT active_vision_tier, staging_vision_tier, paused, idle_unload_secs, updated_at_ms,
                active_chat_tier, vlm_rerank_enabled
         FROM ai_index_state
         WHERE id = 1",
        [],
        row_to_ai_index_state,
    )
    .optional()?
    .ok_or_else(|| DbError::NotFound("ai_index_state singleton row missing".to_string()))
}

/// Updates `active_chat_tier` and optionally `vlm_rerank_enabled` in `ai_index_state`.
pub fn set_chat_tier(
    conn: &Connection,
    tier: ChatTier,
    vlm_rerank_enabled: Option<bool>,
    now_ms: i64,
) -> DbResult<AiIndexStateView> {
    let rerank_int = vlm_rerank_enabled.map(bool_to_int);
    conn.execute(
        "UPDATE ai_index_state
         SET active_chat_tier = ?1,
             vlm_rerank_enabled = COALESCE(?2, vlm_rerank_enabled),
             updated_at_ms = ?3
         WHERE id = 1",
        params![tier.as_str(), rerank_int, now_ms],
    )?;
    get_ai_index_state(conn)
}

/// Sets or clears `staging_vision_tier` without interrupting `active_vision_tier`.
pub fn set_staging_vision_tier(
    conn: &Connection,
    tier: Option<AiTier>,
    now_ms: i64,
) -> DbResult<AiIndexStateView> {
    let tier_str = tier.map(AiTier::as_str);
    conn.execute(
        "UPDATE ai_index_state
         SET staging_vision_tier = ?1, updated_at_ms = ?2
         WHERE id = 1",
        params![tier_str, now_ms],
    )?;
    get_ai_index_state(conn)
}

/// Atomically promotes `new_active` to `active_vision_tier` and clears `staging_vision_tier`.
pub fn commit_vision_tier_cutover(
    conn: &Connection,
    new_active: AiTier,
    now_ms: i64,
) -> DbResult<AiIndexStateView> {
    conn.execute(
        "UPDATE ai_index_state
         SET active_vision_tier = ?1, staging_vision_tier = NULL, updated_at_ms = ?2
         WHERE id = 1",
        params![new_active.as_str(), now_ms],
    )?;
    get_ai_index_state(conn)
}

/// Updates the `paused` flag in `ai_index_state` and returns the refreshed state.
pub fn set_ai_paused(conn: &Connection, paused: bool, now_ms: i64) -> DbResult<AiIndexStateView> {
    conn.execute(
        "UPDATE ai_index_state
         SET paused = ?1, updated_at_ms = ?2
         WHERE id = 1",
        params![bool_to_int(paused), now_ms],
    )?;
    get_ai_index_state(conn)
}

/// Updates `idle_unload_secs` in `ai_index_state` and returns the refreshed state.
pub fn update_ai_idle_unload_secs(
    conn: &Connection,
    secs: u64,
    now_ms: i64,
) -> DbResult<AiIndexStateView> {
    let secs_i64 = i64::try_from(secs)
        .map_err(|_| DbError::InvalidArgument("idle_unload_secs out of range".to_string()))?;
    conn.execute(
        "UPDATE ai_index_state
         SET idle_unload_secs = ?1, updated_at_ms = ?2
         WHERE id = 1",
        params![secs_i64, now_ms],
    )?;
    get_ai_index_state(conn)
}

/// Inserts or updates a model state row in `ai_models`.
pub fn upsert_ai_model_row(conn: &Connection, row: &AiModelRow) -> DbResult<()> {
    let bytes_total = i64::try_from(row.bytes_total)
        .map_err(|_| DbError::InvalidArgument("bytes_total out of range".to_string()))?;
    let bytes_downloaded = i64::try_from(row.bytes_downloaded)
        .map_err(|_| DbError::InvalidArgument("bytes_downloaded out of range".to_string()))?;

    conn.execute(
        "INSERT INTO ai_models (
            model_id, status, local_dir, bytes_total, bytes_downloaded,
            sha256_manifest, error_message, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(model_id) DO UPDATE SET
            status = excluded.status,
            local_dir = excluded.local_dir,
            bytes_total = excluded.bytes_total,
            bytes_downloaded = excluded.bytes_downloaded,
            sha256_manifest = excluded.sha256_manifest,
            error_message = excluded.error_message,
            updated_at_ms = excluded.updated_at_ms",
        params![
            row.model_id,
            row.status.as_str(),
            row.local_dir,
            bytes_total,
            bytes_downloaded,
            row.sha256_manifest,
            row.error_message,
            row.updated_at_ms,
        ],
    )?;
    Ok(())
}

/// Lists all tracked model rows from `ai_models` ordered by `model_id`.
pub fn list_ai_model_rows(conn: &Connection) -> DbResult<Vec<AiModelRow>> {
    let mut stmt = conn.prepare(
        "SELECT
            model_id, status, local_dir, bytes_total, bytes_downloaded,
            sha256_manifest, error_message, updated_at_ms
         FROM ai_models
         ORDER BY model_id ASC",
    )?;
    let rows = stmt.query_map([], row_to_ai_model)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Fetches a single model row by `model_id`.
pub fn get_ai_model_row(conn: &Connection, model_id: &str) -> DbResult<Option<AiModelRow>> {
    let row = conn
        .query_row(
            "SELECT
                model_id, status, local_dir, bytes_total, bytes_downloaded,
                sha256_manifest, error_message, updated_at_ms
             FROM ai_models
             WHERE model_id = ?1",
            params![model_id],
            row_to_ai_model,
        )
        .optional()?;
    Ok(row)
}

/// Deletes a model row by `model_id`, returning `true` if a row was removed.
pub fn delete_ai_model_row(conn: &Connection, model_id: &str) -> DbResult<bool> {
    let deleted = conn.execute(
        "DELETE FROM ai_models WHERE model_id = ?1",
        params![model_id],
    )?;
    Ok(deleted > 0)
}

/// Allocates the next 0-based `vector_id` slot in `vision_embeddings` for `tier`.
pub fn allocate_next_vision_vector_id(conn: &Connection, tier: AiTier) -> DbResult<u32> {
    let next: i64 = conn.query_row(
        "SELECT COALESCE(MAX(vector_id) + 1, 0)
         FROM vision_embeddings
         WHERE model_tier = ?1",
        params![tier.as_str()],
        |row| row.get(0),
    )?;
    u32::try_from(next)
        .map_err(|_| DbError::InvalidArgument("vision vector_id overflow".to_string()))
}

/// Inserts or replaces a vision embedding mapping for `(tier, asset_id, timestamp_ms)` and `(tier, vector_id)`.
pub fn upsert_vision_embedding(
    conn: &Connection,
    tier: AiTier,
    vector_id: u32,
    asset_id: &str,
    timestamp_ms: i64,
    now_ms: i64,
) -> DbResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO vision_embeddings (
            model_tier, vector_id, asset_id, timestamp_ms, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            tier.as_str(),
            i64::from(vector_id),
            asset_id,
            timestamp_ms,
            now_ms,
        ],
    )?;
    Ok(())
}

/// Resolves `vector_ids` for `tier` into `vector_id -> (asset_id, timestamp_ms)`.
pub fn resolve_vision_vector_ids(
    conn: &Connection,
    tier: AiTier,
    vector_ids: &[u32],
) -> DbResult<HashMap<u32, (String, i64)>> {
    if vector_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut unique_ids: Vec<u32> = Vec::with_capacity(vector_ids.len());
    let mut seen: HashSet<u32> = HashSet::with_capacity(vector_ids.len());
    for &vid in vector_ids {
        if seen.insert(vid) {
            unique_ids.push(vid);
        }
    }

    let mut result = HashMap::with_capacity(unique_ids.len());
    for chunk in unique_ids.chunks(500) {
        let mut params: Vec<Value> = Vec::with_capacity(chunk.len() + 1);
        params.push(Value::Text(tier.as_str().to_string()));
        let mut placeholders = Vec::with_capacity(chunk.len());
        for (idx, &vid) in chunk.iter().enumerate() {
            params.push(Value::Integer(i64::from(vid)));
            placeholders.push(format!("?{}", idx + 2));
        }

        let sql = format!(
            "SELECT vector_id, asset_id, timestamp_ms
             FROM vision_embeddings
             WHERE model_tier = ?1
               AND vector_id IN ({})",
            placeholders.join(", ")
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), |row| {
            let vid: i64 = row.get(0)?;
            let asset_id: String = row.get(1)?;
            let ts: i64 = row.get(2)?;
            Ok((u32::try_from(vid).unwrap_or(0), asset_id, ts))
        })?;
        for row in rows {
            let (vid, asset_id, ts) = row?;
            result.insert(vid, (asset_id, ts));
        }
    }

    Ok(result)
}

/// Lists all vision `vector_id`s registered for `asset_id` under `tier`.
pub fn list_vector_ids_for_asset(
    conn: &Connection,
    tier: AiTier,
    asset_id: &str,
) -> DbResult<Vec<u32>> {
    let mut stmt = conn.prepare(
        "SELECT vector_id
         FROM vision_embeddings
         WHERE model_tier = ?1 AND asset_id = ?2
         ORDER BY timestamp_ms ASC, vector_id ASC",
    )?;
    let rows = stmt.query_map(params![tier.as_str(), asset_id], |row| {
        let vid: i64 = row.get(0)?;
        u32::try_from(vid).map_err(|e| to_sql_err(e, 0))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Deletes all `vision_embeddings` rows belonging to `tier` and returns the removed row count.
pub fn delete_vision_embeddings_for_tier(conn: &Connection, tier: AiTier) -> DbResult<u64> {
    let deleted = conn.execute(
        "DELETE FROM vision_embeddings WHERE model_tier = ?1",
        params![tier.as_str()],
    )?;
    Ok(u64::try_from(deleted).unwrap_or(0))
}

/// Computes Stage 2 AI queue counters for the active tier and optional staging tier.
pub fn count_ai_queue_stats(
    conn: &Connection,
    active_tier: AiTier,
    staging_tier: Option<AiTier>,
) -> DbResult<AiQueueStats> {
    let (pending_i64, indexed_i64): (i64, i64) = conn.query_row(
        "SELECT
            COALESCE(SUM(CASE
                WHEN a.ai_stage < 2 OR NOT EXISTS (
                    SELECT 1 FROM vision_embeddings ve
                    WHERE ve.model_tier = ?1 AND ve.asset_id = a.id
                ) THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE
                WHEN a.ai_stage >= 2 AND EXISTS (
                    SELECT 1 FROM vision_embeddings ve
                    WHERE ve.model_tier = ?1 AND ve.asset_id = a.id
                ) THEN 1 ELSE 0 END), 0)
         FROM assets a
         WHERE a.deleted_at_ms IS NULL AND a.is_hidden = 0",
        params![active_tier.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    let staging_remaining = if let Some(st) = staging_tier {
        let rem_i64: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM assets a
             WHERE a.deleted_at_ms IS NULL
               AND a.is_hidden = 0
               AND NOT EXISTS (
                   SELECT 1 FROM vision_embeddings ve
                   WHERE ve.model_tier = ?1 AND ve.asset_id = a.id
               )",
            params![st.as_str()],
            |row| row.get(0),
        )?;
        u64::try_from(rem_i64).unwrap_or(0)
    } else {
        0
    };

    Ok(AiQueueStats {
        pending_assets: u64::try_from(pending_i64).unwrap_or(0),
        indexed_assets: u64::try_from(indexed_i64).unwrap_or(0),
        staging_remaining,
    })
}

/// Lists active, non-deleted asset IDs that still need Stage 2 indexing for the active tier.
pub fn list_assets_needing_stage2(conn: &Connection, limit: usize) -> DbResult<Vec<String>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut stmt = conn.prepare(
        "SELECT a.id
         FROM assets a
         WHERE a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND (
               a.ai_stage < 2
               OR NOT EXISTS (
                   SELECT 1
                   FROM vision_embeddings ve
                   WHERE ve.model_tier = COALESCE(
                       (SELECT active_vision_tier FROM ai_index_state WHERE id = 1),
                       'tier2'
                   )
                     AND ve.asset_id = a.id
               )
           )
         ORDER BY a.capture_at_ms DESC, a.id DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit_i64], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Lists active, non-deleted asset IDs missing a vision embedding in `staging_tier`.
pub fn list_assets_needing_staging_tier(
    conn: &Connection,
    staging_tier: AiTier,
    limit: usize,
) -> DbResult<Vec<String>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut stmt = conn.prepare(
        "SELECT a.id
         FROM assets a
         WHERE a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND NOT EXISTS (
               SELECT 1
               FROM vision_embeddings ve
               WHERE ve.model_tier = ?1
                 AND ve.asset_id = a.id
           )
         ORDER BY a.capture_at_ms DESC, a.id DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![staging_tier.as_str(), limit_i64], |row| {
        row.get::<_, String>(0)
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Updates `assets.ai_stage` (`0`, `1`, or `2`) for `asset_id`.
pub fn mark_asset_ai_stage(
    conn: &Connection,
    asset_id: &str,
    stage: i32,
    now_ms: i64,
) -> DbResult<()> {
    if !(0..=2).contains(&stage) {
        return Err(DbError::InvalidArgument(format!(
            "ai_stage must be 0, 1, or 2 (got {stage})"
        )));
    }
    conn.execute(
        "UPDATE assets
         SET ai_stage = ?1, updated_at_ms = ?2
         WHERE id = ?3",
        params![stage, now_ms, asset_id],
    )?;
    Ok(())
}

/// Resets `ai_stage` from `2` back to `1` on all active, non-deleted assets for reindexing.
pub fn reset_all_assets_ai_stage_to_1(conn: &Connection, now_ms: i64) -> DbResult<u64> {
    let updated = conn.execute(
        "UPDATE assets
         SET ai_stage = 1, updated_at_ms = ?1
         WHERE deleted_at_ms IS NULL
           AND is_hidden = 0
           AND ai_stage > 1",
        params![now_ms],
    )?;
    Ok(u64::try_from(updated).unwrap_or(0))
}

/// Inserts zero-shot AI tags via `INSERT OR IGNORE` so user-rejected (`state = 'rejected'`)
/// and user-authored tags on `(asset_id, tag)` are never overwritten.
pub fn insert_ai_tags_ignoring_rejected(
    conn: &Connection,
    asset_id: &str,
    tags: &[(String, f64)],
    model_id: &str,
    now_ms: i64,
) -> DbResult<usize> {
    if tags.is_empty() {
        return Ok(0);
    }

    with_savepoint(conn, "insert_ai_tags", || {
        let mut stmt = conn.prepare_cached(
            "INSERT OR IGNORE INTO media_tags (
                asset_id, tag, source, state, confidence, model_id,
                created_by, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, 'ai', 'active', ?3, ?4, NULL, ?5, ?5)",
        )?;
        let mut inserted = 0usize;
        for (raw_tag, confidence) in tags {
            let trimmed = raw_tag.trim();
            if trimmed.is_empty() {
                continue;
            }
            inserted += stmt.execute(params![asset_id, trimmed, confidence, model_id, now_ms])?;
        }
        Ok(inserted)
    })
}

/// Upserts extracted OCR text for `asset_id`; Phase 0 triggers auto-sync `ocr_fts`.
pub fn upsert_ocr_document(
    conn: &Connection,
    asset_id: &str,
    full_text: &str,
    lang_hint: Option<&str>,
    model_id: &str,
    now_ms: i64,
) -> DbResult<()> {
    conn.execute(
        "INSERT INTO ocr_documents (asset_id, full_text, lang_hint, model_id, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(asset_id) DO UPDATE SET
            full_text = excluded.full_text,
            lang_hint = excluded.lang_hint,
            model_id = excluded.model_id,
            updated_at_ms = excluded.updated_at_ms",
        params![asset_id, full_text, lang_hint, model_id, now_ms],
    )?;
    Ok(())
}

/// Allocates the next 0-based `vector_id` slot in the `faces.f16` namespace.
pub fn allocate_next_face_vector_id(conn: &Connection) -> DbResult<u32> {
    let next: i64 = conn.query_row(
        "SELECT COALESCE(MAX(v) + 1, 0)
         FROM (
             SELECT MAX(vector_id) AS v FROM face_embeddings
             UNION ALL
             SELECT MAX(vector_id) AS v FROM faces
         )",
        [],
        |row| row.get(0),
    )?;
    u32::try_from(next).map_err(|_| DbError::InvalidArgument("face vector_id overflow".to_string()))
}

/// Inserts a detected face into `faces` and records its `faces.f16` slot in `face_embeddings`.
#[allow(clippy::too_many_arguments)]
pub fn insert_face_with_embedding(
    conn: &Connection,
    face_id: &str,
    asset_id: &str,
    person_id: Option<&str>,
    bbox: (f64, f64, f64, f64),
    det_score: f64,
    vector_id: u32,
    assigned_by: &str,
    now_ms: i64,
) -> DbResult<()> {
    if !matches!(assigned_by, "auto" | "manual" | "unassigned") {
        return Err(DbError::InvalidArgument(format!(
            "invalid assigned_by: {assigned_by}"
        )));
    }

    with_savepoint(conn, "insert_face_with_embedding", || {
        conn.execute(
            "INSERT INTO faces (
                id, asset_id, person_id, bbox_x, bbox_y, bbox_w, bbox_h,
                det_score, vector_id, assigned_by, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
            params![
                face_id,
                asset_id,
                person_id,
                bbox.0,
                bbox.1,
                bbox.2,
                bbox.3,
                det_score,
                i64::from(vector_id),
                assigned_by,
                now_ms,
            ],
        )?;
        conn.execute(
            "INSERT INTO face_embeddings (vector_id, face_id, created_at_ms)
             VALUES (?1, ?2, ?3)",
            params![i64::from(vector_id), face_id, now_ms],
        )?;
        Ok(())
    })
}

/// Lists unassigned faces (`person_id IS NULL`) on active, non-deleted assets with their `vector_id`.
pub fn list_unassigned_faces_with_vectors(conn: &Connection) -> DbResult<Vec<(String, u32)>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, COALESCE(fe.vector_id, f.vector_id) AS vid
         FROM faces f
         JOIN assets a ON a.id = f.asset_id
         LEFT JOIN face_embeddings fe ON fe.face_id = f.id
         WHERE f.person_id IS NULL
           AND a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND COALESCE(fe.vector_id, f.vector_id) IS NOT NULL
         ORDER BY f.created_at_ms ASC, f.id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        let face_id: String = row.get(0)?;
        let vid: i64 = row.get(1)?;
        let vid_u32 = u32::try_from(vid).map_err(|e| to_sql_err(e, 1))?;
        Ok((face_id, vid_u32))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Resolves face bounding box and primary file path for thumbnail cropping, strictly scoped by `scope`.
///
/// Returns `Ok(None)` if the face does not exist, belongs to a deleted/hidden asset, or lies outside `scope`.
pub fn get_face_with_asset_for_thumb(
    conn: &Connection,
    scope: &RbacScope,
    face_id: &str,
) -> DbResult<Option<FaceThumbSource>> {
    let mut params: Vec<Value> = vec![Value::Text(face_id.to_string())];
    let rbac_clause = rbac_sql::folder_clause("a", scope, &mut params);
    let hidden_person_clause = if matches!(scope, RbacScope::AdminAll) {
        "1 = 1"
    } else {
        "(f.person_id IS NULL OR NOT EXISTS (SELECT 1 FROM people ph WHERE ph.id = f.person_id AND ph.is_hidden = 1))"
    };

    let sql = format!(
        "SELECT
            f.id,
            a.id,
            fl.root_path,
            (
                SELECT af.rel_path
                FROM asset_files af
                WHERE af.asset_id = a.id
                  AND af.is_missing = 0
                ORDER BY CASE WHEN af.id = a.primary_file_id THEN 0
                              WHEN af.file_role = 'primary' THEN 1
                              ELSE 2 END,
                         af.rel_path ASC
                LIMIT 1
            ) AS primary_rel_path,
            f.bbox_x,
            f.bbox_y,
            f.bbox_w,
            f.bbox_h
         FROM faces f
         JOIN assets a ON a.id = f.asset_id
         JOIN folders fl ON fl.id = a.folder_id
         WHERE f.id = ?1
           AND a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND fl.is_offline = 0
           AND {hidden_person_clause}
           AND {rbac_clause}
         LIMIT 1"
    );

    let row = conn
        .query_row(&sql, params_from_iter(params), |row| {
            let primary_rel_path: Option<String> = row.get(3)?;
            Ok(primary_rel_path.map(|rel| FaceThumbSource {
                face_id: row.get::<_, String>(0).unwrap_or_default(),
                asset_id: row.get::<_, String>(1).unwrap_or_default(),
                folder_root_path: row.get::<_, String>(2).unwrap_or_default(),
                primary_rel_path: rel,
                bbox_x: row.get::<_, f64>(4).unwrap_or(0.0),
                bbox_y: row.get::<_, f64>(5).unwrap_or(0.0),
                bbox_w: row.get::<_, f64>(6).unwrap_or(0.0),
                bbox_h: row.get::<_, f64>(7).unwrap_or(0.0),
            }))
        })
        .optional()?
        .flatten();

    Ok(row)
}

/// Inserts or updates a person's raw L2-normalized `f32` LE centroid blob in `person_centroids`.
pub fn upsert_person_centroid(
    conn: &Connection,
    person_id: &str,
    face_count: u64,
    centroid_blob: &[u8],
    now_ms: i64,
) -> DbResult<()> {
    let face_count_i64 = i64::try_from(face_count)
        .map_err(|_| DbError::InvalidArgument("face_count out of range".to_string()))?;
    conn.execute(
        "INSERT INTO person_centroids (person_id, face_count, centroid_blob, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(person_id) DO UPDATE SET
            face_count = excluded.face_count,
            centroid_blob = excluded.centroid_blob,
            updated_at_ms = excluded.updated_at_ms",
        params![person_id, face_count_i64, centroid_blob, now_ms],
    )?;
    Ok(())
}

/// Lists all `(person_id, face_count, centroid_blob)` entries for incremental Stage 1 face matching.
pub fn list_person_centroids(conn: &Connection) -> DbResult<Vec<(String, u64, Vec<u8>)>> {
    let mut stmt = conn.prepare(
        "SELECT person_id, face_count, centroid_blob
         FROM person_centroids
         ORDER BY person_id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        let count: i64 = row.get(1)?;
        Ok((
            row.get::<_, String>(0)?,
            u64::try_from(count).unwrap_or(0),
            row.get::<_, Vec<u8>>(2)?,
        ))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Deletes a person's centroid row, returning `true` if a row existed.
pub fn delete_person_centroid(conn: &Connection, person_id: &str) -> DbResult<bool> {
    let deleted = conn.execute(
        "DELETE FROM person_centroids WHERE person_id = ?1",
        params![person_id],
    )?;
    Ok(deleted > 0)
}
