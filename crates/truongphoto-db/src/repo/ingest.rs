use crate::error::{DbError, DbResult};
use crate::repo::{assets::upsert_asset_histogram, bool_to_int, now_ms, with_savepoint};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};
use std::collections::HashMap;
use truongphoto_core::models::{AssetKind, FileRole, Histogram256, PhotoMetaView, VideoMetaView};

/// In-memory index entry for an `asset_files` row during incremental folder scans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingFileEntry {
    pub file_id: String,
    pub asset_id: String,
    pub rel_path: String,
    pub file_size: u64,
    pub mtime_ms: i64,
    pub partial_blake3: String,
    pub is_missing: bool,
}

/// Complete extracted file & metadata payload produced by Stage 1 ingestion.
#[derive(Debug, Clone, Default)]
pub struct IngestedFileData {
    pub asset_id_hint: Option<String>,
    pub folder_id: String,
    pub rel_path: String,
    pub mime_type: String,
    pub extension: String,
    pub file_size: u64,
    pub mtime_ms: i64,
    pub partial_blake3: String,
    pub capture_at_ms: i64,
    pub capture_tz_offset_m: Option<i32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<i64>,
    pub thumb_rel_path: Option<String>,
    pub thumb_ready: bool,
    pub photo_meta: Option<PhotoMetaView>,
    pub raw_exif_json: Option<String>,
    pub video_meta: Option<VideoMetaView>,
    pub raw_probe_json: Option<String>,
    pub histogram: Option<Histogram256>,
    pub content_identifier: Option<String>,
    pub embedded_video_offset: Option<u64>,
    pub embedded_video_length: Option<u64>,
    pub is_raw: bool,
    pub is_video: bool,
    pub is_livp: bool,
    pub parent_dir: String,
    pub lower_stem: String,
}

fn is_raw_extension(ext: &str) -> bool {
    matches!(
        ext.trim().to_ascii_lowercase().as_str(),
        "cr3"
            | "cr2"
            | "nef"
            | "nrw"
            | "arw"
            | "srf"
            | "dng"
            | "raf"
            | "orf"
            | "rw2"
            | "pef"
            | "srw"
            | "iiq"
            | "3fr"
    )
}

fn is_companion_still_ext(ext: &str) -> bool {
    matches!(
        ext.trim().to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "heic" | "heif"
    )
}

fn non_empty_cid(cid: Option<&str>) -> Option<&str> {
    cid.map(str::trim).filter(|s| !s.is_empty())
}

fn stem_stack_key(parent_dir: &str, lower_stem: &str) -> String {
    let norm_dir = parent_dir
        .trim()
        .replace('\\', "/")
        .trim_matches('/')
        .to_string();
    let stem = lower_stem.trim().to_ascii_lowercase();
    if norm_dir.is_empty() {
        format!("stem:{stem}")
    } else {
        format!("stem:{norm_dir}/{stem}")
    }
}

fn compute_aspect_ratio(width: Option<u32>, height: Option<u32>) -> Option<f64> {
    match (width, height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Some(f64::from(w) / f64::from(h)),
        _ => None,
    }
}

/// Loads `rel_path -> ExistingFileEntry` for `folder_id` to enable O(1) `(size, mtime_ms)` skips.
pub fn list_folder_file_index(
    conn: &Connection,
    folder_id: &str,
) -> DbResult<HashMap<String, ExistingFileEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, asset_id, rel_path, file_size, mtime_ms, partial_blake3, is_missing
         FROM asset_files
         WHERE folder_id = ?1",
    )?;
    let rows = stmt.query_map(params![folder_id], |row| {
        let size: i64 = row.get(3)?;
        Ok(ExistingFileEntry {
            file_id: row.get(0)?,
            asset_id: row.get(1)?,
            rel_path: row.get(2)?,
            file_size: u64::try_from(size).unwrap_or(0),
            mtime_ms: row.get(4)?,
            partial_blake3: row.get(5)?,
            is_missing: row.get::<_, i64>(6)? != 0,
        })
    })?;

    let mut map = HashMap::new();
    for row in rows {
        let entry = row?;
        map.insert(entry.rel_path.clone(), entry);
    }
    Ok(map)
}

/// `O(log N)` lookup of a single indexed file by `(folder_id, rel_path)` using the unique index.
pub fn get_folder_file_by_rel_path(
    conn: &Connection,
    folder_id: &str,
    rel_path: &str,
) -> DbResult<Option<ExistingFileEntry>> {
    let entry = conn
        .query_row(
            "SELECT id, asset_id, rel_path, file_size, mtime_ms, partial_blake3, is_missing
             FROM asset_files
             WHERE folder_id = ?1 AND rel_path = ?2
             LIMIT 1",
            params![folder_id, rel_path],
            |row| {
                let sz: i64 = row.get(3)?;
                Ok(ExistingFileEntry {
                    file_id: row.get(0)?,
                    asset_id: row.get(1)?,
                    rel_path: row.get(2)?,
                    file_size: u64::try_from(sz).unwrap_or(0),
                    mtime_ms: row.get(4)?,
                    partial_blake3: row.get(5)?,
                    is_missing: row.get::<_, i64>(6)? != 0,
                })
            },
        )
        .optional()?;
    Ok(entry)
}

/// Marks disappeared `asset_files` rows as `is_missing = 1` in bounded chunks.
pub fn mark_files_missing(conn: &Connection, file_ids: &[String], now_ms: i64) -> DbResult<usize> {
    if file_ids.is_empty() {
        return Ok(0);
    }

    let mut total_affected = 0usize;
    for chunk in file_ids.chunks(400) {
        let mut params: Vec<Value> = vec![Value::Integer(now_ms)];
        let mut placeholders = Vec::with_capacity(chunk.len());
        for (i, id) in chunk.iter().enumerate() {
            params.push(Value::Text(id.clone()));
            placeholders.push(format!("?{}", i + 2));
        }

        let sql = format!(
            "UPDATE asset_files
             SET is_missing = 1, updated_at_ms = ?1
             WHERE id IN ({}) AND is_missing = 0",
            placeholders.join(", ")
        );
        total_affected += conn.execute(&sql, params_from_iter(params))?;
    }
    Ok(total_affected)
}

