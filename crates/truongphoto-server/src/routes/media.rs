use crate::error::AppError;
use crate::extractors::RbacContext;
use crate::ingest::cache_mgr::safe_join_relative;
use crate::state::AppState;
use axum::{
    body::Body,
    extract::{Path as AxumPath, Query, State},
    http::{
        header::{
            ACCEPT_RANGES, CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE,
            CONTENT_TYPE, ETAG, IF_NONE_MATCH, RANGE,
        },
        HeaderMap, HeaderName, HeaderValue, StatusCode,
    },
    response::Response,
};
use chrono::Utc;
use serde::Deserialize;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use truongphoto_core::{
    AssetFileView, AssetKind, AssetSummary, CacheEntryKind, FileRole, FolderRecord, LruCacheEntry,
};
use truongphoto_db::repo;
use truongphoto_media::{
    classify::is_raw_extension,
    decode::{generate_preview_webp, generate_thumb_and_histogram},
    metadata::{extract_livp_entry_bytes, inspect_livp},
    raw::demosaic_raw_to_jpeg,
    video::transcode_to_hls,
};

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct OriginalQuery {
    pub file_role: Option<String>,
    pub mode: Option<String>,
    pub download: Option<String>,
}

fn asset_prefix(asset_id: &str) -> &str {
    let len = asset_id.len().min(2);
    &asset_id[..len]
}

fn asset_prefix_two(asset_id: &str) -> (&str, &str) {
    if asset_id.len() >= 4 {
        (&asset_id[0..2], &asset_id[2..4])
    } else if asset_id.len() >= 2 {
        (&asset_id[0..2], "00")
    } else {
        ("00", "00")
    }
}

fn resolve_asset_file_path(
    folder: &FolderRecord,
    file: &AssetFileView,
) -> Result<PathBuf, AppError> {
    if folder.is_offline || file.is_missing {
        return Err(AppError::not_found(
            "Media file is currently offline or missing.",
        ));
    }
    let root = Path::new(&folder.root_path);
    let full = safe_join_relative(root, &file.rel_path)
        .ok_or_else(|| AppError::not_found("Invalid media file path."))?;
    if !full.is_file() {
        return Err(AppError::not_found("Media file does not exist on disk."));
    }
    Ok(full)
}

fn select_preview_source_file<'a>(
    summary: &AssetSummary,
    files: &'a [AssetFileView],
) -> Option<&'a AssetFileView> {
    if summary.kind == AssetKind::RawJpgStack {
        files
            .iter()
            .find(|f| f.file_role == FileRole::JpegCompanion && !f.is_missing)
            .or_else(|| {
                files
                    .iter()
                    .find(|f| f.file_role == FileRole::Raw && !f.is_missing)
            })
            .or_else(|| files.iter().find(|f| !f.is_missing))
    } else {
        files
            .iter()
            .find(|f| f.file_role == FileRole::Primary && !f.is_missing)
            .or_else(|| files.iter().find(|f| !f.is_missing))
    }
}

fn sanitize_download_filename(rel_path: &str) -> String {
    let raw = Path::new(rel_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("download.bin");
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "download.bin".to_string()
    } else {
        cleaned
    }
}

