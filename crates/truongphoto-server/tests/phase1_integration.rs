use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{header, Method, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tower::ServiceExt;
use truongphoto_core::{
    CacheEntryKind, LruCacheEntry, PowerState, ServerConfig, StoragePaths, WsServerEvent,
};
use truongphoto_db::{rusqlite::params, DbHandle};
use truongphoto_media::video::video_capabilities;
use truongphoto_server::{
    auth, build_router, ingest::trash_gc::run_trash_retention_sweep, mdns::MdnsHandle,
    state::AppState,
};
use uuid::Uuid;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("tp-p1-server-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct TestHarness {
    _tmp: TempDir,
    db: DbHandle,
    state: Arc<AppState>,
    router: Router,
    thumbs_dir: PathBuf,
    cache_dir: PathBuf,
    uploads_dir: PathBuf,
}

impl TestHarness {
    fn new() -> Self {
        let tmp = TempDir::new().expect("create temp dir");
        let mut config = ServerConfig::from_data_dir(tmp.path().to_path_buf());
        config.mdns_enabled = false;

        let default_paths = StoragePaths::from_data_dir(config.data_dir.clone());
        default_paths.ensure_dirs().expect("ensure dirs");

        let db = DbHandle::open(&config.db_path).expect("open test db");
        let gate_key = auth::generate_process_key().expect("gate key");
        let (ws_tx, _) = tokio::sync::broadcast::channel::<WsServerEvent>(128);
        let (_power_tx, power_rx) = watch::channel(PowerState::default());
        let mdns = MdnsHandle::start(config.mdns_hostname.clone(), 38420, false);
        let hardware = truongphoto_ai::hardware::detect_hardware_info();
        let shutdown_token = tokio_util::sync::CancellationToken::new();

        let state = Arc::new(AppState::new(
            db.clone(),
            config,
            gate_key,
            ws_tx,
            power_rx,
            mdns,
            hardware,
            38420,
            shutdown_token,
        ));
        let router = build_router(state.clone());

        Self {
            thumbs_dir: tmp.path().join("thumbnails"),
            cache_dir: tmp.path().join("cache"),
            uploads_dir: tmp.path().join("uploads"),
            _tmp: tmp,
            db,
            state,
            router,
        }
    }

    fn loopback() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 51234)
    }

    async fn send(&self, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Value) {
        let resp = self
            .router
            .clone()
            .oneshot(req)
            .await
            .expect("oneshot request");
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        (status, headers, json)
    }

    async fn send_raw(&self, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let resp = self
            .router
            .clone()
            .oneshot(req)
            .await
            .expect("oneshot request");
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes()
            .to_vec();
        (status, headers, bytes)
    }

    fn req_json(
        method: Method,
        uri: &str,
        cookies: &[&str],
        extra_headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        if !cookies.is_empty() {
            builder = builder.header(header::COOKIE, cookies.join("; "));
        }
        for (k, v) in extra_headers {
            builder = builder.header(*k, *v);
        }
        let body = match body {
            Some(v) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let mut req = builder.body(body).expect("build request");
        req.extensions_mut().insert(ConnectInfo(Self::loopback()));
        req
    }

    fn req_multipart(
        uri: &str,
        cookies: &[&str],
        boundary: &str,
        body_bytes: Vec<u8>,
    ) -> Request<Body> {
        let mut builder = Request::builder().method(Method::POST).uri(uri).header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        );
        if !cookies.is_empty() {
            builder = builder.header(header::COOKIE, cookies.join("; "));
        }
        let mut req = builder
            .body(Body::from(body_bytes))
            .expect("build multipart");
        req.extensions_mut().insert(ConnectInfo(Self::loopback()));
        req
    }

    async fn initialize_admin(&self) -> (String, String) {
        let req = Self::req_json(
            Method::POST,
            "/api/setup/initialize",
            &[],
            &[],
            Some(json!({
                "admin_username": "admin",
                "admin_display_name": "Admin",
                "admin_profile_pin": "1234",
                "server_pin": "123456",
                "managed_uploads_dir": self.uploads_dir.to_string_lossy(),
                "thumbnails_dir": self.thumbs_dir.to_string_lossy(),
                "cache_dir": self.cache_dir.to_string_lossy(),
                "backup_dir": self._tmp.path().join("backups").to_string_lossy(),
                "initial_folder_path": null,
                "initial_folder_name": null
            })),
        );
        let (status, headers, body) = self.send(req).await;
        assert_eq!(status, StatusCode::CREATED, "setup init failed: {body}");
        let cookie = extract_cookie_pair(&headers, "tp_session").expect("tp_session cookie");
        let id = body["user"]["id"].as_str().unwrap().to_string();
        (id, cookie)
    }

    async fn wait_for_job_completed(&self, job_id: i64) {
        for _ in 0..200 {
            let status: Option<(String, Option<String>)> = self
                .db
                .read(move |conn| {
                    let mut stmt =
                        conn.prepare("SELECT status, error_message FROM jobs WHERE id = ?1")?;
                    let mut rows = stmt.query(params![job_id])?;
                    if let Some(row) = rows.next()? {
                        Ok(Some((row.get(0)?, row.get(1)?)))
                    } else {
                        Ok(None)
                    }
                })
                .await
                .expect("query job status");
            match status.as_ref().map(|(s, _)| s.as_str()) {
                Some("completed") => return,
                Some("failed") => panic!("job {job_id} failed: {:?}", status.and_then(|(_, e)| e)),
                _ => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
        panic!("timed out waiting for job {job_id} to complete");
    }
}

fn extract_cookie_pair(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    let raw = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|s| s.starts_with(&format!("{name}=")))?;
    let pair = raw.split(';').next()?.trim();
    if pair.ends_with('=') {
        None
    } else {
        Some(pair.to_string())
    }
}