/// Looks up a missing physical file matching `(partial_blake3, file_size)` within the same folder
/// for move/rename re-linking (never relinking across folders or into hidden/trashed assets).
pub fn find_missing_by_fingerprint(
    conn: &Connection,
    folder_id: &str,
    partial_blake3: &str,
    file_size: u64,
) -> DbResult<Option<ExistingFileEntry>> {
    let size_i64 = i64::try_from(file_size).unwrap_or(i64::MAX);
    let entry = conn
        .query_row(
            "SELECT af.id, af.asset_id, af.rel_path, af.file_size, af.mtime_ms, af.partial_blake3, af.is_missing
             FROM asset_files af
             JOIN assets a ON a.id = af.asset_id
             WHERE af.folder_id = ?1
               AND af.partial_blake3 = ?2
               AND af.file_size = ?3
               AND af.is_missing = 1
               AND a.deleted_at_ms IS NULL
               AND a.is_hidden = 0
             ORDER BY af.updated_at_ms DESC, af.id ASC
             LIMIT 1",
            params![folder_id, partial_blake3, size_i64],
            |row| {
                let sz: i64 = row.get(3)?;
                Ok(ExistingFileEntry {
                    file_id: row.get(0)?,
                    asset_id: row.get(1)?,
                    rel_path: row.get(2)?,
                    file_size: u64::try_from(sz).unwrap_or(0),
                    mtime_ms: row.get(4)?,
                    partial_blake3: row.get(5)?,
                    is_missing: row.get::<_, i64>(6)? != 0,
                })
            },
        )
        .optional()?;
    Ok(entry)
}

/// Re-links a previously missing `asset_files` row to a moved/renamed path, preserving its `asset_id`.
pub fn relink_moved_file(
    conn: &Connection,
    file_id: &str,
    new_folder_id: &str,
    new_rel_path: &str,
    new_mtime_ms: i64,
    now_ms: i64,
) -> DbResult<String> {
    let existing: Option<(String, String)> = conn
        .query_row(
            "SELECT asset_id, folder_id FROM asset_files WHERE id = ?1",
            params![file_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((asset_id, old_folder_id)) = existing else {
        return Err(DbError::NotFound(format!(
            "asset_file not found for relink: {file_id}"
        )));
    };

    with_savepoint(conn, "relink_moved_file", || {
        conn.execute(
            "UPDATE asset_files
             SET folder_id = ?1,
                 rel_path = ?2,
                 mtime_ms = ?3,
                 is_missing = 0,
                 updated_at_ms = ?4
             WHERE id = ?5",
            params![new_folder_id, new_rel_path, new_mtime_ms, now_ms, file_id],
        )?;

        if old_folder_id != new_folder_id {
            conn.execute(
                "UPDATE assets
                 SET folder_id = ?1, updated_at_ms = ?2
                 WHERE id = ?3",
                params![new_folder_id, now_ms, asset_id],
            )?;
            crate::repo::map::sync_asset_geo(conn, &asset_id)?;
        } else {
            conn.execute(
                "UPDATE assets
                 SET updated_at_ms = ?1
                 WHERE id = ?2",
                params![now_ms, asset_id],
            )?;
        }
        Ok(())
    })?;

    Ok(asset_id)
}

fn upsert_photo_meta(
    conn: &Connection,
    asset_id: &str,
    meta: &PhotoMetaView,
    raw_exif_json: Option<&str>,
) -> DbResult<()> {
    conn.execute(
        "INSERT INTO deterministic_photo_meta (
            asset_id, camera_make, camera_model, lens_model,
            focal_length_mm, focal_length_35mm, aperture_f, shutter_speed_sec,
            iso, orientation, color_space, gps_lat, gps_lon, gps_alt_m,
            geo_city, geo_admin1, geo_country, geo_country_code,
            taken_at_ms, raw_exif_json
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
            ?15, ?16, ?17, ?18, ?19, ?20
         )
         ON CONFLICT(asset_id) DO UPDATE SET
            camera_make = excluded.camera_make,
            camera_model = excluded.camera_model,
            lens_model = excluded.lens_model,
            focal_length_mm = excluded.focal_length_mm,
            focal_length_35mm = excluded.focal_length_35mm,
            aperture_f = excluded.aperture_f,
            shutter_speed_sec = excluded.shutter_speed_sec,
            iso = excluded.iso,
            orientation = excluded.orientation,
            color_space = excluded.color_space,
            gps_lat = excluded.gps_lat,
            gps_lon = excluded.gps_lon,
            gps_alt_m = excluded.gps_alt_m,
            geo_city = excluded.geo_city,
            geo_admin1 = excluded.geo_admin1,
            geo_country = excluded.geo_country,
            geo_country_code = excluded.geo_country_code,
            taken_at_ms = excluded.taken_at_ms,
            raw_exif_json = COALESCE(excluded.raw_exif_json, deterministic_photo_meta.raw_exif_json)",
        params![
            asset_id,
            meta.camera_make,
            meta.camera_model,
            meta.lens_model,
            meta.focal_length_mm,
            meta.focal_length_35mm.map(i64::from),
            meta.aperture_f,
            meta.shutter_speed_sec,
            meta.iso.map(i64::from),
            meta.orientation.map(i64::from),
            meta.color_space,
            meta.gps_lat,
            meta.gps_lon,
            meta.gps_alt_m,
            meta.geo_city,
            meta.geo_admin1,
            meta.geo_country,
            meta.geo_country_code,
            meta.taken_at_ms,
            raw_exif_json,
        ],
    )?;
    Ok(())
}

fn upsert_video_meta(
    conn: &Connection,
    asset_id: &str,
    meta: &VideoMetaView,
    raw_probe_json: Option<&str>,
) -> DbResult<()> {
    conn.execute(
        "INSERT INTO deterministic_video_meta (
            asset_id, duration_ms, video_codec, audio_codec, fps, bitrate_bps,
            bit_depth, color_transfer, color_primaries, is_hdr, rotation,
            gps_lat, gps_lon, geo_city, geo_admin1, geo_country, geo_country_code,
            taken_at_ms, raw_probe_json
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
            ?15, ?16, ?17, ?18, ?19
         )
         ON CONFLICT(asset_id) DO UPDATE SET
            duration_ms = excluded.duration_ms,
            video_codec = excluded.video_codec,
            audio_codec = excluded.audio_codec,
            fps = excluded.fps,
            bitrate_bps = excluded.bitrate_bps,
            bit_depth = excluded.bit_depth,
            color_transfer = excluded.color_transfer,
            color_primaries = excluded.color_primaries,
            is_hdr = excluded.is_hdr,
            rotation = excluded.rotation,
            gps_lat = excluded.gps_lat,
            gps_lon = excluded.gps_lon,
            geo_city = excluded.geo_city,
            geo_admin1 = excluded.geo_admin1,
            geo_country = excluded.geo_country,
            geo_country_code = excluded.geo_country_code,
            taken_at_ms = excluded.taken_at_ms,
            raw_probe_json = COALESCE(excluded.raw_probe_json, deterministic_video_meta.raw_probe_json)",
        params![
            asset_id,
            meta.duration_ms,
            meta.video_codec,
            meta.audio_codec,
            meta.fps,
            meta.bitrate_bps.and_then(|v| i64::try_from(v).ok()),
            meta.bit_depth.map(i64::from),
            meta.color_transfer,
            meta.color_primaries,
            bool_to_int(meta.is_hdr),
            meta.rotation,
            meta.gps_lat,
            meta.gps_lon,
            meta.geo_city,
            meta.geo_admin1,
            meta.geo_country,
            meta.geo_country_code,
            meta.taken_at_ms,
            raw_probe_json,
        ],
    )?;
    Ok(())
}

