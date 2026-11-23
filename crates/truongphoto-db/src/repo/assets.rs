use crate::error::{DbError, DbResult};
use crate::rbac_sql;
use crate::repo::{bool_to_int, folders, now_ms, tags, to_sql_err, with_savepoint};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension, Row};
use std::collections::{HashMap, HashSet};
use truongphoto_core::models::{
    AssetDetail, AssetFileView, AssetKind, AssetSummary, FileRole, FolderRecord, Histogram256,
    PhotoMetaView, PlaybackInfo, PlaybackMode, TimelineBucket, UpdateOverridesRequest,
    UserOverridesView, VideoMetaView, HISTOGRAM_BINS,
};
use truongphoto_core::rbac::RbacScope;

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 500;

/// Query filters and pagination parameters for `list_assets`.
#[derive(Debug, Clone)]
pub struct AssetListQuery {
    pub folder_id: Option<String>,
    pub favorite: Option<bool>,
    pub before_ms: Option<i64>,
    pub cursor: Option<String>,
    pub limit: usize,
}

impl Default for AssetListQuery {
    fn default() -> Self {
        Self {
            folder_id: None,
            favorite: None,
            before_ms: None,
            cursor: None,
            limit: DEFAULT_LIMIT,
        }
    }
}

/// Logical trashed asset and its physical files prepared for permanent purge.
#[derive(Debug, Clone)]
pub struct PurgeCandidate {
    pub asset_id: String,
    pub folder_id: String,
    pub folder_root_path: String,
    pub folder_volume_uuid: Option<String>,
    pub allow_disk_delete: bool,
    pub is_offline: bool,
    pub thumb_rel_path: Option<String>,
    pub files: Vec<AssetFileView>,
}

pub(crate) const ASSET_SUMMARY_COLUMNS: &str = "
    v.id,
    v.folder_id,
    v.kind,
    v.effective_capture_at_ms,
    v.width,
    v.height,
    v.effective_duration_ms,
    v.aspect_ratio,
    v.is_favorite,
    v.deleted_at_ms,
    v.thumb_rel_path,
    v.thumb_ready,
    v.effective_title,
    v.effective_rating,
    v.effective_gps_lat,
    v.effective_gps_lon,
    v.effective_geo_city,
    v.effective_geo_country,
    v.camera_model
";

pub(crate) fn row_to_asset_summary(row: &Row<'_>) -> rusqlite::Result<AssetSummary> {
    let kind_str: String = row.get(2)?;
    let kind = kind_str
        .parse::<AssetKind>()
        .map_err(|e| to_sql_err(e, 2))?;
    let width: Option<i64> = row.get(4)?;
    let height: Option<i64> = row.get(5)?;
    let rating: Option<i64> = row.get(13)?;

    Ok(AssetSummary {
        id: row.get(0)?,
        folder_id: row.get(1)?,
        kind,
        capture_at_ms: row.get(3)?,
        width: width.and_then(|v| u32::try_from(v).ok()),
        height: height.and_then(|v| u32::try_from(v).ok()),
        duration_ms: row.get(6)?,
        aspect_ratio: row.get(7)?,
        is_favorite: row.get::<_, i64>(8)? != 0,
        deleted_at_ms: row.get(9)?,
        thumb_rel_path: row.get(10)?,
        thumb_ready: row.get::<_, i64>(11)? != 0,
        title: row.get(12)?,
        rating: rating.and_then(|v| u8::try_from(v).ok()),
        gps_lat: row.get(14)?,
        gps_lon: row.get(15)?,
        geo_city: row.get(16)?,
        geo_country: row.get(17)?,
        camera_model: row.get(18)?,
    })
}

fn row_to_asset_file(row: &Row<'_>) -> rusqlite::Result<AssetFileView> {
    let role_str: String = row.get(4)?;
    let file_role = role_str.parse::<FileRole>().map_err(|e| to_sql_err(e, 4))?;
    let file_size: i64 = row.get(7)?;
    let emb_off: Option<i64> = row.get(11)?;
    let emb_len: Option<i64> = row.get(12)?;

    Ok(AssetFileView {
        id: row.get(0)?,
        asset_id: row.get(1)?,
        folder_id: row.get(2)?,
        rel_path: row.get(3)?,
        file_role,
        mime_type: row.get(5)?,
        extension: row.get(6)?,
        file_size: u64::try_from(file_size).unwrap_or(0),
        mtime_ms: row.get(8)?,
        partial_blake3: row.get(9)?,
        content_identifier: row.get(10)?,
        embedded_video_offset: emb_off.and_then(|v| u64::try_from(v).ok()),
        embedded_video_length: emb_len.and_then(|v| u64::try_from(v).ok()),
        is_missing: row.get::<_, i64>(13)? != 0,
    })
}