fn make_test_jpeg_bytes(width: u32, height: u32) -> Vec<u8> {
    let ffmpeg = video_capabilities()
        .ffmpeg_path
        .as_deref()
        .expect("ffmpeg must be installed for phase1 integration tests");
    let src = format!("testsrc=duration=1:size={width}x{height}:rate=1");
    let out = Command::new(ffmpeg)
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            &src,
            "-frames:v",
            "1",
            "-f",
            "image2pipe",
            "-vcodec",
            "mjpeg",
            "-",
        ])
        .output()
        .expect("spawn ffmpeg for test jpeg");
    assert!(
        out.status.success() && out.stdout.starts_with(&[0xFF, 0xD8]),
        "ffmpeg failed to generate jpeg: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn make_test_png_bytes(width: u32, height: u32) -> Vec<u8> {
    let ffmpeg = video_capabilities()
        .ffmpeg_path
        .as_deref()
        .expect("ffmpeg must be installed for phase1 integration tests");
    let src = format!("color=c=blue:size={width}x{height}:d=1");
    let out = Command::new(ffmpeg)
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            &src,
            "-frames:v",
            "1",
            "-f",
            "image2pipe",
            "-vcodec",
            "png",
            "-",
        ])
        .output()
        .expect("spawn ffmpeg for test png");
    assert!(
        out.status.success() && out.stdout.starts_with(b"\x89PNG"),
        "ffmpeg failed to generate png: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn inject_jpeg_app1_segments(jpeg: &[u8], segments: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(jpeg.len() + 4096);
    out.extend_from_slice(&jpeg[..2]);
    for payload in segments {
        let len = (payload.len() + 2) as u16;
        out.push(0xFF);
        out.push(0xE1);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(payload);
    }
    out.extend_from_slice(&jpeg[2..]);
    out
}

fn build_apple_makernote_blob(uuid: &str) -> Vec<u8> {
    let mut blob = Vec::new();
    blob.extend_from_slice(b"Apple iOS\0");
    blob.extend_from_slice(&[0x00, 0x01]);
    blob.extend_from_slice(b"MM");
    blob.extend_from_slice(&1u16.to_be_bytes());

    let mut val = uuid.as_bytes().to_vec();
    val.push(0);
    let val_offset = 32u32;

    blob.extend_from_slice(&0x0011u16.to_be_bytes());
    blob.extend_from_slice(&2u16.to_be_bytes());
    blob.extend_from_slice(&(val.len() as u32).to_be_bytes());
    blob.extend_from_slice(&val_offset.to_be_bytes());
    blob.extend_from_slice(&0u32.to_be_bytes());
    blob.extend_from_slice(&val);
    blob
}

fn build_isobmff_box(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = (8 + payload.len()) as u32;
    let mut out = Vec::with_capacity(size as usize);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(tag);
    out.extend_from_slice(payload);
    out
}

fn build_isobmff_with_content_id(uuid: &str) -> Vec<u8> {
    let ftyp = build_isobmff_box(b"ftyp", b"qt  \x00\x00\x02\x00qt  ");
    let key_name = b"com.apple.quicktime.content.identifier";
    let key_entry_size = (8 + key_name.len()) as u32;
    let mut keys_payload = Vec::new();
    keys_payload.extend_from_slice(&0u32.to_be_bytes());
    keys_payload.extend_from_slice(&1u32.to_be_bytes());
    keys_payload.extend_from_slice(&key_entry_size.to_be_bytes());
    keys_payload.extend_from_slice(b"mdta");
    keys_payload.extend_from_slice(key_name);
    let keys_box = build_isobmff_box(b"keys", &keys_payload);

    let mut data_payload = Vec::new();
    data_payload.extend_from_slice(&1u32.to_be_bytes());
    data_payload.extend_from_slice(&0u32.to_be_bytes());
    data_payload.extend_from_slice(uuid.as_bytes());
    let data_box = build_isobmff_box(b"data", &data_payload);

    let item_box = build_isobmff_box(&1u32.to_be_bytes(), &data_box);
    let ilst_box = build_isobmff_box(b"ilst", &item_box);

    let mut meta_payload = Vec::new();
    meta_payload.extend_from_slice(&0u32.to_be_bytes());
    meta_payload.extend_from_slice(&keys_box);
    meta_payload.extend_from_slice(&ilst_box);
    let meta_box = build_isobmff_box(b"meta", &meta_payload);
    let moov_box = build_isobmff_box(b"moov", &meta_box);

    let mut out = Vec::new();
    out.extend_from_slice(&ftyp);
    out.extend_from_slice(&moov_box);
    out
}