fn persist_side_metadata(
    conn: &Connection,
    asset_id: &str,
    data: &IngestedFileData,
) -> DbResult<()> {
    if let Some(pm) = &data.photo_meta {
        upsert_photo_meta(conn, asset_id, pm, data.raw_exif_json.as_deref())?;
    }
    if let Some(vm) = &data.video_meta {
        upsert_video_meta(conn, asset_id, vm, data.raw_probe_json.as_deref())?;
    }
    if let Some(hist) = &data.histogram {
        upsert_asset_histogram(conn, asset_id, hist)?;
    }
    Ok(())
}

struct NewAssetRow<'a> {
    asset_id: &'a str,
    kind: AssetKind,
    primary_file_id: &'a str,
    stack_key: Option<&'a str>,
    is_favorite: bool,
    data: &'a IngestedFileData,
    duration_override: Option<i64>,
    now: i64,
}

fn insert_asset_row(conn: &Connection, row: NewAssetRow<'_>) -> DbResult<()> {
    let aspect_ratio = compute_aspect_ratio(row.data.width, row.data.height);
    let duration_ms = row
        .duration_override
        .or(row.data.duration_ms)
        .or_else(|| row.data.video_meta.as_ref().map(|v| v.duration_ms));
    conn.execute(
        "INSERT INTO assets (
            id, folder_id, kind, primary_file_id, stack_key,
            capture_at_ms, capture_tz_offset_m, width, height, duration_ms, aspect_ratio,
            is_favorite, is_hidden, thumb_rel_path, thumb_ready, ai_stage,
            created_at_ms, updated_at_ms
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
            ?12, 0, ?13, ?14, 0, ?15, ?15
         )",
        params![
            row.asset_id,
            row.data.folder_id,
            row.kind.as_str(),
            row.primary_file_id,
            row.stack_key,
            row.data.capture_at_ms,
            row.data.capture_tz_offset_m.map(i64::from),
            row.data.width.map(i64::from),
            row.data.height.map(i64::from),
            duration_ms,
            aspect_ratio,
            bool_to_int(row.is_favorite),
            row.data.thumb_rel_path,
            bool_to_int(row.data.thumb_ready),
            row.now,
        ],
    )?;
    Ok(())
}

struct NewAssetFileRow<'a> {
    file_id: &'a str,
    asset_id: &'a str,
    role: FileRole,
    data: &'a IngestedFileData,
    ext: &'a str,
    cid: Option<&'a str>,
    now: i64,
}

fn insert_asset_file_row(conn: &Connection, row: NewAssetFileRow<'_>) -> DbResult<()> {
    let size_i64 = i64::try_from(row.data.file_size).unwrap_or(i64::MAX);
    let emb_off = row
        .data
        .embedded_video_offset
        .and_then(|v| i64::try_from(v).ok());
    let emb_len = row
        .data
        .embedded_video_length
        .and_then(|v| i64::try_from(v).ok());

    conn.execute(
        "INSERT INTO asset_files (
            id, asset_id, folder_id, rel_path, file_role, mime_type, extension,
            file_size, mtime_ms, partial_blake3, content_identifier,
            embedded_video_offset, embedded_video_length, is_missing,
            created_at_ms, updated_at_ms
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 0, ?14, ?14
         )",
        params![
            row.file_id,
            row.asset_id,
            row.data.folder_id,
            row.data.rel_path,
            row.role.as_str(),
            row.data.mime_type,
            row.ext,
            size_i64,
            row.data.mtime_ms,
            row.data.partial_blake3,
            row.cid,
            emb_off,
            emb_len,
            row.now,
        ],
    )?;
    Ok(())
}