fn atomic_write_bytes_sync(dest: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let parent = dest
        .parent()
        .ok_or_else(|| std::io::Error::other("missing parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    std::io::Write::write_all(&mut tmp, bytes)?;
    std::io::Write::flush(&mut tmp)?;
    tmp.persist(dest).map_err(|e| e.error)?;
    Ok(())
}

fn check_etag_match(req_headers: &HeaderMap, etag: &str) -> bool {
    let Some(val) = req_headers.get(IF_NONE_MATCH).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    val.split(',').any(|tag| {
        let t = tag.trim();
        t == etag || t == "*"
    })
}

pub(crate) struct ServeSliceOptions<'a> {
    pub path: &'a Path,
    pub slice_offset: u64,
    pub slice_len: u64,
    pub content_type: &'a str,
    pub etag: &'a str,
    pub cache_control: Option<&'a str>,
    pub download_filename: Option<&'a str>,
    pub extra_header: Option<(&'a str, &'a str)>,
}

pub(crate) async fn serve_file_slice_with_range(
    opts: ServeSliceOptions<'_>,
    req_headers: &HeaderMap,
) -> Result<Response, AppError> {
    let ServeSliceOptions {
        path,
        slice_offset,
        slice_len,
        content_type,
        etag,
        cache_control,
        download_filename,
        extra_header,
    } = opts;
    if check_etag_match(req_headers, etag) && !req_headers.contains_key(RANGE) {
        let mut builder = Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(ETAG, etag);
        if let Some(cc) = cache_control {
            builder = builder.header(CACHE_CONTROL, cc);
        }
        return builder
            .body(Body::empty())
            .map_err(|e| AppError::internal(format!("Failed to build 304 response: {e}")));
    }

    let range_header = req_headers.get(RANGE).and_then(|v| v.to_str().ok());

    let (status, start, end, content_range_val) = if let Some(raw_range) = range_header {
        if slice_len == 0 {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(CONTENT_RANGE, "bytes */0")
                .body(Body::empty())
                .map_err(|e| AppError::internal(format!("Failed to build 416 response: {e}")));
        }

        let validated = http_range_header::parse_range_header(raw_range)
            .and_then(|parsed| parsed.validate(slice_len));

        let ranges = match validated {
            Ok(r) if !r.is_empty() => r,
            _ => {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(CONTENT_RANGE, format!("bytes */{slice_len}"))
                    .body(Body::empty())
                    .map_err(|e| AppError::internal(format!("Failed to build 416 response: {e}")));
            }
        };

        let first = &ranges[0];
        let s = *first.start();
        let e = *first.end();
        (
            StatusCode::PARTIAL_CONTENT,
            s,
            e,
            Some(format!("bytes {s}-{e}/{slice_len}")),
        )
    } else if slice_len == 0 {
        let mut resp = Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, content_type)
            .header(ACCEPT_RANGES, "bytes")
            .header(CONTENT_LENGTH, "0")
            .header(ETAG, etag)
            .body(Body::empty())
            .map_err(|e| AppError::internal(format!("Failed to build empty response: {e}")))?;
        if let Some(cc) = cache_control {
            if let Ok(v) = HeaderValue::from_str(cc) {
                resp.headers_mut().insert(CACHE_CONTROL, v);
            }
        }
        return Ok(resp);
    } else {
        (StatusCode::OK, 0, slice_len - 1, None)
    };

    let chunk_len = end - start + 1;
    let mut file = tokio::fs::File::open(path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AppError::not_found("Media file not found.")
        } else {
            AppError::internal(format!("Failed to open media file: {e}"))
        }
    })?;

    file.seek(SeekFrom::Start(slice_offset.saturating_add(start)))
        .await
        .map_err(|e| AppError::internal(format!("Failed to seek media file: {e}")))?;

    let limited = file.take(chunk_len);
    let stream = ReaderStream::new(limited);

    let mut builder = Response::builder()
        .status(status)
        .header(CONTENT_TYPE, content_type)
        .header(ACCEPT_RANGES, "bytes")
        .header(CONTENT_LENGTH, chunk_len.to_string())
        .header(ETAG, etag);

    if let Some(cr) = content_range_val {
        builder = builder.header(CONTENT_RANGE, cr);
    }
    if let Some(cc) = cache_control {
        builder = builder.header(CACHE_CONTROL, cc);
    }
    if let Some(fname) = download_filename {
        builder = builder.header(
            CONTENT_DISPOSITION,
            format!("attachment; filename=\"{fname}\""),
        );
    }
    if let Some((hk, hv)) = extra_header {
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(hk.as_bytes()),
            HeaderValue::from_str(hv),
        ) {
            builder = builder.header(name, val);
        }
    }

    builder
        .body(Body::from_stream(stream))
        .map_err(|e| AppError::internal(format!("Failed to build media response: {e}")))
}