fn build_synthetic_exif_app1(makernote: Option<&[u8]>) -> Vec<u8> {
    struct Entry {
        tag: u16,
        ty: u16,
        count: u32,
        inline_or_data: EntryData,
    }
    enum EntryData {
        Inline([u8; 4]),
        Bytes(Vec<u8>),
    }

    fn write_ifd(buf: &mut Vec<u8>, entries: Vec<Entry>, next_ifd: u32) {
        let start = buf.len();
        let data_start = start + 2 + entries.len() * 12 + 4;
        buf.extend_from_slice(&(entries.len() as u16).to_le_bytes());

        let mut extra = Vec::new();
        for e in entries {
            buf.extend_from_slice(&e.tag.to_le_bytes());
            buf.extend_from_slice(&e.ty.to_le_bytes());
            buf.extend_from_slice(&e.count.to_le_bytes());
            match e.inline_or_data {
                EntryData::Inline(val) => buf.extend_from_slice(&val),
                EntryData::Bytes(bytes) => {
                    if bytes.len() <= 4 {
                        let mut padded = [0u8; 4];
                        padded[..bytes.len()].copy_from_slice(&bytes);
                        buf.extend_from_slice(&padded);
                    } else {
                        let offset = (data_start + extra.len()) as u32;
                        buf.extend_from_slice(&offset.to_le_bytes());
                        extra.extend_from_slice(&bytes);
                        if extra.len() % 2 == 1 {
                            extra.push(0);
                        }
                    }
                }
            }
        }
        buf.extend_from_slice(&next_ifd.to_le_bytes());
        buf.extend_from_slice(&extra);
    }

    fn rational_le(num: u32, den: u32) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[..4].copy_from_slice(&num.to_le_bytes());
        out[4..8].copy_from_slice(&den.to_le_bytes());
        out
    }

    let mut gps_lat_bytes = Vec::with_capacity(24);
    gps_lat_bytes.extend_from_slice(&rational_le(35, 1));
    gps_lat_bytes.extend_from_slice(&rational_le(40, 1));
    gps_lat_bytes.extend_from_slice(&rational_le(3432, 100));

    let mut gps_lon_bytes = Vec::with_capacity(24);
    gps_lon_bytes.extend_from_slice(&rational_le(139, 1));
    gps_lon_bytes.extend_from_slice(&rational_le(39, 1));
    gps_lon_bytes.extend_from_slice(&rational_le(108, 100));

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&42u16.to_le_bytes());
    tiff.extend_from_slice(&8u32.to_le_bytes());
    let exif_ifd_offset = 94u32;

    let ifd0_entries = vec![
        Entry {
            tag: 0x010F,
            ty: 2,
            count: 6,
            inline_or_data: EntryData::Bytes(b"Apple\0".to_vec()),
        },
        Entry {
            tag: 0x0110,
            ty: 2,
            count: 14,
            inline_or_data: EntryData::Bytes(b"iPhone 16 Pro\0".to_vec()),
        },
        Entry {
            tag: 0x0112,
            ty: 3,
            count: 1,
            inline_or_data: EntryData::Inline([6, 0, 0, 0]),
        },
        Entry {
            tag: 0x8769,
            ty: 4,
            count: 1,
            inline_or_data: EntryData::Inline(exif_ifd_offset.to_le_bytes()),
        },
        Entry {
            tag: 0x8825,
            ty: 4,
            count: 1,
            inline_or_data: EntryData::Inline([0, 0, 0, 0]),
        },
    ];
    write_ifd(&mut tiff, ifd0_entries, 0);

    let mut exif_entries = vec![
        Entry {
            tag: 0x829A,
            ty: 5,
            count: 1,
            inline_or_data: EntryData::Bytes(rational_le(1, 250).to_vec()),
        },
        Entry {
            tag: 0x829D,
            ty: 5,
            count: 1,
            inline_or_data: EntryData::Bytes(rational_le(18, 10).to_vec()),
        },
        Entry {
            tag: 0x8827,
            ty: 3,
            count: 1,
            inline_or_data: EntryData::Inline([100, 0, 0, 0]),
        },
        Entry {
            tag: 0x9003,
            ty: 2,
            count: 20,
            inline_or_data: EntryData::Bytes(b"2026:11:19 14:30:00\0".to_vec()),
        },
        Entry {
            tag: 0x920A,
            ty: 5,
            count: 1,
            inline_or_data: EntryData::Bytes(rational_le(24, 1).to_vec()),
        },
        Entry {
            tag: 0xA434,
            ty: 2,
            count: 13,
            inline_or_data: EntryData::Bytes(b"24mm f/1.78\0\0".to_vec()),
        },
    ];
    if let Some(mn) = makernote {
        exif_entries.push(Entry {
            tag: 0x927C,
            ty: 7,
            count: mn.len() as u32,
            inline_or_data: EntryData::Bytes(mn.to_vec()),
        });
    }
    exif_entries.sort_by_key(|e| e.tag);
    write_ifd(&mut tiff, exif_entries, 0);

    let gps_ifd_offset = tiff.len() as u32;
    tiff[66..70].copy_from_slice(&gps_ifd_offset.to_le_bytes());

    let gps_entries = vec![
        Entry {
            tag: 0x0001,
            ty: 2,
            count: 2,
            inline_or_data: EntryData::Inline([b'N', 0, 0, 0]),
        },
        Entry {
            tag: 0x0002,
            ty: 5,
            count: 3,
            inline_or_data: EntryData::Bytes(gps_lat_bytes),
        },
        Entry {
            tag: 0x0003,
            ty: 2,
            count: 2,
            inline_or_data: EntryData::Inline([b'E', 0, 0, 0]),
        },
        Entry {
            tag: 0x0004,
            ty: 5,
            count: 3,
            inline_or_data: EntryData::Bytes(gps_lon_bytes),
        },
    ];
    write_ifd(&mut tiff, gps_entries, 0);

    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&tiff);
    app1
}

fn build_synthetic_cr3_with_embedded_jpeg(jpeg: &[u8]) -> Vec<u8> {
    let jpeg_offset = 38u32;
    let jpeg_len = jpeg.len() as u32;

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&42u16.to_le_bytes());
    tiff.extend_from_slice(&8u32.to_le_bytes());
    tiff.extend_from_slice(&2u16.to_le_bytes());

    tiff.extend_from_slice(&0x0201u16.to_le_bytes());
    tiff.extend_from_slice(&4u16.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&jpeg_offset.to_le_bytes());

    tiff.extend_from_slice(&0x0202u16.to_le_bytes());
    tiff.extend_from_slice(&4u16.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&jpeg_len.to_le_bytes());

    tiff.extend_from_slice(&0u32.to_le_bytes());
    tiff.extend_from_slice(jpeg);
    tiff
}