fn find_live_still_partner_for_video(
    conn: &Connection,
    data: &IngestedFileData,
    cid: Option<&str>,
    stack_key: &str,
) -> DbResult<Option<String>> {
    // Priority 1: identical non-empty content_identifier in same folder_id
    if let Some(cid_val) = cid {
        let matched: Option<String> = conn
            .query_row(
                "SELECT a.id
                 FROM asset_files af
                 JOIN assets a ON a.id = af.asset_id
                 WHERE af.folder_id = ?1
                   AND af.content_identifier = ?2
                   AND af.is_missing = 0
                   AND af.file_role = 'primary'
                   AND a.kind = 'photo'
                   AND a.deleted_at_ms IS NULL
                   AND a.is_hidden = 0
                 LIMIT 1",
                params![data.folder_id, cid_val],
                |row| row.get(0),
            )
            .optional()?;
        if matched.is_some() {
            return Ok(matched);
        }
    }

    // Priority 2: same (folder_id, parent_dir, lower_stem) ONLY IF |still_ts - video_ts| <= 2000ms
    // AND video_duration_ms <= 6000ms.
    let video_duration_ms = data
        .duration_ms
        .or_else(|| data.video_meta.as_ref().map(|v| v.duration_ms));
    let Some(dur) = video_duration_ms else {
        return Ok(None);
    };
    if dur > 6000 {
        return Ok(None);
    }

    let video_ts = data
        .video_meta
        .as_ref()
        .and_then(|v| v.taken_at_ms)
        .unwrap_or(data.capture_at_ms);

    let mut stmt = conn.prepare(
        "SELECT
            a.id,
            a.capture_at_ms,
            pm.taken_at_ms,
            af.extension,
            af.content_identifier
         FROM assets a
         JOIN asset_files af ON af.id = a.primary_file_id
         LEFT JOIN deterministic_photo_meta pm ON pm.asset_id = a.id
         WHERE a.folder_id = ?1
           AND a.stack_key = ?2
           AND a.kind = 'photo'
           AND a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND af.is_missing = 0",
    )?;
    let rows = stmt.query_map(params![data.folder_id, stack_key], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;

    for row in rows {
        let (asset_id, asset_capture_ms, taken_at_ms, ext, cand_cid) = row?;
        if is_raw_extension(&ext) || ext.eq_ignore_ascii_case("livp") {
            continue;
        }
        if let (Some(c1), Some(c2)) = (cid, non_empty_cid(cand_cid.as_deref())) {
            if c1 != c2 {
                continue;
            }
        }
        let still_ts = taken_at_ms.unwrap_or(asset_capture_ms);
        if (still_ts - video_ts).abs() <= 2000 {
            return Ok(Some(asset_id));
        }
    }

    Ok(None)
}

struct TempVideoCandidate {
    asset_id: String,
    file_id: String,
    duration_ms: Option<i64>,
    is_favorite: bool,
    thumb_rel_path: Option<String>,
    thumb_ready: bool,
}

fn find_live_video_partner_for_still(
    conn: &Connection,
    data: &IngestedFileData,
    cid: Option<&str>,
    stack_key: &str,
) -> DbResult<Option<TempVideoCandidate>> {
    // Priority 1: identical non-empty content_identifier in same folder_id
    if let Some(cid_val) = cid {
        let matched: Option<TempVideoCandidate> = conn
            .query_row(
                "SELECT
                    a.id,
                    af.id,
                    COALESCE(vm.duration_ms, a.duration_ms),
                    a.is_favorite,
                    a.thumb_rel_path,
                    a.thumb_ready
                 FROM asset_files af
                 JOIN assets a ON a.id = af.asset_id
                 LEFT JOIN deterministic_video_meta vm ON vm.asset_id = a.id
                 WHERE af.folder_id = ?1
                   AND af.content_identifier = ?2
                   AND af.is_missing = 0
                   AND a.kind = 'video'
                   AND a.deleted_at_ms IS NULL
                   AND a.is_hidden = 0
                 LIMIT 1",
                params![data.folder_id, cid_val],
                |row| {
                    Ok(TempVideoCandidate {
                        asset_id: row.get(0)?,
                        file_id: row.get(1)?,
                        duration_ms: row.get(2)?,
                        is_favorite: row.get::<_, i64>(3)? != 0,
                        thumb_rel_path: row.get(4)?,
                        thumb_ready: row.get::<_, i64>(5)? != 0,
                    })
                },
            )
            .optional()?;
        if matched.is_some() {
            return Ok(matched);
        }
    }

    // Priority 2: same (folder_id, parent_dir, lower_stem) ONLY IF |still_ts - video_ts| <= 2000ms
    // AND video_duration_ms <= 6000ms.
    let still_ts = data
        .photo_meta
        .as_ref()
        .and_then(|p| p.taken_at_ms)
        .unwrap_or(data.capture_at_ms);

    let mut stmt = conn.prepare(
        "SELECT
            a.id,
            af.id,
            COALESCE(vm.duration_ms, a.duration_ms),
            a.capture_at_ms,
            vm.taken_at_ms,
            af.content_identifier,
            a.is_favorite,
            a.thumb_rel_path,
            a.thumb_ready
         FROM assets a
         JOIN asset_files af ON af.id = a.primary_file_id
         LEFT JOIN deterministic_video_meta vm ON vm.asset_id = a.id
         WHERE a.folder_id = ?1
           AND a.stack_key = ?2
           AND a.kind = 'video'
           AND a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND af.is_missing = 0",
    )?;
    let rows = stmt.query_map(params![data.folder_id, stack_key], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, i64>(6)? != 0,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, i64>(8)? != 0,
        ))
    })?;

    for row in rows {
        let (
            asset_id,
            file_id,
            dur_opt,
            vid_capture_ms,
            vid_taken_ms,
            cand_cid,
            is_favorite,
            thumb_rel_path,
            thumb_ready,
        ) = row?;

        if let (Some(c1), Some(c2)) = (cid, non_empty_cid(cand_cid.as_deref())) {
            if c1 != c2 {
                continue;
            }
        }
        let Some(dur) = dur_opt else {
            continue;
        };
        if dur > 6000 {
            continue;
        }
        let video_ts = vid_taken_ms.unwrap_or(vid_capture_ms);
        if (still_ts - video_ts).abs() <= 2000 {
            return Ok(Some(TempVideoCandidate {
                asset_id,
                file_id,
                duration_ms: Some(dur),
                is_favorite,
                thumb_rel_path,
                thumb_ready,
            }));
        }
    }

    Ok(None)
}