fn load_asset_files(conn: &Connection, asset_id: &str) -> DbResult<Vec<AssetFileView>> {
    let mut stmt = conn.prepare(
        "SELECT
            id, asset_id, folder_id, rel_path, file_role, mime_type, extension,
            file_size, mtime_ms, partial_blake3, content_identifier,
            embedded_video_offset, embedded_video_length, is_missing
         FROM asset_files
         WHERE asset_id = ?1
         ORDER BY CASE file_role
            WHEN 'primary' THEN 0
            WHEN 'raw' THEN 1
            WHEN 'jpeg_companion' THEN 2
            WHEN 'live_video' THEN 3
            ELSE 4
         END, rel_path ASC",
    )?;
    let rows = stmt.query_map(params![asset_id], row_to_asset_file)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn load_photo_meta(conn: &Connection, asset_id: &str) -> DbResult<Option<PhotoMetaView>> {
    let row = conn
        .query_row(
            "SELECT
                camera_make, camera_model, lens_model, focal_length_mm, focal_length_35mm,
                aperture_f, shutter_speed_sec, iso, orientation, color_space,
                gps_lat, gps_lon, gps_alt_m, geo_city, geo_admin1, geo_country,
                geo_country_code, taken_at_ms
             FROM deterministic_photo_meta
             WHERE asset_id = ?1",
            params![asset_id],
            |row| {
                let fl35: Option<i64> = row.get(4)?;
                let iso: Option<i64> = row.get(7)?;
                let orientation: Option<i64> = row.get(8)?;
                Ok(PhotoMetaView {
                    camera_make: row.get(0)?,
                    camera_model: row.get(1)?,
                    lens_model: row.get(2)?,
                    focal_length_mm: row.get(3)?,
                    focal_length_35mm: fl35.and_then(|v| u32::try_from(v).ok()),
                    aperture_f: row.get(5)?,
                    shutter_speed_sec: row.get(6)?,
                    iso: iso.and_then(|v| u32::try_from(v).ok()),
                    orientation: orientation.and_then(|v| u16::try_from(v).ok()),
                    color_space: row.get(9)?,
                    gps_lat: row.get(10)?,
                    gps_lon: row.get(11)?,
                    gps_alt_m: row.get(12)?,
                    geo_city: row.get(13)?,
                    geo_admin1: row.get(14)?,
                    geo_country: row.get(15)?,
                    geo_country_code: row.get(16)?,
                    taken_at_ms: row.get(17)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

fn load_video_meta(conn: &Connection, asset_id: &str) -> DbResult<Option<VideoMetaView>> {
    let row = conn
        .query_row(
            "SELECT
                duration_ms, video_codec, audio_codec, fps, bitrate_bps, bit_depth,
                color_transfer, color_primaries, is_hdr, rotation,
                gps_lat, gps_lon, geo_city, geo_admin1, geo_country, geo_country_code,
                taken_at_ms
             FROM deterministic_video_meta
             WHERE asset_id = ?1",
            params![asset_id],
            |row| {
                let bitrate: Option<i64> = row.get(4)?;
                let bit_depth: Option<i64> = row.get(5)?;
                Ok(VideoMetaView {
                    duration_ms: row.get(0)?,
                    video_codec: row.get(1)?,
                    audio_codec: row.get(2)?,
                    fps: row.get(3)?,
                    bitrate_bps: bitrate.and_then(|v| u64::try_from(v).ok()),
                    bit_depth: bit_depth.and_then(|v| u8::try_from(v).ok()),
                    color_transfer: row.get(6)?,
                    color_primaries: row.get(7)?,
                    is_hdr: row.get::<_, i64>(8)? != 0,
                    rotation: row.get::<_, Option<i32>>(9)?.unwrap_or(0),
                    gps_lat: row.get(10)?,
                    gps_lon: row.get(11)?,
                    geo_city: row.get(12)?,
                    geo_admin1: row.get(13)?,
                    geo_country: row.get(14)?,
                    geo_country_code: row.get(15)?,
                    taken_at_ms: row.get(16)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

fn load_user_overrides(conn: &Connection, asset_id: &str) -> DbResult<Option<UserOverridesView>> {
    let row = conn
        .query_row(
            "SELECT
                title, description, capture_at_ms, gps_lat, gps_lon,
                geo_city, geo_admin1, geo_country, geo_country_code,
                rating, updated_by, updated_at_ms
             FROM user_overrides
             WHERE asset_id = ?1",
            params![asset_id],
            |row| {
                let rating: Option<i64> = row.get(9)?;
                Ok(UserOverridesView {
                    title: row.get(0)?,
                    description: row.get(1)?,
                    capture_at_ms: row.get(2)?,
                    gps_lat: row.get(3)?,
                    gps_lon: row.get(4)?,
                    geo_city: row.get(5)?,
                    geo_admin1: row.get(6)?,
                    geo_country: row.get(7)?,
                    geo_country_code: row.get(8)?,
                    rating: rating.and_then(|v| u8::try_from(v).ok()),
                    updated_by: row.get(10)?,
                    updated_at_ms: row.get(11)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

struct RawHistogramBlobs {
    r: Vec<u8>,
    g: Vec<u8>,
    b: Vec<u8>,
    luma: Vec<u8>,
}

fn load_histogram(conn: &Connection, asset_id: &str) -> DbResult<Option<Histogram256>> {
    let row: Option<RawHistogramBlobs> = conn
        .query_row(
            "SELECT r_bins, g_bins, b_bins, luma_bins
             FROM asset_histograms
             WHERE asset_id = ?1",
            params![asset_id],
            |row| {
                Ok(RawHistogramBlobs {
                    r: row.get(0)?,
                    g: row.get(1)?,
                    b: row.get(2)?,
                    luma: row.get(3)?,
                })
            },
        )
        .optional()?;

    let Some(blobs) = row else {
        return Ok(None);
    };

    let (Some(r), Some(g), Some(b), Some(luma)) = (
        Histogram256::decode_bins_le(&blobs.r),
        Histogram256::decode_bins_le(&blobs.g),
        Histogram256::decode_bins_le(&blobs.b),
        Histogram256::decode_bins_le(&blobs.luma),
    ) else {
        return Ok(None);
    };

    Ok(Some(Histogram256 { r, g, b, luma }))
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

/// Constructs client playback and rendition URLs from asset metadata.
pub fn build_playback_info(
    asset_id: &str,
    kind: AssetKind,
    files: &[AssetFileView],
    video_meta: Option<&VideoMetaView>,
) -> PlaybackInfo {
    let has_raw = kind == AssetKind::RawJpgStack
        || files
            .iter()
            .any(|f| f.file_role == FileRole::Raw || is_raw_extension(&f.extension));

    let live_video_url = matches!(kind, AssetKind::LivePhoto | AssetKind::MotionPhoto)
        .then(|| format!("/api/media/{asset_id}/live-video"));

    let (mode, direct_url, hls_url) = if kind == AssetKind::Video {
        let primary = files
            .iter()
            .find(|f| f.file_role == FileRole::Primary)
            .or_else(|| files.first());
        let container_ok = primary
            .map(|f| {
                matches!(
                    f.extension.trim().to_ascii_lowercase().as_str(),
                    "mp4" | "m4v" | "webm"
                )
            })
            .unwrap_or(false);
        let codec_ok = video_meta
            .and_then(|vm| vm.video_codec.as_deref())
            .map(|c| {
                matches!(
                    c.trim().to_ascii_lowercase().as_str(),
                    "h264" | "avc1" | "vp8" | "vp9"
                )
            })
            .unwrap_or(false);
        let sdr_8bit = video_meta
            .map(|vm| !vm.is_hdr && vm.bit_depth.unwrap_or(8) <= 8)
            .unwrap_or(false);

        if container_ok && codec_ok && sdr_8bit {
            (
                PlaybackMode::Direct,
                Some(format!("/api/media/{asset_id}/original")),
                None,
            )
        } else {
            (
                PlaybackMode::Hls,
                None,
                Some(format!("/api/media/{asset_id}/hls/index.m3u8")),
            )
        }
    } else {
        (PlaybackMode::Image, None, None)
    };

    PlaybackInfo {
        mode,
        direct_url,
        hls_url,
        live_video_url,
        has_raw,
        preview_url: format!("/api/media/{asset_id}/preview"),
        thumb_url: format!("/api/media/{asset_id}/thumb"),
        original_url: format!("/api/media/{asset_id}/original"),
    }
}

pub(crate) fn parse_cursor(cursor: &str) -> DbResult<(i64, String)> {
    let Some((ts_str, id)) = cursor.split_once(':') else {
        return Err(DbError::InvalidArgument("invalid asset cursor".to_string()));
    };
    if id.is_empty() {
        return Err(DbError::InvalidArgument("invalid asset cursor".to_string()));
    }
    let ts = ts_str
        .parse::<i64>()
        .map_err(|_| DbError::InvalidArgument("invalid asset cursor timestamp".to_string()))?;
    Ok((ts, id.to_string()))
}

const ASSET_SUMMARY_JOIN_COLUMNS: &str = "
    a.id,
    a.folder_id,
    a.kind,
    COALESCE(o.capture_at_ms, p.taken_at_ms, v.taken_at_ms, a.capture_at_ms) AS effective_capture_at_ms,
    a.width,
    a.height,
    COALESCE(v.duration_ms, a.duration_ms) AS effective_duration_ms,
    a.aspect_ratio,
    a.is_favorite,
    a.deleted_at_ms,
    a.thumb_rel_path,
    a.thumb_ready,
    o.title AS effective_title,
    o.rating AS effective_rating,
    COALESCE(o.gps_lat, p.gps_lat, v.gps_lat) AS effective_gps_lat,
    COALESCE(o.gps_lon, p.gps_lon, v.gps_lon) AS effective_gps_lon,
    COALESCE(o.geo_city, p.geo_city, v.geo_city) AS effective_geo_city,
    COALESCE(o.geo_country, p.geo_country, v.geo_country) AS effective_geo_country,
    p.camera_model
";

#[inline]
fn year_month_from_unix_ms(ms: i64) -> (i32, u32) {
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = (y + i64::from(m <= 2)) as i32;
    (year, m)
}

/// Lists non-deleted, non-hidden assets with effective metadata overrides scoped by `scope`.
///
/// Returns `(items, next_cursor, total_count)`.
pub fn list_assets(
    conn: &Connection,
    scope: &RbacScope,
    query: &AssetListQuery,
) -> DbResult<(Vec<AssetSummary>, Option<String>, i64)> {
    let limit = if query.limit == 0 {
        DEFAULT_LIMIT
    } else {
        query.limit.min(MAX_LIMIT)
    };

    if let Some(folder_id) = &query.folder_id {
        if !scope.allows_folder(folder_id) {
            return Ok((Vec::new(), None, 0));
        }
    }

    let mut base_params: Vec<Value> = Vec::new();
    let rbac_clause = rbac_sql::folder_clause("a", scope, &mut base_params);
    let mut where_clauses = vec![
        "a.deleted_at_ms IS NULL".to_string(),
        "a.is_hidden = 0".to_string(),
        rbac_clause,
    ];

    if let Some(folder_id) = &query.folder_id {
        let idx = base_params.len() + 1;
        base_params.push(Value::Text(folder_id.clone()));
        where_clauses.push(format!("a.folder_id = ?{idx}"));
    }

    if let Some(favorite) = query.favorite {
        let idx = base_params.len() + 1;
        base_params.push(Value::Integer(i64::from(favorite)));
        where_clauses.push(format!("a.is_favorite = ?{idx}"));
    }

    if let Some(before_ms) = query.before_ms {
        let idx = base_params.len() + 1;
        base_params.push(Value::Integer(before_ms));
        where_clauses.push(format!("a.capture_at_ms <= ?{idx}"));
    }

    let base_where = where_clauses.join(" AND ");
    let count_sql = format!("SELECT COUNT(*) FROM assets a WHERE {base_where}");
    let total_count: i64 =
        conn.query_row(&count_sql, params_from_iter(base_params.clone()), |row| {
            row.get(0)
        })?;

    let mut page_params = base_params;
    let mut page_where = base_where;
    if let Some(cursor) = &query.cursor {
        let (cursor_ts, cursor_id) = parse_cursor(cursor)?;
        let ts_idx = page_params.len() + 1;
        let id_idx = page_params.len() + 2;
        page_params.push(Value::Integer(cursor_ts));
        page_params.push(Value::Text(cursor_id));
        page_where = format!(
            "{page_where} AND (a.capture_at_ms < ?{ts_idx} OR (a.capture_at_ms = ?{ts_idx} AND a.id < ?{id_idx}))"
        );
    }

    let limit_idx = page_params.len() + 1;
    page_params.push(Value::Integer((limit + 1) as i64));

    let list_sql = format!(
        "SELECT {ASSET_SUMMARY_JOIN_COLUMNS}, a.capture_at_ms AS cursor_capture_at_ms
         FROM assets a
         LEFT JOIN deterministic_photo_meta p ON p.asset_id = a.id
         LEFT JOIN deterministic_video_meta v ON v.asset_id = a.id
         LEFT JOIN user_overrides o ON o.asset_id = a.id
         WHERE {page_where}
         ORDER BY a.capture_at_ms DESC, a.id DESC
         LIMIT ?{limit_idx}"
    );

    let mut stmt = conn.prepare(&list_sql)?;
    let mut rows_with_cursor: Vec<(AssetSummary, i64)> = stmt
        .query_map(params_from_iter(page_params), |row| {
            let summary = row_to_asset_summary(row)?;
            let raw_capture_at_ms: i64 = row.get(19)?;
            Ok((summary, raw_capture_at_ms))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let next_cursor = if rows_with_cursor.len() > limit {
        rows_with_cursor.truncate(limit);
        rows_with_cursor
            .last()
            .map(|(item, raw_ts)| format!("{raw_ts}:{}", item.id))
    } else {
        None
    };
    let items: Vec<AssetSummary> = rows_with_cursor.into_iter().map(|(s, _)| s).collect();

    Ok((items, next_cursor, total_count))
}

/// Returns year/month asset count buckets ordered newest-first, scoped by `scope`.
pub fn list_timeline_buckets(
    conn: &Connection,
    scope: &RbacScope,
) -> DbResult<Vec<TimelineBucket>> {
    let mut params: Vec<Value> = Vec::new();
    let rbac_clause = rbac_sql::folder_clause("a", scope, &mut params);
    let sql = format!(
        "SELECT a.capture_at_ms
         FROM assets a
         WHERE a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND {rbac_clause}"
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params_from_iter(params))?;

    let mut counts: HashMap<(i32, u32), u64> = HashMap::new();
    let mut cur_ym: Option<((i32, u32), u64)> = None;

    while let Some(row) = rows.next()? {
        let ms: i64 = row.get(0)?;
        let ym = year_month_from_unix_ms(ms);
        match &mut cur_ym {
            Some((active_ym, run_count)) if *active_ym == ym => {
                *run_count += 1;
            }
            Some((active_ym, run_count)) => {
                *counts.entry(*active_ym).or_insert(0) += *run_count;
                *active_ym = ym;
                *run_count = 1;
            }
            None => {
                cur_ym = Some((ym, 1));
            }
        }
    }
    if let Some((active_ym, run_count)) = cur_ym {
        *counts.entry(active_ym).or_insert(0) += run_count;
    }

    let mut buckets: Vec<TimelineBucket> = counts
        .into_iter()
        .map(|((year, month), count)| TimelineBucket { year, month, count })
        .collect();
    buckets.sort_unstable_by(|a, b| b.year.cmp(&a.year).then_with(|| b.month.cmp(&a.month)));

    Ok(buckets)
}

/// Lists soft-deleted assets visible under `scope`, ordered by deletion time descending.
pub fn list_trash(conn: &Connection, scope: &RbacScope) -> DbResult<(Vec<AssetSummary>, i64)> {
    let mut params: Vec<Value> = Vec::new();
    let rbac_clause = rbac_sql::folder_clause("v", scope, &mut params);
    let sql = format!(
        "SELECT {ASSET_SUMMARY_COLUMNS}
         FROM v_assets_effective v
         WHERE v.deleted_at_ms IS NOT NULL
           AND {rbac_clause}
         ORDER BY v.deleted_at_ms DESC, v.id DESC"
    );

    let mut stmt = conn.prepare(&sql)?;
    let items = stmt
        .query_map(params_from_iter(params), row_to_asset_summary)?
        .collect::<Result<Vec<_>, _>>()?;
    let total_count = items.len() as i64;
    Ok((items, total_count))
}

/// Counts active (non-deleted, non-hidden) assets visible under `scope`.
pub fn count_active_assets(conn: &Connection, scope: &RbacScope) -> DbResult<u64> {
    let mut params: Vec<Value> = Vec::new();
    let rbac_clause = rbac_sql::folder_clause("a", scope, &mut params);
    let sql = format!(
        "SELECT COUNT(*)
         FROM assets a
         WHERE a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND {rbac_clause}"
    );
    let count: i64 = conn.query_row(&sql, params_from_iter(params), |row| row.get(0))?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// Loads full asset inspection metadata (`AssetDetail`) if visible under `scope`.
pub fn get_asset_detail(
    conn: &Connection,
    scope: &RbacScope,
    asset_id: &str,
) -> DbResult<Option<AssetDetail>> {
    get_asset_detail_for_user(conn, scope, asset_id, None)
}

/// Fetches full asset detail scoped by `scope`, filtering `album_ids` to albums accessible by `caller`.
pub fn get_asset_detail_for_user(
    conn: &Connection,
    scope: &RbacScope,
    asset_id: &str,
    caller: Option<(&str, truongphoto_core::Role)>,
) -> DbResult<Option<AssetDetail>> {
    let mut params: Vec<Value> = vec![Value::Text(asset_id.to_string())];
    let rbac_clause = rbac_sql::folder_clause("v", scope, &mut params);
    let trash_clause = if matches!(caller, Some((_, truongphoto_core::Role::Viewer))) {
        "AND v.deleted_at_ms IS NULL"
    } else {
        ""
    };
    let sql = format!(
        "SELECT {ASSET_SUMMARY_COLUMNS}
         FROM v_assets_effective v
         WHERE v.id = ?1
           AND v.is_hidden = 0
           {trash_clause}
           AND {rbac_clause}
         LIMIT 1"
    );

    let Some(summary) = conn
        .query_row(&sql, params_from_iter(params), row_to_asset_summary)
        .optional()?
    else {
        return Ok(None);
    };

    let files = load_asset_files(conn, asset_id)?;
    let photo_meta = load_photo_meta(conn, asset_id)?;
    let video_meta = load_video_meta(conn, asset_id)?;
    let overrides = load_user_overrides(conn, asset_id)?;
    let tags = tags::list_active_tags(conn, asset_id)?;
    let histogram = load_histogram(conn, asset_id)?;

    let album_ids = match caller {
        Some((user_id, role)) if role != truongphoto_core::Role::Admin => {
            let mut album_stmt = conn.prepare(
                "SELECT aa.album_id
                 FROM album_assets aa
                 JOIN albums al ON al.id = aa.album_id
                 WHERE aa.asset_id = ?1
                   AND (
                       al.owner_user_id = ?2
                       OR EXISTS (
                           SELECT 1 FROM album_shares s
                           WHERE s.album_id = al.id AND s.shared_with_user_id = ?2
                       )
                   )
                 ORDER BY aa.added_at_ms ASC, aa.album_id ASC",
            )?;
            let ids = album_stmt
                .query_map(params![asset_id, user_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            ids
        }
        _ => {
            let mut album_stmt = conn.prepare(
                "SELECT album_id
                 FROM album_assets
                 WHERE asset_id = ?1
                 ORDER BY added_at_ms ASC, album_id ASC",
            )?;
            let ids = album_stmt
                .query_map(params![asset_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            ids
        }
    };

    let playback = build_playback_info(asset_id, summary.kind, &files, video_meta.as_ref());

    Ok(Some(AssetDetail {
        summary,
        files,
        photo_meta,
        video_meta,
        overrides,
        tags,
        histogram,
        album_ids,
        playback,
    }))
}

/// Tuple returned by [`get_asset_with_files`] for `/api/media/{id}/*` handlers.
pub type AssetWithFiles = (
    AssetSummary,
    Vec<AssetFileView>,
    FolderRecord,
    Option<VideoMetaView>,
);

/// Fast projection for media streaming handlers (`/api/media/{id}/*`) scoped by `scope`.
///
/// Excludes soft-deleted (trashed) assets by default.
pub fn get_asset_with_files(
    conn: &Connection,
    scope: &RbacScope,
    asset_id: &str,
) -> DbResult<Option<AssetWithFiles>> {
    get_asset_with_files_filtered(conn, scope, asset_id, false)
}

/// Fast projection for media streaming handlers (`/api/media/{id}/*`) with explicit trash visibility.
pub fn get_asset_with_files_filtered(
    conn: &Connection,
    scope: &RbacScope,
    asset_id: &str,
    include_trashed: bool,
) -> DbResult<Option<AssetWithFiles>> {
    let mut params: Vec<Value> = vec![Value::Text(asset_id.to_string())];
    let rbac_clause = rbac_sql::folder_clause("v", scope, &mut params);
    let trash_clause = if include_trashed {
        ""
    } else {
        "AND v.deleted_at_ms IS NULL"
    };
    let sql = format!(
        "SELECT {ASSET_SUMMARY_COLUMNS}
         FROM v_assets_effective v
         WHERE v.id = ?1
           AND v.is_hidden = 0
           {trash_clause}
           AND {rbac_clause}
         LIMIT 1"
    );

    let Some(summary) = conn
        .query_row(&sql, params_from_iter(params), row_to_asset_summary)
        .optional()?
    else {
        return Ok(None);
    };

    let Some(folder) = folders::get_folder_by_id(conn, &summary.folder_id)? else {
        return Ok(None);
    };
    let files = load_asset_files(conn, asset_id)?;
    let video_meta = load_video_meta(conn, asset_id)?;
    Ok(Some((summary, files, folder, video_meta)))
}

/// Sets `assets.is_favorite` for an asset visible under `scope`.
///
/// Returns `Ok(None)` if the asset does not exist, is hidden, or is outside `scope`.
pub fn set_asset_favorite(
    conn: &Connection,
    scope: &RbacScope,
    asset_id: &str,
    is_favorite: bool,
) -> DbResult<Option<bool>> {
    let mut params: Vec<Value> = vec![
        Value::Integer(bool_to_int(is_favorite)),
        Value::Integer(now_ms()),
        Value::Text(asset_id.to_string()),
    ];
    let rbac_clause = rbac_sql::folder_clause("assets", scope, &mut params);
    let sql = format!(
        "UPDATE assets
         SET is_favorite = ?1, updated_at_ms = ?2
         WHERE id = ?3 AND is_hidden = 0 AND deleted_at_ms IS NULL AND {rbac_clause}"
    );
    let affected = conn.execute(&sql, params_from_iter(params))?;
    if affected == 0 {
        Ok(None)
    } else {
        Ok(Some(is_favorite))
    }
}

/// Non-destructively merges partial user overrides into `user_overrides` and updates `assets.updated_at_ms`.
///
/// Returns `Ok(None)` if the asset does not exist, is hidden, or is outside `scope`.
pub fn upsert_user_overrides(
    conn: &Connection,
    scope: &RbacScope,
    asset_id: &str,
    actor_user_id: &str,
    req: &UpdateOverridesRequest,
) -> DbResult<Option<()>> {
    if let Some(rating) = req.rating {
        if rating > 5 {
            return Err(DbError::InvalidArgument(
                "rating must be between 0 and 5".to_string(),
            ));
        }
    }

    let mut check_params: Vec<Value> = vec![Value::Text(asset_id.to_string())];
    let rbac_clause = rbac_sql::folder_clause("a", scope, &mut check_params);
    let check_sql = format!(
        "SELECT 1 FROM assets a WHERE a.id = ?1 AND a.is_hidden = 0 AND a.deleted_at_ms IS NULL AND {rbac_clause} LIMIT 1"
    );
    let exists: Option<i64> = conn
        .query_row(&check_sql, params_from_iter(check_params), |row| row.get(0))
        .optional()?;
    if exists.is_none() {
        return Ok(None);
    }

    let existing = load_user_overrides(conn, asset_id)?.unwrap_or_default();
    let merged_title = req.title.clone().or(existing.title);
    let merged_description = req.description.clone().or(existing.description);
    let merged_capture_at_ms = req.capture_at_ms.or(existing.capture_at_ms);
    let merged_gps_lat = req.gps_lat.or(existing.gps_lat);
    let merged_gps_lon = req.gps_lon.or(existing.gps_lon);
    let merged_geo_city = req.geo_city.clone().or(existing.geo_city);
    let merged_geo_admin1 = req.geo_admin1.clone().or(existing.geo_admin1);
    let merged_geo_country = req.geo_country.clone().or(existing.geo_country);
    let merged_geo_country_code = req.geo_country_code.clone().or(existing.geo_country_code);
    let merged_rating = req.rating.or(existing.rating).map(i64::from);
    let ts = now_ms();

    with_savepoint(conn, "upsert_user_overrides", || {
        conn.execute(
            "INSERT INTO user_overrides (
                asset_id, title, description, capture_at_ms, gps_lat, gps_lon,
                geo_city, geo_admin1, geo_country, geo_country_code,
                rating, updated_by, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(asset_id) DO UPDATE SET
                title = excluded.title,
                description = excluded.description,
                capture_at_ms = excluded.capture_at_ms,
                gps_lat = excluded.gps_lat,
                gps_lon = excluded.gps_lon,
                geo_city = excluded.geo_city,
                geo_admin1 = excluded.geo_admin1,
                geo_country = excluded.geo_country,
                geo_country_code = excluded.geo_country_code,
                rating = excluded.rating,
                updated_by = excluded.updated_by,
                updated_at_ms = excluded.updated_at_ms",
            params![
                asset_id,
                merged_title,
                merged_description,
                merged_capture_at_ms,
                merged_gps_lat,
                merged_gps_lon,
                merged_geo_city,
                merged_geo_admin1,
                merged_geo_country,
                merged_geo_country_code,
                merged_rating,
                actor_user_id,
                ts,
            ],
        )?;
        conn.execute(
            "UPDATE assets
             SET capture_at_ms = COALESCE(
                     (SELECT o.capture_at_ms FROM user_overrides o WHERE o.asset_id = assets.id),
                     (SELECT p.taken_at_ms FROM deterministic_photo_meta p WHERE p.asset_id = assets.id),
                     (SELECT v.taken_at_ms FROM deterministic_video_meta v WHERE v.asset_id = assets.id),
                     assets.capture_at_ms
                 ),
                 updated_at_ms = ?1
             WHERE id = ?2",
            params![ts, asset_id],
        )?;
        crate::repo::map::sync_asset_geo(conn, asset_id)?;
        Ok(())
    })?;

    Ok(Some(()))
}

/// Inserts or updates the 256-bin RGB + Luma histogram for `asset_id`.
pub fn upsert_asset_histogram(
    conn: &Connection,
    asset_id: &str,
    hist: &Histogram256,
) -> DbResult<()> {
    if hist.r.len() != HISTOGRAM_BINS
        || hist.g.len() != HISTOGRAM_BINS
        || hist.b.len() != HISTOGRAM_BINS
        || hist.luma.len() != HISTOGRAM_BINS
    {
        return Err(DbError::InvalidArgument(format!(
            "histogram channels must each contain {HISTOGRAM_BINS} bins"
        )));
    }

    let r_blob = Histogram256::encode_bins_le(&hist.r);
    let g_blob = Histogram256::encode_bins_le(&hist.g);
    let b_blob = Histogram256::encode_bins_le(&hist.b);
    let luma_blob = Histogram256::encode_bins_le(&hist.luma);
    let ts = now_ms();

    conn.execute(
        "INSERT INTO asset_histograms (
            asset_id, r_bins, g_bins, b_bins, luma_bins, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(asset_id) DO UPDATE SET
            r_bins = excluded.r_bins,
            g_bins = excluded.g_bins,
            b_bins = excluded.b_bins,
            luma_bins = excluded.luma_bins,
            updated_at_ms = excluded.updated_at_ms",
        params![asset_id, r_blob, g_blob, b_blob, luma_blob, ts],
    )?;
    Ok(())
}

fn push_unique_id_placeholders(ids: &[String], params: &mut Vec<Value>) -> Option<String> {
    let mut seen = HashSet::with_capacity(ids.len());
    let mut placeholders = Vec::with_capacity(ids.len());
    for id in ids {
        if seen.insert(id.as_str()) {
            let idx = params.len() + 1;
            params.push(Value::Text(id.clone()));
            placeholders.push(format!("?{idx}"));
        }
    }
    if placeholders.is_empty() {
        None
    } else {
        Some(placeholders.join(", "))
    }
}

const BATCH_IN_CHUNK: usize = 400;

/// Soft-deletes non-deleted, non-hidden assets in `asset_ids` scoped by `scope`.
pub fn soft_delete_assets(
    conn: &Connection,
    scope: &RbacScope,
    actor_user_id: &str,
    asset_ids: &[String],
    now_ms: i64,
) -> DbResult<usize> {
    if asset_ids.is_empty() {
        return Ok(0);
    }

    with_savepoint(conn, "soft_delete_assets", || {
        let mut total_affected = 0usize;
        for chunk in asset_ids.chunks(BATCH_IN_CHUNK) {
            let mut params: Vec<Value> = vec![
                Value::Integer(now_ms),
                Value::Text(actor_user_id.to_string()),
            ];
            let Some(id_list) = push_unique_id_placeholders(chunk, &mut params) else {
                continue;
            };
            let rbac_clause = rbac_sql::folder_clause("assets", scope, &mut params);
            let sql = format!(
                "UPDATE assets
                 SET deleted_at_ms = ?1, deleted_by = ?2, updated_at_ms = ?1
                 WHERE id IN ({id_list})
                   AND deleted_at_ms IS NULL
                   AND is_hidden = 0
                   AND {rbac_clause}"
            );
            let affected = conn.execute(&sql, params_from_iter(params))?;
            if affected > 0 {
                crate::repo::map::sync_assets_geo(conn, chunk)?;
                total_affected += affected;
            }
        }
        Ok(total_affected)
    })
}

/// Restores soft-deleted assets in `asset_ids` back to the library, scoped by `scope`.
pub fn restore_assets(
    conn: &Connection,
    scope: &RbacScope,
    asset_ids: &[String],
    now_ms: i64,
) -> DbResult<usize> {
    if asset_ids.is_empty() {
        return Ok(0);
    }

    with_savepoint(conn, "restore_assets", || {
        let mut total_affected = 0usize;
        for chunk in asset_ids.chunks(BATCH_IN_CHUNK) {
            let mut params: Vec<Value> = vec![Value::Integer(now_ms)];
            let Some(id_list) = push_unique_id_placeholders(chunk, &mut params) else {
                continue;
            };
            let rbac_clause = rbac_sql::folder_clause("assets", scope, &mut params);
            let sql = format!(
                "UPDATE assets
                 SET deleted_at_ms = NULL, deleted_by = NULL, updated_at_ms = ?1
                 WHERE id IN ({id_list})
                   AND deleted_at_ms IS NOT NULL
                   AND {rbac_clause}"
            );
            let affected = conn.execute(&sql, params_from_iter(params))?;
            if affected > 0 {
                crate::repo::map::sync_assets_geo(conn, chunk)?;
                total_affected += affected;
            }
        }
        Ok(total_affected)
    })
}

/// Lists IDs of trashed assets whose `deleted_at_ms` is at or before `cutoff_ms`.
pub fn list_expired_trash(conn: &Connection, cutoff_ms: i64) -> DbResult<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT id
         FROM assets
         WHERE deleted_at_ms IS NOT NULL
           AND deleted_at_ms <= ?1
         ORDER BY deleted_at_ms ASC, id ASC",
    )?;
    let rows = stmt.query_map(params![cutoff_ms], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Resolves trashed assets and their physical file records for permanent purge.
///
/// When `asset_ids` is `None`, selects all trashed assets across the library.
pub fn get_assets_for_purge(
    conn: &Connection,
    asset_ids: Option<&[String]>,
) -> DbResult<Vec<PurgeCandidate>> {
    let mut sortable: Vec<(i64, PurgeCandidate)> = Vec::new();

    match asset_ids {
        Some(ids) => {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let mut seen = HashSet::with_capacity(ids.len());
            let unique_ids: Vec<&String> =
                ids.iter().filter(|id| seen.insert(id.as_str())).collect();
            for chunk in unique_ids.chunks(BATCH_IN_CHUNK) {
                let mut params: Vec<Value> = Vec::with_capacity(chunk.len());
                let mut placeholders = Vec::with_capacity(chunk.len());
                for (i, id) in chunk.iter().enumerate() {
                    params.push(Value::Text((*id).clone()));
                    placeholders.push(format!("?{}", i + 1));
                }
                let sql = format!(
                    "SELECT
                        a.id,
                        a.folder_id,
                        f.root_path,
                        f.volume_uuid,
                        f.allow_disk_delete,
                        f.is_offline,
                        a.thumb_rel_path,
                        COALESCE(a.deleted_at_ms, 0)
                     FROM assets a
                     JOIN folders f ON f.id = a.folder_id
                     WHERE a.deleted_at_ms IS NOT NULL
                       AND a.id IN ({})
                     ORDER BY a.deleted_at_ms ASC, a.id ASC",
                    placeholders.join(", ")
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map(params_from_iter(params), |row| {
                    let deleted_at_ms: i64 = row.get(7)?;
                    Ok((
                        deleted_at_ms,
                        PurgeCandidate {
                            asset_id: row.get(0)?,
                            folder_id: row.get(1)?,
                            folder_root_path: row.get(2)?,
                            folder_volume_uuid: row.get(3)?,
                            allow_disk_delete: row.get::<_, i64>(4)? != 0,
                            is_offline: row.get::<_, i64>(5)? != 0,
                            thumb_rel_path: row.get(6)?,
                            files: Vec::new(),
                        },
                    ))
                })?;
                for r in rows {
                    sortable.push(r?);
                }
            }
            sortable.sort_unstable_by(|a, b| {
                a.0.cmp(&b.0).then_with(|| a.1.asset_id.cmp(&b.1.asset_id))
            });
        }
        None => {
            let sql = "SELECT
                    a.id,
                    a.folder_id,
                    f.root_path,
                    f.volume_uuid,
                    f.allow_disk_delete,
                    f.is_offline,
                    a.thumb_rel_path,
                    COALESCE(a.deleted_at_ms, 0)
                 FROM assets a
                 JOIN folders f ON f.id = a.folder_id
                 WHERE a.deleted_at_ms IS NOT NULL
                 ORDER BY a.deleted_at_ms ASC, a.id ASC";
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map([], |row| {
                let deleted_at_ms: i64 = row.get(7)?;
                Ok((
                    deleted_at_ms,
                    PurgeCandidate {
                        asset_id: row.get(0)?,
                        folder_id: row.get(1)?,
                        folder_root_path: row.get(2)?,
                        folder_volume_uuid: row.get(3)?,
                        allow_disk_delete: row.get::<_, i64>(4)? != 0,
                        is_offline: row.get::<_, i64>(5)? != 0,
                        thumb_rel_path: row.get(6)?,
                        files: Vec::new(),
                    },
                ))
            })?;
            for r in rows {
                sortable.push(r?);
            }
        }
    }

    let mut candidates: Vec<PurgeCandidate> = sortable.into_iter().map(|(_, c)| c).collect();

    if candidates.is_empty() {
        return Ok(candidates);
    }

    let mut by_asset: HashMap<String, Vec<AssetFileView>> = HashMap::new();
    for chunk in candidates.chunks(BATCH_IN_CHUNK) {
        let mut file_params: Vec<Value> = Vec::with_capacity(chunk.len());
        let mut placeholders = Vec::with_capacity(chunk.len());
        for (idx, c) in chunk.iter().enumerate() {
            file_params.push(Value::Text(c.asset_id.clone()));
            placeholders.push(format!("?{}", idx + 1));
        }

        let files_sql = format!(
            "SELECT
                id, asset_id, folder_id, rel_path, file_role, mime_type, extension,
                file_size, mtime_ms, partial_blake3, content_identifier,
                embedded_video_offset, embedded_video_length, is_missing
             FROM asset_files
             WHERE asset_id IN ({})
             ORDER BY rel_path ASC",
            placeholders.join(", ")
        );

        let mut file_stmt = conn.prepare(&files_sql)?;
        let file_rows = file_stmt.query_map(params_from_iter(file_params), row_to_asset_file)?;
        for file in file_rows {
            let f = file?;
            by_asset.entry(f.asset_id.clone()).or_default().push(f);
        }
    }

    for candidate in &mut candidates {
        if let Some(files) = by_asset.remove(&candidate.asset_id) {
            candidate.files = files;
        }
    }

    Ok(candidates)
}

/// Returns `true` if `asset_id` currently remains soft-deleted in Trash.
pub fn is_asset_trashed(conn: &Connection, asset_id: &str) -> DbResult<bool> {
    let trashed: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM assets WHERE id = ?1 AND deleted_at_ms IS NOT NULL LIMIT 1",
            params![asset_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(trashed.is_some())
}

/// Finalizes DB removal or hiding after a permanent purge.
///
/// Only mutates rows that are still soft-deleted (`deleted_at_ms IS NOT NULL`) so a concurrent
/// restore cannot be overwritten. Returns `true` if the row was purged.
pub fn delete_purged_asset_records(
    conn: &Connection,
    asset_id: &str,
    hide_only: bool,
) -> DbResult<bool> {
    if hide_only {
        let affected = with_savepoint(conn, "delete_purged_asset_records", || {
            let affected = conn.execute(
                "UPDATE assets
                 SET is_hidden = 1, deleted_at_ms = NULL, deleted_by = NULL, updated_at_ms = ?1
                 WHERE id = ?2 AND deleted_at_ms IS NOT NULL",
                params![now_ms(), asset_id],
            )?;
            if affected > 0 {
                crate::repo::map::sync_asset_geo(conn, asset_id)?;
            }
            Ok(affected)
        })?;
        Ok(affected > 0)
    } else {
        let affected = conn.execute(
            "DELETE FROM assets WHERE id = ?1 AND deleted_at_ms IS NOT NULL",
            params![asset_id],
        )?;
        Ok(affected > 0)
    }
}