/// `GET /api/media/{id}/thumb` (Authenticated + RbacContext).
pub async fn get_thumb(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath(asset_id): AxumPath<String>,
    req_headers: HeaderMap,
) -> Result<Response, AppError> {
    let scope = rbac.scope.clone();
    let aid_lookup = asset_id.clone();
    let default_paths = state.default_paths.clone();

    let (asset_opt, settings) = state
        .db
        .read(move |conn| {
            let asset = repo::assets::get_asset_with_files(conn, &scope, &aid_lookup)?;
            let settings = repo::settings::load_app_settings(conn, &default_paths)?;
            Ok((asset, settings))
        })
        .await?;

    let (summary, files, folder, _) =
        asset_opt.ok_or_else(|| AppError::not_found("Asset not found."))?;
    let thumbs_dir = PathBuf::from(&settings.thumbnails_dir);

    let existing_thumb = summary
        .thumb_rel_path
        .as_deref()
        .and_then(|rel| safe_join_relative(&thumbs_dir, rel))
        .filter(|p| p.is_file());

    let thumb_path = if let Some(path) = existing_thumb {
        path
    } else {
        let lock_key = format!("thumb:{asset_id}");
        let _guard = state.cache_mgr.singleflight_lock(&lock_key).await;

        let scope2 = rbac.scope;
        let aid2 = asset_id.clone();
        let refreshed = state
            .db
            .read(move |conn| repo::assets::get_asset_with_files(conn, &scope2, &aid2))
            .await?
            .ok_or_else(|| AppError::not_found("Asset not found."))?;

        if let Some(rel) = refreshed.0.thumb_rel_path.as_deref() {
            if let Some(p) = safe_join_relative(&thumbs_dir, rel).filter(|p| p.is_file()) {
                let meta = tokio::fs::metadata(&p)
                    .await
                    .map_err(|e| AppError::internal(format!("Failed to stat thumb: {e}")))?;
                let etag = format!("\"{asset_id}-thumb-{}\"", meta.len());
                return serve_file_slice_with_range(
                    ServeSliceOptions {
                        path: &p,
                        slice_offset: 0,
                        slice_len: meta.len(),
                        content_type: "image/webp",
                        etag: &etag,
                        cache_control: Some("private, max-age=86400"),
                        download_filename: None,
                        extra_header: None,
                    },
                    &req_headers,
                )
                .await;
            }
        }

        let src_file = select_preview_source_file(&summary, &files)
            .ok_or_else(|| AppError::not_found("No physical media file available."))?;
        let src_path = resolve_asset_file_path(&folder, src_file)?;

        let (p0, p1) = asset_prefix_two(&asset_id);
        let rel_path = format!("{p0}/{p1}/{asset_id}.webp");
        let dest_path = thumbs_dir
            .join(p0)
            .join(p1)
            .join(format!("{asset_id}.webp"));
        let dest_clone = dest_path.clone();

        let generated = tokio::task::spawn_blocking(move || {
            generate_thumb_and_histogram(&src_path, &dest_clone)
        })
        .await
        .map_err(|e| AppError::internal(format!("Thumbnail worker panicked: {e}")))?
        .map_err(|e| AppError::internal(format!("Failed to generate thumbnail: {e}")))?;

        let aid_upd = asset_id.clone();
        let rel_upd = rel_path.clone();
        let hist = generated.histogram;
        let now_ms = Utc::now().timestamp_millis();
        state
            .db
            .write(move |conn| {
                conn.execute(
                    "UPDATE assets
                     SET thumb_rel_path = ?1, thumb_ready = 1, updated_at_ms = ?2
                     WHERE id = ?3",
                    truongphoto_db::rusqlite::params![rel_upd, now_ms, aid_upd],
                )?;
                repo::assets::upsert_asset_histogram(conn, &aid_upd, &hist)?;
                Ok(())
            })
            .await?;

        dest_path
    };

    let meta = tokio::fs::metadata(&thumb_path)
        .await
        .map_err(|e| AppError::internal(format!("Failed to stat thumb: {e}")))?;
    let etag = format!("\"{asset_id}-thumb-{}\"", meta.len());
    serve_file_slice_with_range(
        ServeSliceOptions {
            path: &thumb_path,
            slice_offset: 0,
            slice_len: meta.len(),
            content_type: "image/webp",
            etag: &etag,
            cache_control: Some("private, max-age=86400"),
            download_filename: None,
            extra_header: None,
        },
        &req_headers,
    )
    .await
}