fn find_raw_companion_candidate(
    conn: &Connection,
    folder_id: &str,
    stack_key: &str,
    want_raw: bool,
) -> DbResult<Option<(String, String, bool)>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, af.id, af.extension, a.thumb_ready
         FROM assets a
         JOIN asset_files af ON af.id = a.primary_file_id
         WHERE a.folder_id = ?1
           AND a.stack_key = ?2
           AND a.kind = 'photo'
           AND a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND af.is_missing = 0",
    )?;
    let rows = stmt.query_map(params![folder_id, stack_key], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)? != 0,
        ))
    })?;

    for row in rows {
        let (asset_id, file_id, ext, thumb_ready) = row?;
        let matches = if want_raw {
            is_raw_extension(&ext)
        } else {
            is_companion_still_ext(&ext)
        };
        if matches {
            return Ok(Some((asset_id, file_id, thumb_ready)));
        }
    }

    Ok(None)
}

fn merge_temp_video_into_still_asset(
    conn: &Connection,
    still_asset_id: &str,
    temp_video_asset_id: &str,
) -> DbResult<()> {
    conn.execute(
        "UPDATE deterministic_video_meta SET asset_id = ?1 WHERE asset_id = ?2",
        params![still_asset_id, temp_video_asset_id],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO album_assets (album_id, asset_id, position, added_by, added_at_ms)
         SELECT album_id, ?1, position, added_by, added_at_ms
         FROM album_assets
         WHERE asset_id = ?2",
        params![still_asset_id, temp_video_asset_id],
    )?;
    conn.execute(
        "UPDATE albums SET cover_asset_id = ?1 WHERE cover_asset_id = ?2",
        params![still_asset_id, temp_video_asset_id],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO media_tags (
            asset_id, tag, source, state, confidence, model_id,
            created_by, created_at_ms, updated_at_ms
         )
         SELECT ?1, tag, source, state, confidence, model_id, created_by, created_at_ms, updated_at_ms
         FROM media_tags
         WHERE asset_id = ?2",
        params![still_asset_id, temp_video_asset_id],
    )?;
    conn.execute(
        "UPDATE user_overrides SET asset_id = ?1 WHERE asset_id = ?2",
        params![still_asset_id, temp_video_asset_id],
    )?;
    conn.execute(
        "UPDATE lru_cache_entries SET asset_id = ?1 WHERE asset_id = ?2",
        params![still_asset_id, temp_video_asset_id],
    )?;
    conn.execute(
        "UPDATE faces SET asset_id = ?1 WHERE asset_id = ?2",
        params![still_asset_id, temp_video_asset_id],
    )?;
    conn.execute(
        "UPDATE ocr_documents SET asset_id = ?1 WHERE asset_id = ?2",
        params![still_asset_id, temp_video_asset_id],
    )?;
    conn.execute(
        "UPDATE video_keyframes SET asset_id = ?1 WHERE asset_id = ?2",
        params![still_asset_id, temp_video_asset_id],
    )?;
    conn.execute(
        "DELETE FROM assets WHERE id = ?1",
        params![temp_video_asset_id],
    )?;
    Ok(())
}

/// Idempotently ingests a single physical file and reconciles Live Photo / Motion Photo /
/// RAW+JPEG stacks in an order-independent atomic savepoint, returning the logical `asset_id`.
pub fn reconcile_ingested_file(conn: &Connection, data: &IngestedFileData) -> DbResult<String> {
    with_savepoint(conn, "reconcile_ingested_file", || {
        let asset_id = reconcile_ingested_file_inner(conn, data)?;
        // Keep `assets.capture_at_ms` synchronized with effective metadata/overrides so indexed
        // timeline keyset pagination and bucket queries remain consistent with `v_assets_effective`.
        conn.execute(
            "UPDATE assets
             SET capture_at_ms = COALESCE(
                 (SELECT o.capture_at_ms FROM user_overrides o WHERE o.asset_id = assets.id),
                 (SELECT p.taken_at_ms FROM deterministic_photo_meta p WHERE p.asset_id = assets.id),
                 (SELECT v.taken_at_ms FROM deterministic_video_meta v WHERE v.asset_id = assets.id),
                 capture_at_ms
             )
             WHERE id = ?1",
            params![&asset_id],
        )?;
        crate::repo::map::sync_asset_geo(conn, &asset_id)?;
        Ok(asset_id)
    })
}