fn generate_test_mp4(dest: &Path) {
    let caps = video_capabilities();
    let ffmpeg = caps
        .ffmpeg_path
        .as_deref()
        .expect("ffmpeg must be installed for phase1 integration tests");

    let out = Command::new(ffmpeg)
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=2:size=320x240:rate=24",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
            "-c:v",
            &caps.h264_encoder,
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-metadata",
            "creation_time=2026-11-19T14:30:00Z",
        ])
        .arg(dest)
        .output()
        .expect("spawn ffmpeg for test mp4");
    assert!(
        out.status.success(),
        "ffmpeg failed to generate test mp4: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test]
async fn phase1_e2e_ingest_media_uploads_rbac_trash_and_albums() {
    let app = TestHarness::new();
    let (_admin_id, admin_cookie) = app.initialize_admin().await;

    let folder_ro_dir = app._tmp.path().join("folder_ro");
    let folder_rw_dir = app._tmp.path().join("folder_rw");
    let folder_secret_dir = app._tmp.path().join("folder_secret");
    std::fs::create_dir_all(&folder_ro_dir).unwrap();
    std::fs::create_dir_all(&folder_rw_dir).unwrap();
    std::fs::create_dir_all(&folder_secret_dir).unwrap();

    // 1. Create watched folders
    let (s, _, f_ro) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/folders",
            &[&admin_cookie],
            &[],
            Some(json!({
                "name": "Library RO",
                "root_path": folder_ro_dir.to_string_lossy(),
                "allow_disk_delete": false,
                "is_managed_uploads": false
            })),
        ))
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let folder_ro_id = f_ro["folder"]["id"].as_str().unwrap().to_string();

    let (s, _, f_rw) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/folders",
            &[&admin_cookie],
            &[],
            Some(json!({
                "name": "Deletable",
                "root_path": folder_rw_dir.to_string_lossy(),
                "allow_disk_delete": true,
                "is_managed_uploads": false
            })),
        ))
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let folder_rw_id = f_rw["folder"]["id"].as_str().unwrap().to_string();

    let (s, _, f_sec) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/folders",
            &[&admin_cookie],
            &[],
            Some(json!({
                "name": "Secret",
                "root_path": folder_secret_dir.to_string_lossy(),
                "allow_disk_delete": false,
                "is_managed_uploads": false
            })),
        ))
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let folder_sec_id = f_sec["folder"]["id"].as_str().unwrap().to_string();

    // 2. Create Contributor and Viewer granted ONLY folder_ro & folder_rw (NOT folder_secret)
    let (s, _, c_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/admin/users",
            &[&admin_cookie],
            &[],
            Some(json!({
                "username": "contrib",
                "display_name": "Contributor",
                "role": "contributor",
                "profile_pin": "2222",
                "allowed_folder_ids": [&folder_ro_id, &folder_rw_id]
            })),
        ))
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let contrib_id = c_body["user"]["id"].as_str().unwrap().to_string();

    let (s, _, v_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/admin/users",
            &[&admin_cookie],
            &[],
            Some(json!({
                "username": "viewer",
                "display_name": "Viewer",
                "role": "viewer",
                "profile_pin": "3333",
                "allowed_folder_ids": [&folder_ro_id, &folder_rw_id]
            })),
        ))
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let viewer_id = v_body["user"]["id"].as_str().unwrap().to_string();

    let (_, c_hdrs, _) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/auth/login",
            &[],
            &[],
            Some(json!({ "user_id": contrib_id, "profile_pin": "2222", "server_pin": "123456" })),
        ))
        .await;
    let contrib_cookie = extract_cookie_pair(&c_hdrs, "tp_session").unwrap();

    let (_, v_hdrs, _) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/auth/login",
            &[],
            &[],
            Some(json!({ "user_id": viewer_id, "profile_pin": "3333", "server_pin": "123456" })),
        ))
        .await;
    let viewer_cookie = extract_cookie_pair(&v_hdrs, "tp_session").unwrap();

    // 3. Populate synthetic fixtures in folder_ro:
    //    - tokyo.jpg (JPEG + EXIF)
    let tokyo_path = folder_ro_dir.join("tokyo.jpg");
    let base_jpeg = make_test_jpeg_bytes(640, 480);
    let tokyo_bytes = inject_jpeg_app1_segments(&base_jpeg, &[build_synthetic_exif_app1(None)]);
    std::fs::write(&tokyo_path, &tokyo_bytes).unwrap();
    let tokyo_mtime_orig = std::fs::metadata(&tokyo_path).unwrap().modified().unwrap();

    //    - graphic.png (PNG)
    let png_path = folder_ro_dir.join("graphic.png");
    std::fs::write(&png_path, make_test_png_bytes(320, 240)).unwrap();

    //    - motion.jpg (Android Motion Photo)
    let motion_trailer = build_isobmff_with_content_id("ANDROID-MOTION-E2E");
    let trailer_len = motion_trailer.len() as u64;
    let xmp = format!(
        "http://ns.adobe.com/xap/1.0/\0<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:GCamera=\"http://ns.google.com/photos/1.0/camera/\" xmlns:Container=\"http://ns.google.com/photos/1.0/container/\" xmlns:Item=\"http://ns.google.com/photos/1.0/container/item/\" GCamera:MotionPhoto=\"1\" GCamera:MicroVideoOffset=\"{trailer_len}\"><Container:Directory><rdf:Seq><rdf:li rdf:parseType=\"Resource\"><Container:Item Item:Semantic=\"Primary\" Item:Mime=\"image/jpeg\"/></rdf:li><rdf:li rdf:parseType=\"Resource\"><Container:Item Item:Semantic=\"MotionPhoto\" Item:Mime=\"video/mp4\" Item:Length=\"{trailer_len}\"/></rdf:li></rdf:Seq></Container:Directory></rdf:Description></rdf:RDF></x:xmpmeta>"
    );
    let mut motion_bytes =
        inject_jpeg_app1_segments(&make_test_jpeg_bytes(320, 240), &[xmp.into_bytes()]);
    motion_bytes.extend_from_slice(&motion_trailer);
    std::fs::write(folder_ro_dir.join("motion.jpg"), &motion_bytes).unwrap();

    //    - Apple Live Photo pair (IMG_1024.JPG + IMG_1024.MOV sharing ContentIdentifier)
    let live_cid = "7A9C4E12-3B8D-4F11-9D2A-1234567890AB";
    let mn_app1 = build_synthetic_exif_app1(Some(&build_apple_makernote_blob(live_cid)));
    let live_jpg = inject_jpeg_app1_segments(&make_test_jpeg_bytes(400, 300), &[mn_app1]);
    std::fs::write(folder_ro_dir.join("IMG_1024.JPG"), &live_jpg).unwrap();
    let live_mov = build_isobmff_with_content_id(live_cid);
    std::fs::write(folder_ro_dir.join("IMG_1024.MOV"), &live_mov).unwrap();

    //    - RAW + JPG pair (DSC0001.CR3 + DSC0001.JPG)
    let raw_embedded = make_test_jpeg_bytes(640, 480);
    std::fs::write(
        folder_ro_dir.join("DSC0001.CR3"),
        build_synthetic_cr3_with_embedded_jpeg(&raw_embedded),
    )
    .unwrap();
    std::fs::write(
        folder_ro_dir.join("DSC0001.JPG"),
        make_test_jpeg_bytes(640, 480),
    )
    .unwrap();

    //    - Real 2-second H.264 MP4 via ffmpeg
    generate_test_mp4(&folder_ro_dir.join("clip.mp4"));

    //    - Asset in folder_rw (for allow_disk_delete = true purge test)
    let deletable_path = folder_rw_dir.join("deletable.jpg");
    std::fs::write(&deletable_path, make_test_jpeg_bytes(256, 256)).unwrap();

    //    - Asset in folder_secret (for RBAC isolation test)
    std::fs::write(
        folder_secret_dir.join("secret.jpg"),
        make_test_jpeg_bytes(256, 256),
    )
    .unwrap();

    // 4. Trigger scans via POST /api/folders/{id}/scan and verify 202 + completion
    for fid in [&folder_ro_id, &folder_rw_id, &folder_sec_id] {
        let (s, _, body) = app
            .send(TestHarness::req_json(
                Method::POST,
                &format!("/api/folders/{fid}/scan"),
                &[&admin_cookie],
                &[],
                None,
            ))
            .await;
        assert_eq!(s, StatusCode::ACCEPTED, "scan {fid} failed: {body}");
        let job_id = body["job_id"].as_i64().unwrap();
        app.wait_for_job_completed(job_id).await;
    }

    // Verify scan-all returns 202 with all 3 folders
    let (s, _, scan_all_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/folders/scan-all",
            &[&admin_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    for jid in scan_all_body["job_ids"].as_array().unwrap() {
        app.wait_for_job_completed(jid.as_i64().unwrap()).await;
    }

    // Check asset list counts & identify asset IDs by filename
    let (s, _, admin_assets) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/assets",
            &[&admin_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    let admin_items = admin_assets["items"].as_array().unwrap();
    assert_eq!(
        admin_items.len(),
        8,
        "expected 8 logical assets: {admin_assets}"
    );

    let mut tokyo_id = String::new();
    let mut png_id = String::new();
    let mut motion_id = String::new();
    let mut live_id = String::new();
    let mut raw_stack_id = String::new();
    let mut video_id = String::new();
    let mut deletable_id = String::new();
    let mut secret_id = String::new();

    for item in admin_items {
        let id = item["id"].as_str().unwrap().to_string();
        let (ds, _, detail) = app
            .send(TestHarness::req_json(
                Method::GET,
                &format!("/api/assets/{id}"),
                &[&admin_cookie],
                &[],
                None,
            ))
            .await;
        assert_eq!(ds, StatusCode::OK);
        let rel = detail["files"][0]["rel_path"].as_str().unwrap();
        if rel == "tokyo.jpg" {
            tokyo_id = id;
        } else if rel == "graphic.png" {
            png_id = id;
        } else if rel == "motion.jpg" {
            motion_id = id;
            assert_eq!(detail["summary"]["kind"], "motion_photo");
        } else if rel == "IMG_1024.JPG" {
            live_id = id;
            assert_eq!(detail["summary"]["kind"], "live_photo");
        } else if rel == "DSC0001.CR3" || rel == "DSC0001.JPG" {
            raw_stack_id = id;
            assert_eq!(detail["summary"]["kind"], "raw_jpg_stack");
            assert_eq!(detail["playback"]["has_raw"], true);
        } else if rel == "clip.mp4" {
            video_id = id;
            assert_eq!(detail["summary"]["kind"], "video");
        } else if rel == "deletable.jpg" {
            deletable_id = id;
        } else if rel == "secret.jpg" {
            secret_id = id;
        }
    }
    assert!(!tokyo_id.is_empty());
    assert!(!png_id.is_empty());
    assert!(!motion_id.is_empty());
    assert!(!live_id.is_empty());
    assert!(!raw_stack_id.is_empty());
    assert!(!video_id.is_empty());
    assert!(!deletable_id.is_empty());
    assert!(!secret_id.is_empty());

    // 5. Media Serving Verification
    // 5a. /thumb -> 200 image/webp (RIFF....WEBP)
    let (s, hdrs, thumb_bytes) = app
        .send_raw(TestHarness::req_json(
            Method::GET,
            &format!("/api/media/{tokyo_id}/thumb"),
            &[&contrib_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(hdrs.get(header::CONTENT_TYPE).unwrap(), "image/webp");
    assert!(thumb_bytes.starts_with(b"RIFF") && &thumb_bytes[8..12] == b"WEBP");

    // 5b. /preview -> 200 image/webp + creates lru_cache_entries row
    let (s, hdrs, prev_bytes) = app
        .send_raw(TestHarness::req_json(
            Method::GET,
            &format!("/api/media/{tokyo_id}/preview"),
            &[&contrib_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(hdrs.get(header::CONTENT_TYPE).unwrap(), "image/webp");
    assert!(prev_bytes.starts_with(b"RIFF") && &prev_bytes[8..12] == b"WEBP");
    let preview_cache_key = format!("preview_2048:{tokyo_id}");
    let cached_row = app
        .db
        .read(move |conn| truongphoto_db::repo::cache::get_cache_entry(conn, &preview_cache_key))
        .await
        .unwrap();
    assert!(
        cached_row.is_some(),
        "preview should create lru_cache_entries row"
    );

    // 5c. /original with Range: bytes=0-1023 -> 206 Partial Content
    let (s, hdrs, range_bytes) = app
        .send_raw(TestHarness::req_json(
            Method::GET,
            &format!("/api/media/{tokyo_id}/original"),
            &[&contrib_cookie],
            &[("Range", "bytes=0-1023")],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::PARTIAL_CONTENT);
    assert_eq!(range_bytes.len(), 1024);
    assert_eq!(&range_bytes[..], &tokyo_bytes[..1024]);
    let cr = hdrs.get(header::CONTENT_RANGE).unwrap().to_str().unwrap();
    assert_eq!(cr, format!("bytes 0-1023/{}", tokyo_bytes.len()));

    // 5d. RAW demosaic + download header
    let (s, hdrs, demosaic_bytes) = app
        .send_raw(TestHarness::req_json(
            Method::GET,
            &format!("/api/media/{raw_stack_id}/original?mode=raw_demosaic&download=1"),
            &[&contrib_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(hdrs.get(header::CONTENT_TYPE).unwrap(), "image/jpeg");
    assert!(hdrs.get("X-TruongPhoto-Decoder").is_some());
    assert!(hdrs
        .get(header::CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap()
        .contains("attachment"));
    assert!(demosaic_bytes.starts_with(&[0xFF, 0xD8, 0xFF]));

    // 5e. /live-video on Motion Photo & Apple Live Photo
    let (s, _, mp_vid_bytes) = app
        .send_raw(TestHarness::req_json(
            Method::GET,
            &format!("/api/media/{motion_id}/live-video"),
            &[&contrib_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(mp_vid_bytes, motion_trailer);

    let (s, _, live_vid_bytes) = app
        .send_raw(TestHarness::req_json(
            Method::GET,
            &format!("/api/media/{live_id}/live-video"),
            &[&contrib_cookie],
            &[("Range", "bytes=0-15")],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::PARTIAL_CONTENT);
    assert_eq!(live_vid_bytes, &live_mov[..16]);

    // 5f. HLS playlist + segment + path traversal rejection
    let (s, hdrs, m3u8_bytes) = app
        .send_raw(TestHarness::req_json(
            Method::GET,
            &format!("/api/media/{video_id}/hls/index.m3u8"),
            &[&contrib_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        hdrs.get(header::CONTENT_TYPE).unwrap(),
        "application/vnd.apple.mpegurl"
    );
    let m3u8 = String::from_utf8(m3u8_bytes).unwrap();
    assert!(m3u8.contains("#EXTM3U"));
    assert!(m3u8.contains("seg_0000.ts"));

    let (s, hdrs, seg_bytes) = app
        .send_raw(TestHarness::req_json(
            Method::GET,
            &format!("/api/media/{video_id}/hls/seg_0000.ts"),
            &[&contrib_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(hdrs.get(header::CONTENT_TYPE).unwrap(), "video/mp2t");
    assert!(!seg_bytes.is_empty());

    for bad_seg in ["..%2F..%2Fsecret", "evil.ts", "seg_001.ts"] {
        let (s, _, _) = app
            .send(TestHarness::req_json(
                Method::GET,
                &format!("/api/media/{video_id}/hls/{bad_seg}"),
                &[&contrib_cookie],
                &[],
                None,
            ))
            .await;
        assert!(
            s == StatusCode::BAD_REQUEST || s == StatusCode::NOT_FOUND,
            "expected 400/404 for {bad_seg}, got {s}"
        );
    }

    // 6. Streaming Upload (POST /api/uploads)
    let upload_jpeg = make_test_jpeg_bytes(300, 200);
    let boundary = "----TruongPhotoTestBoundary123";
    let mut mp_body = Vec::new();
    mp_body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"files[]\"; filename=\"vacation.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n"
        )
        .as_bytes(),
    );
    mp_body.extend_from_slice(&upload_jpeg);
    mp_body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let (s, _, up_body) = app
        .send(TestHarness::req_multipart(
            "/api/uploads",
            &[&contrib_cookie],
            boundary,
            mp_body.clone(),
        ))
        .await;
    assert_eq!(s, StatusCode::CREATED, "upload failed: {up_body}");
    assert_eq!(up_body["uploaded"].as_array().unwrap().len(), 1);
    assert!(up_body["errors"].as_array().unwrap().is_empty());
    let uploaded_id = up_body["uploaded"][0]["id"].as_str().unwrap().to_string();

    // Verify stored under managed_uploads_dir/YYYY/MM/
    let now = chrono::Utc::now();
    let expected_month_dir = app
        .uploads_dir
        .join(format!("{:04}", chrono::Datelike::year(&now)))
        .join(format!("{:02}", chrono::Datelike::month(&now)));
    assert!(expected_month_dir.join("vacation.jpg").is_file());

    // Viewer upload -> 403
    let (s, _, _) = app
        .send(TestHarness::req_multipart(
            "/api/uploads",
            &[&viewer_cookie],
            boundary,
            mp_body,
        ))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // 7. Overrides, Tags & RBAC
    let (s, _, fav_res) = app
        .send(TestHarness::req_json(
            Method::PATCH,
            &format!("/api/assets/{tokyo_id}/favorite"),
            &[&contrib_cookie],
            &[],
            Some(json!({ "is_favorite": true })),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(fav_res["is_favorite"], true);

    let (s, _, ov_res) = app
        .send(TestHarness::req_json(
            Method::PATCH,
            &format!("/api/assets/{tokyo_id}/overrides"),
            &[&contrib_cookie],
            &[],
            Some(json!({ "title": "Shibuya Sunset", "rating": 5 })),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(ov_res["summary"]["title"], "Shibuya Sunset");
    assert_eq!(ov_res["summary"]["rating"], 5);

    let (s, _, tag_res) = app
        .send(TestHarness::req_json(
            Method::POST,
            &format!("/api/assets/{tokyo_id}/tags"),
            &[&contrib_cookie],
            &[],
            Some(json!({ "tag": "sunset" })),
        ))
        .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(tag_res["tags"][0]["tag"], "sunset");

    let (s, _, tag_del_res) = app
        .send(TestHarness::req_json(
            Method::DELETE,
            &format!("/api/assets/{tokyo_id}/tags/sunset"),
            &[&contrib_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(tag_del_res["tags"].as_array().unwrap().is_empty());

    // Re-add a tag for inspection verification
    let (s, _, _) = app
        .send(TestHarness::req_json(
            Method::POST,
            &format!("/api/assets/{tokyo_id}/tags"),
            &[&contrib_cookie],
            &[],
            Some(json!({ "tag": "tokyo" })),
        ))
        .await;
    assert_eq!(s, StatusCode::CREATED);

    let (s, _, detail_after) = app
        .send(TestHarness::req_json(
            Method::GET,
            &format!("/api/assets/{tokyo_id}"),
            &[&contrib_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(detail_after["summary"]["title"], "Shibuya Sunset");
    assert_eq!(detail_after["tags"][0]["tag"], "tokyo");

    // Verify before_ms filter works on GET /api/assets
    let (s, _, before_res) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/assets?before_ms=1000",
            &[&contrib_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(before_res["items"].as_array().unwrap().is_empty());

    // Original file untouched & zero .xmp files created
    assert_eq!(
        std::fs::metadata(&tokyo_path).unwrap().modified().unwrap(),
        tokyo_mtime_orig
    );
    assert_eq!(std::fs::read(&tokyo_path).unwrap(), tokyo_bytes);
    let has_xmp = walkdir::WalkDir::new(&folder_ro_dir)
        .into_iter()
        .filter_map(Result::ok)
        .any(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x.eq_ignore_ascii_case("xmp"))
        });
    assert!(!has_xmp, "zero .xmp files must be created");

    // Viewer RBAC checks: 403 on mutations, 404 on restricted thumb/detail
    for (method, uri, body) in [
        (
            Method::PATCH,
            format!("/api/assets/{tokyo_id}/favorite"),
            Some(json!({ "is_favorite": false })),
        ),
        (
            Method::PATCH,
            format!("/api/assets/{tokyo_id}/overrides"),
            Some(json!({ "title": "Nope" })),
        ),
        (
            Method::POST,
            format!("/api/assets/{tokyo_id}/tags"),
            Some(json!({ "tag": "nope" })),
        ),
        (
            Method::DELETE,
            format!("/api/assets/{tokyo_id}/tags/tokyo"),
            None,
        ),
        (
            Method::POST,
            "/api/assets/trash".to_string(),
            Some(json!({ "asset_ids": [&tokyo_id] })),
        ),
        (
            Method::POST,
            "/api/trash/restore".to_string(),
            Some(json!({ "asset_ids": [&tokyo_id] })),
        ),
        (
            Method::POST,
            "/api/trash/purge".to_string(),
            Some(json!({ "all": true })),
        ),
    ] {
        let (s, _, _) = app
            .send(TestHarness::req_json(
                method,
                &uri,
                &[&viewer_cookie],
                &[],
                body,
            ))
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }

    let (s, _, _) = app
        .send(TestHarness::req_json(
            Method::GET,
            &format!("/api/media/{secret_id}/thumb"),
            &[&viewer_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // 8. Trash, allow_disk_delete & 30-day auto-purge
    let (s, _, tr_res) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/assets/trash",
            &[&contrib_cookie],
            &[],
            Some(json!({ "asset_ids": [&png_id] })),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(tr_res["trashed_count"], 1);

    // Visible in /trash, hidden from /assets
    let (s, _, trash_list) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/trash",
            &[&contrib_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(trash_list["items"].as_array().unwrap().len(), 1);
    assert_eq!(trash_list["items"][0]["id"], png_id);

    // Contributor cannot purge -> 403
    let (s, _, _) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/trash/purge",
            &[&contrib_cookie],
            &[],
            Some(json!({ "asset_ids": [&png_id] })),
        ))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Admin purge in allow_disk_delete = false -> DB hidden, disk file STILL EXISTS!
    let (s, _, purge_ro) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/trash/purge",
            &[&admin_cookie],
            &[],
            Some(json!({ "asset_ids": [&png_id] })),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(purge_ro["purged_count"], 1);
    assert_eq!(purge_ro["db_only_assets"], 1);
    assert!(
        png_path.is_file(),
        "allow_disk_delete=false must keep disk file"
    );

    // Admin purge in allow_disk_delete = true -> DB + thumb + disk file REMOVED!
    let (s, _, _) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/assets/trash",
            &[&contrib_cookie],
            &[],
            Some(json!({ "asset_ids": [&deletable_id] })),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);

    let (s, _, purge_rw) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/trash/purge",
            &[&admin_cookie],
            &[],
            Some(json!({ "asset_ids": [&deletable_id] })),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(purge_rw["purged_count"], 1);
    assert!(purge_rw["disk_deleted_files"].as_u64().unwrap() >= 1);
    assert!(
        !deletable_path.exists(),
        "allow_disk_delete=true must delete disk file"
    );

    // 30-day auto-purge sweep: trash uploaded_id & motion_id, backdate uploaded_id > 30 days
    let (s, _, _) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/assets/trash",
            &[&contrib_cookie],
            &[],
            Some(json!({ "asset_ids": [&uploaded_id, &motion_id] })),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);

    let old_ts = chrono::Utc::now().timestamp_millis() - 31 * 86_400_000;
    let uid_clone = uploaded_id.clone();
    app.db
        .write(move |conn| {
            conn.execute(
                "UPDATE assets SET deleted_at_ms = ?1 WHERE id = ?2",
                params![old_ts, uid_clone],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let sweep_report = run_trash_retention_sweep(
        &app.db,
        &app.thumbs_dir,
        &app.cache_dir,
        &app.state.cache_mgr,
        Some(&app.state.ai),
        30,
    )
    .await
    .unwrap();
    assert_eq!(sweep_report.purged_count, 1);

    // Recent trashed item (motion_id) remains in trash and can be restored
    let (s, _, trash_after_sweep) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/trash",
            &[&contrib_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(trash_after_sweep["items"].as_array().unwrap().len(), 1);
    assert_eq!(trash_after_sweep["items"][0]["id"], motion_id);

    let (s, _, restore_res) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/trash/restore",
            &[&contrib_cookie],
            &[],
            Some(json!({ "asset_ids": [&motion_id] })),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(restore_res["restored_count"], 1);

    // 9. Albums CRUD & Sharing
    let (s, _, shareable) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/users/shareable",
            &[&contrib_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(shareable["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|u| u["id"] == viewer_id));

    let (s, _, alb_res) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/albums",
            &[&contrib_cookie],
            &[],
            Some(json!({
                "title": "Japan 2026",
                "description": "Trip photos",
                "asset_ids": [&tokyo_id]
            })),
        ))
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let album_id = alb_res["album"]["id"].as_str().unwrap().to_string();

    // Admin adds secret_id to album to verify Viewer filtering inside shared album
    let (s, _, add_sec) = app
        .send(TestHarness::req_json(
            Method::POST,
            &format!("/api/albums/{album_id}/assets"),
            &[&admin_cookie],
            &[],
            Some(json!({ "asset_ids": [&secret_id] })),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(add_sec["added_count"], 1);

    // Contributor shares (view) with Viewer
    let (s, _, shares_res) = app
        .send(TestHarness::req_json(
            Method::PUT,
            &format!("/api/albums/{album_id}/shares"),
            &[&contrib_cookie],
            &[],
            Some(json!({
                "shares": [{ "user_id": &viewer_id, "permission": "view" }]
            })),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(shares_res["shares"].as_array().unwrap().len(), 1);

    // Viewer sees ONLY tokyo_id (NOT secret_id!)
    let (s, _, viewer_alb) = app
        .send(TestHarness::req_json(
            Method::GET,
            &format!("/api/albums/{album_id}"),
            &[&viewer_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    let v_assets = viewer_alb["assets"].as_array().unwrap();
    assert_eq!(v_assets.len(), 1);
    assert_eq!(v_assets[0]["id"], tokyo_id);

    // Viewer cannot mutate album assets -> 403
    let (s, _, _) = app
        .send(TestHarness::req_json(
            Method::POST,
            &format!("/api/albums/{album_id}/assets"),
            &[&viewer_cookie],
            &[],
            Some(json!({ "asset_ids": [&motion_id] })),
        ))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Contributor edits, removes asset, deletes album
    let (s, _, upd_alb) = app
        .send(TestHarness::req_json(
            Method::PATCH,
            &format!("/api/albums/{album_id}"),
            &[&contrib_cookie],
            &[],
            Some(json!({ "title": "Japan Autumn" })),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(upd_alb["album"]["title"], "Japan Autumn");

    let (s, _, rem_alb) = app
        .send(TestHarness::req_json(
            Method::DELETE,
            &format!("/api/albums/{album_id}/assets"),
            &[&contrib_cookie],
            &[],
            Some(json!({ "asset_ids": [&tokyo_id] })),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(rem_alb["removed_count"], 1);

    let (s, _, _) = app
        .send(TestHarness::req_json(
            Method::DELETE,
            &format!("/api/albums/{album_id}"),
            &[&contrib_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    app.db.shutdown().await.expect("db shutdown");
}

#[tokio::test]
async fn cache_manager_evicts_oldest_down_to_85_percent() {
    let app = TestHarness::new();
    let p1 = app.cache_dir.join("previews/old.webp");
    let p2 = app.cache_dir.join("previews/new.webp");
    std::fs::create_dir_all(p1.parent().unwrap()).unwrap();
    std::fs::write(&p1, b"old-bytes").unwrap();
    std::fs::write(&p2, b"new-bytes").unwrap();

    let limit_1gb: u64 = 1024 * 1024 * 1024;
    app.state
        .cache_mgr
        .record_and_evict(
            LruCacheEntry {
                cache_key: "k-old".to_string(),
                asset_id: None,
                kind: CacheEntryKind::Preview2048,
                rel_path: "previews/old.webp".to_string(),
                size_bytes: limit_1gb / 2 + 100,
                last_accessed_ms: 1000,
                created_at_ms: 1000,
            },
            &app.cache_dir,
            1,
        )
        .await
        .unwrap();

    app.state
        .cache_mgr
        .record_and_evict(
            LruCacheEntry {
                cache_key: "k-new".to_string(),
                asset_id: None,
                kind: CacheEntryKind::Preview2048,
                rel_path: "previews/new.webp".to_string(),
                size_bytes: limit_1gb / 2 + 100,
                last_accessed_ms: 2000,
                created_at_ms: 2000,
            },
            &app.cache_dir,
            1,
        )
        .await
        .unwrap();

    assert!(!p1.exists(), "oldest cache entry must be evicted from disk");
    assert!(p2.exists(), "newest cache entry must remain on disk");
    app.db.shutdown().await.expect("db shutdown");
}