/// `GET /api/media/{id}/preview` (Authenticated + RbacContext; 2048px singleflight LRU cached WebP).
pub async fn get_preview(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath(asset_id): AxumPath<String>,
    req_headers: HeaderMap,
) -> Result<Response, AppError> {
    let scope = rbac.scope.clone();
    let aid_lookup = asset_id.clone();
    let default_paths = state.default_paths.clone();

    let (asset_opt, settings) = state
        .db
        .read(move |conn| {
            let asset = repo::assets::get_asset_with_files(conn, &scope, &aid_lookup)?;
            let settings = repo::settings::load_app_settings(conn, &default_paths)?;
            Ok((asset, settings))
        })
        .await?;

    let (summary, files, folder, _) =
        asset_opt.ok_or_else(|| AppError::not_found("Asset not found."))?;
    let cache_dir = PathBuf::from(&settings.cache_dir);
    let p0 = asset_prefix(&asset_id);
    let rel_path = format!("previews/{p0}/{asset_id}.webp");
    let dest_path = cache_dir.join(&rel_path);
    let cache_key = format!("preview_2048:{asset_id}");

    if dest_path.is_file() {
        state.cache_mgr.touch_throttled(&cache_key).await;
    } else {
        let _guard = state.cache_mgr.singleflight_lock(&cache_key).await;
        if dest_path.is_file() {
            state.cache_mgr.touch_throttled(&cache_key).await;
        } else {
            let src_file = select_preview_source_file(&summary, &files)
                .ok_or_else(|| AppError::not_found("No physical media file available."))?;
            let src_path = resolve_asset_file_path(&folder, src_file)?;
            let dest_clone = dest_path.clone();

            let size_bytes = tokio::task::spawn_blocking(move || {
                generate_preview_webp(&src_path, &dest_clone, 2048)
            })
            .await
            .map_err(|e| AppError::internal(format!("Preview worker panicked: {e}")))?
            .map_err(|e| AppError::internal(format!("Failed to generate preview: {e}")))?;

            let now_ms = Utc::now().timestamp_millis();
            state
                .cache_mgr
                .record_and_evict(
                    LruCacheEntry {
                        cache_key,
                        asset_id: Some(asset_id.clone()),
                        kind: CacheEntryKind::Preview2048,
                        rel_path,
                        size_bytes,
                        last_accessed_ms: now_ms,
                        created_at_ms: now_ms,
                    },
                    &cache_dir,
                    settings.cache_limit_gb,
                )
                .await
                .map_err(|e| AppError::internal(format!("Failed to record cache entry: {e}")))?;
        }
    }

    let meta = tokio::fs::metadata(&dest_path)
        .await
        .map_err(|e| AppError::internal(format!("Failed to stat preview: {e}")))?;
    let etag = format!("\"{asset_id}-preview-{}\"", meta.len());
    serve_file_slice_with_range(
        ServeSliceOptions {
            path: &dest_path,
            slice_offset: 0,
            slice_len: meta.len(),
            content_type: "image/webp",
            etag: &etag,
            cache_control: Some("private, max-age=86400"),
            download_filename: None,
            extra_header: None,
        },
        &req_headers,
    )
    .await
}