fn reconcile_ingested_file_inner(conn: &Connection, data: &IngestedFileData) -> DbResult<String> {
    let ext = data.extension.trim().to_ascii_lowercase();
    let cid = non_empty_cid(data.content_identifier.as_deref());
    let stack_key = stem_stack_key(&data.parent_dir, &data.lower_stem);
    let now = now_ms();

    // Idempotent update if `(folder_id, rel_path)` already exists in `asset_files`.
    let existing: Option<(String, String, String)> = conn
        .query_row(
            "SELECT id, asset_id, file_role
                 FROM asset_files
                 WHERE folder_id = ?1 AND rel_path = ?2",
            params![data.folder_id, data.rel_path],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;

    if let Some((existing_file_id, existing_asset_id, existing_role)) = existing {
        let size_i64 = i64::try_from(data.file_size).unwrap_or(i64::MAX);
        let emb_off = data
            .embedded_video_offset
            .and_then(|v| i64::try_from(v).ok());
        let emb_len = data
            .embedded_video_length
            .and_then(|v| i64::try_from(v).ok());

        conn.execute(
            "UPDATE asset_files
                 SET mime_type = ?1,
                     extension = ?2,
                     file_size = ?3,
                     mtime_ms = ?4,
                     partial_blake3 = ?5,
                     content_identifier = ?6,
                     embedded_video_offset = ?7,
                     embedded_video_length = ?8,
                     is_missing = 0,
                     updated_at_ms = ?9
                 WHERE id = ?10",
            params![
                data.mime_type,
                ext,
                size_i64,
                data.mtime_ms,
                data.partial_blake3,
                cid,
                emb_off,
                emb_len,
                now,
                existing_file_id,
            ],
        )?;

        let duration_ms = data
            .duration_ms
            .or_else(|| data.video_meta.as_ref().map(|v| v.duration_ms));
        if existing_role == FileRole::Primary.as_str() || existing_role == FileRole::Raw.as_str() {
            let aspect_ratio = compute_aspect_ratio(data.width, data.height);
            let upgraded_kind: Option<&str> = if !data.is_video
                && !data.is_raw
                && !data.is_livp
                && data.embedded_video_offset.is_some()
                && data.embedded_video_length.is_some()
            {
                Some(AssetKind::MotionPhoto.as_str())
            } else {
                None
            };
            conn.execute(
                    "UPDATE assets
                     SET kind = COALESCE(?1, kind),
                         capture_at_ms = COALESCE((SELECT o.capture_at_ms FROM user_overrides o WHERE o.asset_id = assets.id), ?2),
                         capture_tz_offset_m = COALESCE(?3, capture_tz_offset_m),
                         width = COALESCE(?4, width),
                         height = COALESCE(?5, height),
                         aspect_ratio = COALESCE(?6, aspect_ratio),
                         duration_ms = COALESCE(?7, duration_ms),
                         thumb_rel_path = CASE WHEN ?8 = 1 THEN COALESCE(?9, thumb_rel_path) ELSE thumb_rel_path END,
                         thumb_ready = CASE WHEN ?8 = 1 THEN 1 ELSE thumb_ready END,
                         updated_at_ms = ?10
                     WHERE id = ?11",
                    params![
                        upgraded_kind,
                        data.capture_at_ms,
                        data.capture_tz_offset_m.map(i64::from),
                        data.width.map(i64::from),
                        data.height.map(i64::from),
                        aspect_ratio,
                        duration_ms,
                        bool_to_int(data.thumb_ready),
                        data.thumb_rel_path,
                        now,
                        existing_asset_id,
                    ],
                )?;
        } else if existing_role == FileRole::JpegCompanion.as_str() && data.thumb_ready {
            conn.execute(
                "UPDATE assets
                     SET thumb_rel_path = COALESCE(?1, thumb_rel_path),
                         thumb_ready = 1,
                         updated_at_ms = ?2
                     WHERE id = ?3",
                params![data.thumb_rel_path, now, existing_asset_id],
            )?;
        } else if duration_ms.is_some() {
            conn.execute(
                "UPDATE assets
                     SET duration_ms = COALESCE(?1, duration_ms),
                         updated_at_ms = ?2
                     WHERE id = ?3",
                params![duration_ms, now, existing_asset_id],
            )?;
        }

        persist_side_metadata(conn, &existing_asset_id, data)?;
        return Ok(existing_asset_id);
    }

    let new_file_id = uuid::Uuid::new_v4().to_string();
    let new_asset_id = data
        .asset_id_hint
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // 1. Android Motion Photo (1-file)
    if !data.is_video
        && !data.is_raw
        && !data.is_livp
        && data.embedded_video_offset.is_some()
        && data.embedded_video_length.is_some()
    {
        insert_asset_row(
            conn,
            NewAssetRow {
                asset_id: &new_asset_id,
                kind: AssetKind::MotionPhoto,
                primary_file_id: &new_file_id,
                stack_key: Some(&stack_key),
                is_favorite: false,
                data,
                duration_override: None,
                now,
            },
        )?;
        insert_asset_file_row(
            conn,
            NewAssetFileRow {
                file_id: &new_file_id,
                asset_id: &new_asset_id,
                role: FileRole::Primary,
                data,
                ext: &ext,
                cid,
                now,
            },
        )?;
        persist_side_metadata(conn, &new_asset_id, data)?;
        return Ok(new_asset_id);
    }

    // 2. Apple .livp Archive (1-file)
    if data.is_livp || ext == "livp" {
        insert_asset_row(
            conn,
            NewAssetRow {
                asset_id: &new_asset_id,
                kind: AssetKind::LivePhoto,
                primary_file_id: &new_file_id,
                stack_key: Some(&stack_key),
                is_favorite: false,
                data,
                duration_override: None,
                now,
            },
        )?;
        insert_asset_file_row(
            conn,
            NewAssetFileRow {
                file_id: &new_file_id,
                asset_id: &new_asset_id,
                role: FileRole::Primary,
                data,
                ext: &ext,
                cid,
                now,
            },
        )?;
        persist_side_metadata(conn, &new_asset_id, data)?;
        return Ok(new_asset_id);
    }

    // 3. Video file (check if pairing onto an already-ingested Live Photo still)
    if data.is_video {
        if let Some(still_asset_id) =
            find_live_still_partner_for_video(conn, data, cid, &stack_key)?
        {
            insert_asset_file_row(
                conn,
                NewAssetFileRow {
                    file_id: &new_file_id,
                    asset_id: &still_asset_id,
                    role: FileRole::LiveVideo,
                    data,
                    ext: &ext,
                    cid,
                    now,
                },
            )?;
            let dur = data
                .duration_ms
                .or_else(|| data.video_meta.as_ref().map(|v| v.duration_ms));
            conn.execute(
                "UPDATE assets
                     SET kind = 'live_photo',
                         duration_ms = COALESCE(?1, duration_ms),
                         updated_at_ms = ?2
                     WHERE id = ?3",
                params![dur, now, still_asset_id],
            )?;
            if let Some(vm) = &data.video_meta {
                upsert_video_meta(conn, &still_asset_id, vm, data.raw_probe_json.as_deref())?;
            }
            return Ok(still_asset_id);
        }

        insert_asset_row(
            conn,
            NewAssetRow {
                asset_id: &new_asset_id,
                kind: AssetKind::Video,
                primary_file_id: &new_file_id,
                stack_key: Some(&stack_key),
                is_favorite: false,
                data,
                duration_override: None,
                now,
            },
        )?;
        insert_asset_file_row(
            conn,
            NewAssetFileRow {
                file_id: &new_file_id,
                asset_id: &new_asset_id,
                role: FileRole::Primary,
                data,
                ext: &ext,
                cid,
                now,
            },
        )?;
        persist_side_metadata(conn, &new_asset_id, data)?;
        return Ok(new_asset_id);
    }

    // 4. RAW file (check if pairing with an already-ingested JPEG/HEIC companion)
    if data.is_raw {
        if let Some((comp_asset_id, comp_file_id, comp_thumb_ready)) =
            find_raw_companion_candidate(conn, &data.folder_id, &stack_key, false)?
        {
            insert_asset_file_row(
                conn,
                NewAssetFileRow {
                    file_id: &new_file_id,
                    asset_id: &comp_asset_id,
                    role: FileRole::Raw,
                    data,
                    ext: &ext,
                    cid,
                    now,
                },
            )?;
            conn.execute(
                "UPDATE asset_files
                     SET file_role = 'jpeg_companion', updated_at_ms = ?1
                     WHERE id = ?2",
                params![now, comp_file_id],
            )?;
            conn.execute(
                    "UPDATE assets
                     SET kind = 'raw_jpg_stack',
                         primary_file_id = ?1,
                         thumb_rel_path = CASE WHEN ?2 = 0 AND ?3 = 1 THEN COALESCE(?4, thumb_rel_path) ELSE thumb_rel_path END,
                         thumb_ready = CASE WHEN ?2 = 0 AND ?3 = 1 THEN 1 ELSE thumb_ready END,
                         updated_at_ms = ?5
                     WHERE id = ?6",
                    params![
                        new_file_id,
                        bool_to_int(comp_thumb_ready),
                        bool_to_int(data.thumb_ready),
                        data.thumb_rel_path,
                        now,
                        comp_asset_id,
                    ],
                )?;
            if let Some(pm) = &data.photo_meta {
                let has_pm: Option<i64> = conn
                    .query_row(
                        "SELECT 1 FROM deterministic_photo_meta WHERE asset_id = ?1",
                        params![comp_asset_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if has_pm.is_none() {
                    upsert_photo_meta(conn, &comp_asset_id, pm, data.raw_exif_json.as_deref())?;
                }
            }
            return Ok(comp_asset_id);
        }

        insert_asset_row(
            conn,
            NewAssetRow {
                asset_id: &new_asset_id,
                kind: AssetKind::Photo,
                primary_file_id: &new_file_id,
                stack_key: Some(&stack_key),
                is_favorite: false,
                data,
                duration_override: None,
                now,
            },
        )?;
        insert_asset_file_row(
            conn,
            NewAssetFileRow {
                file_id: &new_file_id,
                asset_id: &new_asset_id,
                role: FileRole::Primary,
                data,
                ext: &ext,
                cid,
                now,
            },
        )?;
        persist_side_metadata(conn, &new_asset_id, data)?;
        return Ok(new_asset_id);
    }

    // 5. Still photo (check Live Photo video partner first, then RAW+JPG partner)
    if let Some(temp_video) = find_live_video_partner_for_still(conn, data, cid, &stack_key)? {
        let use_thumb_rel = if data.thumb_ready {
            data.thumb_rel_path.clone()
        } else {
            data.thumb_rel_path
                .clone()
                .or(temp_video.thumb_rel_path.clone())
        };
        let use_thumb_ready = data.thumb_ready || temp_video.thumb_ready;
        let mut merged_data = data.clone();
        merged_data.thumb_rel_path = use_thumb_rel;
        merged_data.thumb_ready = use_thumb_ready;

        insert_asset_row(
            conn,
            NewAssetRow {
                asset_id: &new_asset_id,
                kind: AssetKind::LivePhoto,
                primary_file_id: &new_file_id,
                stack_key: Some(&stack_key),
                is_favorite: temp_video.is_favorite,
                data: &merged_data,
                duration_override: temp_video.duration_ms,
                now,
            },
        )?;
        insert_asset_file_row(
            conn,
            NewAssetFileRow {
                file_id: &new_file_id,
                asset_id: &new_asset_id,
                role: FileRole::Primary,
                data,
                ext: &ext,
                cid,
                now,
            },
        )?;
        conn.execute(
            "UPDATE asset_files
                 SET asset_id = ?1, file_role = 'live_video', updated_at_ms = ?2
                 WHERE id = ?3",
            params![new_asset_id, now, temp_video.file_id],
        )?;
        merge_temp_video_into_still_asset(conn, &new_asset_id, &temp_video.asset_id)?;
        persist_side_metadata(conn, &new_asset_id, data)?;
        return Ok(new_asset_id);
    }

    if is_companion_still_ext(&ext) {
        if let Some((raw_asset_id, raw_file_id, _)) =
            find_raw_companion_candidate(conn, &data.folder_id, &stack_key, true)?
        {
            insert_asset_file_row(
                conn,
                NewAssetFileRow {
                    file_id: &new_file_id,
                    asset_id: &raw_asset_id,
                    role: FileRole::JpegCompanion,
                    data,
                    ext: &ext,
                    cid,
                    now,
                },
            )?;
            conn.execute(
                "UPDATE asset_files
                     SET file_role = 'raw', updated_at_ms = ?1
                     WHERE id = ?2",
                params![now, raw_file_id],
            )?;
            let aspect_ratio = compute_aspect_ratio(data.width, data.height);
            conn.execute(
                    "UPDATE assets
                     SET kind = 'raw_jpg_stack',
                         primary_file_id = ?1,
                         width = COALESCE(?2, width),
                         height = COALESCE(?3, height),
                         aspect_ratio = COALESCE(?4, aspect_ratio),
                         thumb_rel_path = CASE WHEN ?5 = 1 THEN COALESCE(?6, thumb_rel_path) ELSE thumb_rel_path END,
                         thumb_ready = CASE WHEN ?5 = 1 THEN 1 ELSE thumb_ready END,
                         updated_at_ms = ?7
                     WHERE id = ?8",
                    params![
                        raw_file_id,
                        data.width.map(i64::from),
                        data.height.map(i64::from),
                        aspect_ratio,
                        bool_to_int(data.thumb_ready),
                        data.thumb_rel_path,
                        now,
                        raw_asset_id,
                    ],
                )?;
            persist_side_metadata(conn, &raw_asset_id, data)?;
            return Ok(raw_asset_id);
        }
    }

    // Standalone still photo
    insert_asset_row(
        conn,
        NewAssetRow {
            asset_id: &new_asset_id,
            kind: AssetKind::Photo,
            primary_file_id: &new_file_id,
            stack_key: Some(&stack_key),
            is_favorite: false,
            data,
            duration_override: None,
            now,
        },
    )?;
    insert_asset_file_row(
        conn,
        NewAssetFileRow {
            file_id: &new_file_id,
            asset_id: &new_asset_id,
            role: FileRole::Primary,
            data,
            ext: &ext,
            cid,
            now,
        },
    )?;
    persist_side_metadata(conn, &new_asset_id, data)?;
    Ok(new_asset_id)
}

/// Removes missing `asset_files` rows in `folder_id`, downgrading split stacks or deleting
/// orphaned assets whose files all vanished.
pub fn cleanup_missing_files(conn: &Connection, folder_id: &str, now_ms: i64) -> DbResult<usize> {
    with_savepoint(conn, "cleanup_missing_files", || {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT asset_id
             FROM asset_files
             WHERE folder_id = ?1 AND is_missing = 1",
        )?;
        let affected_assets: Vec<String> = stmt
            .query_map(params![folder_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        if affected_assets.is_empty() {
            return Ok(0);
        }

        let removed = conn.execute(
            "DELETE FROM asset_files WHERE folder_id = ?1 AND is_missing = 1",
            params![folder_id],
        )?;

        for asset_id in affected_assets {
            let mut rem_stmt = conn.prepare(
                "SELECT id, file_role, extension
                 FROM asset_files
                 WHERE asset_id = ?1
                 ORDER BY created_at_ms ASC, id ASC",
            )?;
            let remaining: Vec<(String, String, String)> = rem_stmt
                .query_map(params![asset_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(rem_stmt);

            if remaining.is_empty() {
                conn.execute("DELETE FROM assets WHERE id = ?1", params![asset_id])?;
                continue;
            }

            if remaining.len() == 1 {
                let (rem_file_id, rem_role, rem_ext) = &remaining[0];
                let current_kind: Option<String> = conn
                    .query_row(
                        "SELECT kind FROM assets WHERE id = ?1",
                        params![asset_id],
                        |row| row.get(0),
                    )
                    .optional()?;

                match current_kind.as_deref() {
                    Some("live_photo") if !rem_ext.eq_ignore_ascii_case("livp") => {
                        let new_kind = if rem_role == FileRole::LiveVideo.as_str() {
                            AssetKind::Video.as_str()
                        } else {
                            AssetKind::Photo.as_str()
                        };
                        conn.execute(
                            "UPDATE asset_files
                             SET file_role = 'primary', updated_at_ms = ?1
                             WHERE id = ?2",
                            params![now_ms, rem_file_id],
                        )?;
                        if new_kind == AssetKind::Photo.as_str() {
                            conn.execute(
                                "DELETE FROM deterministic_video_meta WHERE asset_id = ?1",
                                params![asset_id],
                            )?;
                            conn.execute(
                                "UPDATE assets
                                 SET kind = 'photo',
                                     primary_file_id = ?1,
                                     duration_ms = NULL,
                                     updated_at_ms = ?2
                                 WHERE id = ?3",
                                params![rem_file_id, now_ms, asset_id],
                            )?;
                            crate::repo::map::sync_asset_geo(conn, &asset_id)?;
                        } else {
                            conn.execute(
                                "UPDATE assets
                                 SET kind = 'video',
                                     primary_file_id = ?1,
                                     updated_at_ms = ?2
                                 WHERE id = ?3",
                                params![rem_file_id, now_ms, asset_id],
                            )?;
                        }
                    }
                    Some("raw_jpg_stack") => {
                        conn.execute(
                            "UPDATE asset_files
                             SET file_role = 'primary', updated_at_ms = ?1
                             WHERE id = ?2",
                            params![now_ms, rem_file_id],
                        )?;
                        conn.execute(
                            "UPDATE assets
                             SET kind = 'photo',
                                 primary_file_id = ?1,
                                 updated_at_ms = ?2
                             WHERE id = ?3",
                            params![rem_file_id, now_ms, asset_id],
                        )?;
                    }
                    _ => {
                        conn.execute(
                            "UPDATE assets
                             SET primary_file_id = COALESCE(primary_file_id, ?1),
                                 updated_at_ms = ?2
                             WHERE id = ?3",
                            params![rem_file_id, now_ms, asset_id],
                        )?;
                    }
                }
            }
        }

        Ok(removed)
    })
}