/// `GET /api/media/{id}/original` (Authenticated + RbacContext; Range + ?file_role + ?mode=raw_demosaic + ?download=1).
pub async fn get_original(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath(asset_id): AxumPath<String>,
    Query(query): Query<OriginalQuery>,
    req_headers: HeaderMap,
) -> Result<Response, AppError> {
    let scope = rbac.scope.clone();
    let aid_lookup = asset_id.clone();
    let default_paths = state.default_paths.clone();

    let (asset_opt, settings) = state
        .db
        .read(move |conn| {
            let asset = repo::assets::get_asset_with_files(conn, &scope, &aid_lookup)?;
            let settings = repo::settings::load_app_settings(conn, &default_paths)?;
            Ok((asset, settings))
        })
        .await?;

    let (_summary, files, folder, _) =
        asset_opt.ok_or_else(|| AppError::not_found("Asset not found."))?;

    let want_download = matches!(
        query.download.as_deref(),
        Some("1") | Some("true") | Some("TRUE")
    );

    if query.mode.as_deref() == Some("raw_demosaic") {
        let raw_file = files
            .iter()
            .find(|f| f.file_role == FileRole::Raw && !f.is_missing)
            .or_else(|| {
                files
                    .iter()
                    .find(|f| is_raw_extension(&f.extension) && !f.is_missing)
            })
            .or_else(|| {
                files
                    .iter()
                    .find(|f| f.file_role == FileRole::Primary && !f.is_missing)
            })
            .ok_or_else(|| AppError::not_found("No RAW file available for asset."))?;

        let src_path = resolve_asset_file_path(&folder, raw_file)?;
        let cache_dir = PathBuf::from(&settings.cache_dir);
        let p0 = asset_prefix(&asset_id);
        let rel_dir = format!("raw/{p0}/{asset_id}");
        let entry_dir = cache_dir.join(&rel_dir);
        let jpeg_path = entry_dir.join("demosaic.jpg");
        let decoder_path = entry_dir.join("decoder.txt");
        let cache_key = format!("raw_full:{asset_id}");

        let decoder_name = if jpeg_path.is_file() {
            state.cache_mgr.touch_throttled(&cache_key).await;
            tokio::fs::read_to_string(&decoder_path)
                .await
                .unwrap_or_else(|_| "embedded_preview".to_string())
        } else {
            let _guard = state.cache_mgr.singleflight_lock(&cache_key).await;
            if jpeg_path.is_file() {
                state.cache_mgr.touch_throttled(&cache_key).await;
                tokio::fs::read_to_string(&decoder_path)
                    .await
                    .unwrap_or_else(|_| "embedded_preview".to_string())
            } else {
                let jpeg_clone = jpeg_path.clone();
                let dec_clone = decoder_path.clone();
                let (bytes_len, decoder_str) = tokio::task::spawn_blocking(move || {
                    let (jpeg_bytes, backend) = demosaic_raw_to_jpeg(&src_path)?;
                    atomic_write_bytes_sync(&jpeg_clone, &jpeg_bytes)?;
                    atomic_write_bytes_sync(&dec_clone, backend.as_bytes())?;
                    Ok::<_, truongphoto_media::MediaError>((
                        (jpeg_bytes.len() + backend.len()) as u64,
                        backend.to_string(),
                    ))
                })
                .await
                .map_err(|e| AppError::internal(format!("RAW demosaic worker panicked: {e}")))?
                .map_err(|e| AppError::internal(format!("RAW demosaic failed: {e}")))?;

                let now_ms = Utc::now().timestamp_millis();
                state
                    .cache_mgr
                    .record_and_evict(
                        LruCacheEntry {
                            cache_key,
                            asset_id: Some(asset_id.clone()),
                            kind: CacheEntryKind::RawFull,
                            rel_path: rel_dir,
                            size_bytes: bytes_len,
                            last_accessed_ms: now_ms,
                            created_at_ms: now_ms,
                        },
                        &cache_dir,
                        settings.cache_limit_gb,
                    )
                    .await
                    .map_err(|e| AppError::internal(format!("Failed to record RAW cache: {e}")))?;

                decoder_str
            }
        };

        let meta = tokio::fs::metadata(&jpeg_path)
            .await
            .map_err(|e| AppError::internal(format!("Failed to stat demosaiced JPEG: {e}")))?;
        let etag = format!("\"{asset_id}-raw-{}\"", meta.len());
        let dl_name = if want_download {
            Some(format!(
                "{}.jpg",
                Path::new(&raw_file.rel_path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("raw")
            ))
        } else {
            None
        };

        return serve_file_slice_with_range(
            ServeSliceOptions {
                path: &jpeg_path,
                slice_offset: 0,
                slice_len: meta.len(),
                content_type: "image/jpeg",
                etag: &etag,
                cache_control: Some("private, max-age=86400"),
                download_filename: dl_name.as_deref(),
                extra_header: Some(("X-TruongPhoto-Decoder", decoder_name.trim())),
            },
            &req_headers,
        )
        .await;
    }

    let target_file = if let Some(role_str) = query.file_role.as_deref() {
        let parsed_role = role_str
            .parse::<FileRole>()
            .map_err(|_| AppError::validation("Invalid file_role parameter."))?;
        files
            .iter()
            .find(|f| f.file_role == parsed_role && !f.is_missing)
            .ok_or_else(|| AppError::not_found("Requested file role not found for asset."))?
    } else {
        files
            .iter()
            .find(|f| f.file_role == FileRole::Primary && !f.is_missing)
            .or_else(|| files.iter().find(|f| !f.is_missing))
            .ok_or_else(|| AppError::not_found("No physical media file available."))?
    };

    let src_path = resolve_asset_file_path(&folder, target_file)?;
    let meta = tokio::fs::metadata(&src_path)
        .await
        .map_err(|e| AppError::internal(format!("Failed to stat original file: {e}")))?;
    let etag = format!(
        "\"{}-{}-{}\"",
        target_file.id,
        meta.len(),
        target_file.mtime_ms
    );
    let dl_name = if want_download {
        Some(sanitize_download_filename(&target_file.rel_path))
    } else {
        None
    };

    serve_file_slice_with_range(
        ServeSliceOptions {
            path: &src_path,
            slice_offset: 0,
            slice_len: meta.len(),
            content_type: &target_file.mime_type,
            etag: &etag,
            cache_control: Some("private, max-age=3600"),
            download_filename: dl_name.as_deref(),
            extra_header: None,
        },
        &req_headers,
    )
    .await
}

/// `GET /api/media/{id}/live-video` (Authenticated + RbacContext; Range streaming for Live Photo, `.livp`, and Motion Photo).
pub async fn get_live_video(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath(asset_id): AxumPath<String>,
    req_headers: HeaderMap,
) -> Result<Response, AppError> {
    let scope = rbac.scope.clone();
    let aid_lookup = asset_id.clone();
    let default_paths = state.default_paths.clone();

    let (asset_opt, settings) = state
        .db
        .read(move |conn| {
            let asset = repo::assets::get_asset_with_files(conn, &scope, &aid_lookup)?;
            let settings = repo::settings::load_app_settings(conn, &default_paths)?;
            Ok((asset, settings))
        })
        .await?;

    let (_summary, files, folder, _) =
        asset_opt.ok_or_else(|| AppError::not_found("Asset not found."))?;

    // 1. Apple Live Photo companion video file
    if let Some(live_file) = files
        .iter()
        .find(|f| f.file_role == FileRole::LiveVideo && !f.is_missing)
    {
        let src_path = resolve_asset_file_path(&folder, live_file)?;
        let meta = tokio::fs::metadata(&src_path)
            .await
            .map_err(|e| AppError::internal(format!("Failed to stat live video: {e}")))?;
        let etag = format!("\"{}-live-{}\"", live_file.id, meta.len());
        return serve_file_slice_with_range(
            ServeSliceOptions {
                path: &src_path,
                slice_offset: 0,
                slice_len: meta.len(),
                content_type: &live_file.mime_type,
                etag: &etag,
                cache_control: Some("private, max-age=86400"),
                download_filename: None,
                extra_header: None,
            },
            &req_headers,
        )
        .await;
    }

    let primary = files
        .iter()
        .find(|f| f.file_role == FileRole::Primary && !f.is_missing)
        .or_else(|| files.iter().find(|f| !f.is_missing))
        .ok_or_else(|| AppError::not_found("No physical media file available."))?;

    // 2. Apple `.livp` ZIP archive unpacked on demand into `cache_dir/live/`
    if primary.extension.eq_ignore_ascii_case("livp") {
        let src_path = resolve_asset_file_path(&folder, primary)?;
        let cache_dir = PathBuf::from(&settings.cache_dir);
        let p0 = asset_prefix(&asset_id);
        let rel_dir = format!("live/{p0}/{asset_id}");
        let entry_dir = cache_dir.join(&rel_dir);
        let video_cache_path = entry_dir.join("live.mov");
        let mime_cache_path = entry_dir.join("mime.txt");
        let cache_key = format!("live_proxy:{asset_id}");

        let mime_type = if video_cache_path.is_file() {
            state.cache_mgr.touch_throttled(&cache_key).await;
            tokio::fs::read_to_string(&mime_cache_path)
                .await
                .unwrap_or_else(|_| "video/quicktime".to_string())
        } else {
            let _guard = state.cache_mgr.singleflight_lock(&cache_key).await;
            if video_cache_path.is_file() {
                state.cache_mgr.touch_throttled(&cache_key).await;
                tokio::fs::read_to_string(&mime_cache_path)
                    .await
                    .unwrap_or_else(|_| "video/quicktime".to_string())
            } else {
                let vid_out = video_cache_path.clone();
                let mime_out = mime_cache_path.clone();
                let (bytes_len, mime) = tokio::task::spawn_blocking(move || {
                    let entries = inspect_livp(&src_path)?;
                    let v_entry = entries.video_entry.ok_or_else(|| {
                        truongphoto_media::MediaError::Archive(
                            "livp archive has no inner video".to_string(),
                        )
                    })?;
                    let v_bytes = extract_livp_entry_bytes(&src_path, &v_entry)?;
                    let mime = if v_entry.to_ascii_lowercase().ends_with(".mp4") {
                        "video/mp4"
                    } else {
                        "video/quicktime"
                    };
                    atomic_write_bytes_sync(&vid_out, &v_bytes)?;
                    atomic_write_bytes_sync(&mime_out, mime.as_bytes())?;
                    Ok::<_, truongphoto_media::MediaError>((
                        (v_bytes.len() + mime.len()) as u64,
                        mime.to_string(),
                    ))
                })
                .await
                .map_err(|e| AppError::internal(format!("livp unpack panicked: {e}")))?
                .map_err(|e| AppError::not_found(format!("Live video not available: {e}")))?;

                let now_ms = Utc::now().timestamp_millis();
                state
                    .cache_mgr
                    .record_and_evict(
                        LruCacheEntry {
                            cache_key,
                            asset_id: Some(asset_id.clone()),
                            kind: CacheEntryKind::LiveProxy,
                            rel_path: rel_dir,
                            size_bytes: bytes_len,
                            last_accessed_ms: now_ms,
                            created_at_ms: now_ms,
                        },
                        &cache_dir,
                        settings.cache_limit_gb,
                    )
                    .await
                    .map_err(|e| AppError::internal(format!("Failed to record live cache: {e}")))?;

                mime
            }
        };

        let meta = tokio::fs::metadata(&video_cache_path)
            .await
            .map_err(|e| AppError::internal(format!("Failed to stat live cache: {e}")))?;
        let etag = format!("\"{asset_id}-livp-video-{}\"", meta.len());
        return serve_file_slice_with_range(
            ServeSliceOptions {
                path: &video_cache_path,
                slice_offset: 0,
                slice_len: meta.len(),
                content_type: mime_type.trim(),
                etag: &etag,
                cache_control: Some("private, max-age=86400"),
                download_filename: None,
                extra_header: None,
            },
            &req_headers,
        )
        .await;
    }

    // 3. Android Motion Photo embedded MP4 trailer byte slice
    if let (Some(offset), Some(length)) =
        (primary.embedded_video_offset, primary.embedded_video_length)
    {
        if length > 0 {
            let src_path = resolve_asset_file_path(&folder, primary)?;
            let meta = tokio::fs::metadata(&src_path)
                .await
                .map_err(|e| AppError::internal(format!("Failed to stat motion photo: {e}")))?;
            if offset.saturating_add(length) > meta.len() {
                return Err(AppError::not_found(
                    "Embedded motion video range exceeds file length.",
                ));
            }
            let etag = format!("\"{}-motion-{offset}-{length}\"", primary.id);
            return serve_file_slice_with_range(
                ServeSliceOptions {
                    path: &src_path,
                    slice_offset: offset,
                    slice_len: length,
                    content_type: "video/mp4",
                    etag: &etag,
                    cache_control: Some("private, max-age=86400"),
                    download_filename: None,
                    extra_header: None,
                },
                &req_headers,
            )
            .await;
        }
    }

    Err(AppError::not_found(
        "Asset does not contain a live or motion video stream.",
    ))
}

/// `GET /api/media/{id}/hls/index.m3u8` (Authenticated + RbacContext; singleflight FFmpeg HLS transcode).
pub async fn get_hls_playlist(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath(asset_id): AxumPath<String>,
) -> Result<Response, AppError> {
    let scope = rbac.scope.clone();
    let aid_lookup = asset_id.clone();
    let default_paths = state.default_paths.clone();

    let (asset_opt, settings) = state
        .db
        .read(move |conn| {
            let asset = repo::assets::get_asset_with_files(conn, &scope, &aid_lookup)?;
            let settings = repo::settings::load_app_settings(conn, &default_paths)?;
            Ok((asset, settings))
        })
        .await?;

    let (_summary, files, folder, video_meta) =
        asset_opt.ok_or_else(|| AppError::not_found("Asset not found."))?;
    let video_file = files
        .iter()
        .find(|f| f.file_role == FileRole::LiveVideo && !f.is_missing)
        .or_else(|| {
            files
                .iter()
                .find(|f| f.file_role == FileRole::Primary && !f.is_missing)
        })
        .ok_or_else(|| AppError::not_found("No physical video file available."))?;

    let src_path = resolve_asset_file_path(&folder, video_file)?;
    let cache_dir = PathBuf::from(&settings.cache_dir);
    let p0 = asset_prefix(&asset_id);
    let rel_dir = format!("hls/{p0}/{asset_id}");
    let hls_dir = cache_dir.join(&rel_dir);
    let playlist_path = hls_dir.join("index.m3u8");
    let cache_key = format!("hls_proxy:{asset_id}");

    if playlist_path.is_file() {
        state.cache_mgr.touch_throttled(&cache_key).await;
    } else {
        let _guard = state.cache_mgr.singleflight_lock(&cache_key).await;
        if playlist_path.is_file() {
            state.cache_mgr.touch_throttled(&cache_key).await;
        } else {
            let is_hdr = video_meta.as_ref().is_some_and(|vm| vm.is_hdr);
            let hls_out = hls_dir.clone();
            let total_bytes =
                tokio::task::spawn_blocking(move || transcode_to_hls(&src_path, &hls_out, is_hdr))
                    .await
                    .map_err(|e| AppError::internal(format!("HLS transcode worker panicked: {e}")))?
                    .map_err(|e| AppError::internal(format!("HLS transcode failed: {e}")))?;

            let now_ms = Utc::now().timestamp_millis();
            state
                .cache_mgr
                .record_and_evict(
                    LruCacheEntry {
                        cache_key,
                        asset_id: Some(asset_id.clone()),
                        kind: CacheEntryKind::HlsProxy,
                        rel_path: rel_dir,
                        size_bytes: total_bytes,
                        last_accessed_ms: now_ms,
                        created_at_ms: now_ms,
                    },
                    &cache_dir,
                    settings.cache_limit_gb,
                )
                .await
                .map_err(|e| AppError::internal(format!("Failed to record HLS cache: {e}")))?;
        }
    }

    let bytes = tokio::fs::read(&playlist_path)
        .await
        .map_err(|e| AppError::internal(format!("Failed to read HLS playlist: {e}")))?;

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/vnd.apple.mpegurl")
        .header(CACHE_CONTROL, "private, max-age=3600")
        .body(Body::from(bytes))
        .map_err(|e| AppError::internal(format!("Failed to build HLS playlist response: {e}")))
}

/// Strictly validates HLS segment filenames (`seg_DDDD.ts` or `seg_DDDD.m4s`, 4 ASCII digits ONLY).
fn is_valid_hls_segment(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("seg_") else {
        return false;
    };
    let Some((digits, ext)) = rest.split_once('.') else {
        return false;
    };
    digits.len() == 4 && digits.bytes().all(|b| b.is_ascii_digit()) && matches!(ext, "ts" | "m4s")
}

/// `GET /api/media/{id}/hls/{segment}` (Authenticated + RbacContext; strict segment filename validation).
pub async fn get_hls_segment(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath((asset_id, segment)): AxumPath<(String, String)>,
    req_headers: HeaderMap,
) -> Result<Response, AppError> {
    if !is_valid_hls_segment(&segment) {
        return Err(AppError::not_found("Invalid HLS segment name."));
    }

    let scope = rbac.scope.clone();
    let aid_lookup = asset_id.clone();
    let default_paths = state.default_paths.clone();

    let (asset_opt, settings) = state
        .db
        .read(move |conn| {
            let asset = repo::assets::get_asset_with_files(conn, &scope, &aid_lookup)?;
            let settings = repo::settings::load_app_settings(conn, &default_paths)?;
            Ok((asset, settings))
        })
        .await?;

    if asset_opt.is_none() {
        return Err(AppError::not_found("Asset not found."));
    }

    let cache_dir = PathBuf::from(&settings.cache_dir);
    let p0 = asset_prefix(&asset_id);
    let segment_path = cache_dir
        .join("hls")
        .join(p0)
        .join(&asset_id)
        .join(&segment);
    if !segment_path.is_file() {
        return Err(AppError::not_found("HLS segment not found."));
    }

    let cache_key = format!("hls_proxy:{asset_id}");
    state.cache_mgr.touch_throttled(&cache_key).await;

    let mime = if segment.ends_with(".m4s") {
        "video/iso.segment"
    } else {
        "video/mp2t"
    };
    let meta = tokio::fs::metadata(&segment_path)
        .await
        .map_err(|e| AppError::internal(format!("Failed to stat HLS segment: {e}")))?;
    let etag = format!("\"{asset_id}-{segment}-{}\"", meta.len());

    serve_file_slice_with_range(
        ServeSliceOptions {
            path: &segment_path,
            slice_offset: 0,
            slice_len: meta.len(),
            content_type: mime,
            etag: &etag,
            cache_control: Some("private, max-age=86400"),
            download_filename: None,
            extra_header: None,
        },
        &req_headers,
    )
    .await
}
