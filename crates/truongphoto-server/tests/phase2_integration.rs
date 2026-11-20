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
use truongphoto_ai::AiPipeline;
use truongphoto_core::{PowerState, ServerConfig, StoragePaths, WsServerEvent};
use truongphoto_db::{rusqlite::params, DbHandle};
use truongphoto_media::video::video_capabilities;
use truongphoto_server::{
    ai::worker::process_stage2_batch, auth, build_router, mdns::MdnsHandle, state::AppState,
};
use uuid::Uuid;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("tp-p2-server-test-{}", Uuid::new_v4()));
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
    fn new_deterministic() -> Self {
        let tmp = TempDir::new().expect("create temp dir");
        let mut config = ServerConfig::from_data_dir(tmp.path().to_path_buf());
        config.mdns_enabled = false;

        let default_paths = StoragePaths::from_data_dir(config.data_dir.clone());
        default_paths.ensure_dirs().expect("ensure dirs");

        let db = DbHandle::open(&config.db_path).expect("open test db");
        let gate_key = auth::generate_process_key().expect("gate key");
        let (ws_tx, _) = tokio::sync::broadcast::channel::<WsServerEvent>(256);
        let (_power_tx, power_rx) = watch::channel(PowerState::default());
        let mdns = MdnsHandle::start(config.mdns_hostname.clone(), 38420, false);
        let hardware = truongphoto_ai::hardware::detect_hardware_info();
        let shutdown_token = tokio_util::sync::CancellationToken::new();

        let state = Arc::new(
            AppState::new(
                db.clone(),
                config,
                gate_key,
                ws_tx,
                power_rx,
                mdns,
                hardware,
                38420,
                shutdown_token,
            )
            .with_ai_pipeline(AiPipeline::deterministic_test()),
        );
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

    fn req_json(method: Method, uri: &str, cookies: &[&str], body: Option<Value>) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        if !cookies.is_empty() {
            builder = builder.header(header::COOKIE, cookies.join("; "));
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

    async fn initialize_admin(&self) -> (String, String) {
        let req = Self::req_json(
            Method::POST,
            "/api/setup/initialize",
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

    async fn create_user_and_login(
        &self,
        admin_cookie: &str,
        username: &str,
        role: &str,
        allowed_folder_ids: &[&str],
    ) -> String {
        let (status, _, body) = self
            .send(Self::req_json(
                Method::POST,
                "/api/admin/users",
                &[admin_cookie],
                Some(json!({
                    "username": username,
                    "display_name": username,
                    "role": role,
                    "profile_pin": "2222",
                    "allowed_folder_ids": allowed_folder_ids
                })),
            ))
            .await;
        assert_eq!(status, StatusCode::CREATED, "create user failed: {body}");
        let user_id = body["user"]["id"].as_str().unwrap().to_string();

        let (status, headers, login_body) = self
            .send(Self::req_json(
                Method::POST,
                "/api/auth/login",
                &[],
                Some(json!({
                    "user_id": user_id,
                    "profile_pin": "2222",
                    "server_pin": "123456"
                })),
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "login failed: {login_body}");
        extract_cookie_pair(&headers, "tp_session").expect("user session cookie")
    }

    async fn create_folder(&self, admin_cookie: &str, name: &str, root_path: &Path) -> String {
        std::fs::create_dir_all(root_path).expect("create folder dir");
        let (status, _, body) = self
            .send(Self::req_json(
                Method::POST,
                "/api/folders",
                &[admin_cookie],
                Some(json!({
                    "name": name,
                    "root_path": root_path.to_string_lossy()
                })),
            ))
            .await;
        assert_eq!(status, StatusCode::CREATED, "create folder failed: {body}");
        body["folder"]["id"].as_str().unwrap().to_string()
    }

    async fn scan_folder_and_wait(&self, admin_cookie: &str, folder_id: &str) {
        let (status, _, body) = self
            .send(Self::req_json(
                Method::POST,
                &format!("/api/folders/{folder_id}/scan"),
                &[admin_cookie],
                None,
            ))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "scan folder failed: {body}");
        let job_id = body["job_id"].as_i64().expect("job_id");
        self.wait_for_job_completed(job_id).await;
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

    async fn drain_stage2(&self) {
        for _ in 0..20 {
            let processed = process_stage2_batch(&self.state, 50)
                .await
                .expect("process stage2 batch");
            if processed == 0 {
                break;
            }
        }
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

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let mut s1 = 1u32;
    let mut s2 = 0u32;
    for &b in bytes {
        s1 = (s1 + u32::from(b)) % 65521;
        s2 = (s2 + s1) % 65521;
    }
    (s2 << 16) | s1
}

fn write_png_chunk(out: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(tag);
    out.extend_from_slice(data);
    let mut crc_buf = Vec::with_capacity(4 + data.len());
    crc_buf.extend_from_slice(tag);
    crc_buf.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_buf).to_be_bytes());
}

fn encode_rgb_png(width: u32, height: u32, rgb_rows: &[u8]) -> Vec<u8> {
    let row_len = (width as usize) * 3;
    let mut raw = Vec::with_capacity((row_len + 1) * (height as usize));
    for y in 0..(height as usize) {
        raw.push(0); // filter type 0
        raw.extend_from_slice(&rgb_rows[y * row_len..(y + 1) * row_len]);
    }

    // Stored (uncompressed) zlib stream
    let mut zlib = Vec::with_capacity(raw.len() + 64);
    zlib.extend_from_slice(&[0x78, 0x01]); // CMF/FLG
    let mut offset = 0usize;
    while offset < raw.len() {
        let chunk_len = (raw.len() - offset).min(65535);
        let is_last = offset + chunk_len == raw.len();
        zlib.push(u8::from(is_last));
        let len_u16 = chunk_len as u16;
        zlib.extend_from_slice(&len_u16.to_le_bytes());
        zlib.extend_from_slice(&(!len_u16).to_le_bytes());
        zlib.extend_from_slice(&raw[offset..offset + chunk_len]);
        offset += chunk_len;
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut png = Vec::new();
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(2); // RGB
    ihdr.push(0);
    ihdr.push(0);
    ihdr.push(0);
    write_png_chunk(&mut png, b"IHDR", &ihdr);
    write_png_chunk(&mut png, b"IDAT", &zlib);
    write_png_chunk(&mut png, b"IEND", &[]);
    png
}

fn write_solid_png(path: &Path, width: u32, height: u32, rgb: [u8; 3]) {
    let mut buf = Vec::with_capacity((width as usize) * (height as usize) * 3);
    for _ in 0..(width * height) {
        buf.extend_from_slice(&rgb);
    }
    std::fs::write(path, encode_rgb_png(width, height, &buf)).expect("write png");
}

fn write_receipt_png(path: &Path, width: u32, height: u32) {
    let mut buf = vec![245u8; (width as usize) * (height as usize) * 3];
    for y in (height / 3)..(height / 2) {
        for x in (width / 4)..(width * 3 / 4) {
            if y % 6 < 2 {
                let idx = ((y * width + x) as usize) * 3;
                buf[idx] = 20;
                buf[idx + 1] = 20;
                buf[idx + 2] = 20;
            }
        }
    }
    std::fs::write(path, encode_rgb_png(width, height, &buf)).expect("write receipt png");
}

fn write_two_scene_video_mp4(path: &Path) {
    let ffmpeg = video_capabilities()
        .ffmpeg_path
        .as_deref()
        .expect("ffmpeg must be installed for phase2 video keyframe test");
    // 6s blue followed by 6s warm orange-red so scene-change detection picks t=0s and t>=5s.
    let filter = "color=c=#143cdc:s=160x120:d=6:r=10[c0];color=c=#f05a28:s=160x120:d=6:r=10[c1];[c0][c1]concat=n=2:v=1:a=0[v]";
    let out = Command::new(ffmpeg)
        .args([
            "-v",
            "error",
            "-y",
            "-filter_complex",
            filter,
            "-map",
            "[v]",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(path)
        .output()
        .expect("spawn ffmpeg for multi-scene video");
    assert!(
        out.status.success() && path.is_file(),
        "ffmpeg failed to generate multi-scene video: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test]
async fn fresh_empty_db_search_models_and_people_return_clean_200() {
    let app = TestHarness::new_deterministic();
    let (_admin_id, admin_cookie) = app.initialize_admin().await;

    let (status, _, search_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/search",
            &[&admin_cookie],
            Some(json!({ "query": "sunset" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "search failed: {search_body}");
    assert_eq!(search_body["items"], json!([]));
    assert_eq!(search_body["total_count"], 0);

    let (status, _, models_body) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/admin/ai/models",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "models failed: {models_body}");
    let models = models_body["models"].as_array().expect("models array");
    assert_eq!(models.len(), 12);
    assert!(models.iter().any(|m| m["id"] == "siglip2-large"));

    let (status, _, people_body) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/people",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "people failed: {people_body}");
    assert_eq!(people_body["items"], json!([]));
}

#[tokio::test]
async fn hybrid_search_video_timestamp_and_strict_rbac() {
    let app = TestHarness::new_deterministic();
    let (_admin_id, admin_cookie) = app.initialize_admin().await;

    let folder_a_dir = app._tmp.path().join("folder_a");
    let folder_b_dir = app._tmp.path().join("folder_b");
    let folder_a_id = app
        .create_folder(&admin_cookie, "Folder A", &folder_a_dir)
        .await;
    let folder_b_id = app
        .create_folder(&admin_cookie, "Folder B", &folder_b_dir)
        .await;

    let viewer_cookie = app
        .create_user_and_login(&admin_cookie, "viewer_a", "viewer", &[&folder_a_id])
        .await;

    write_solid_png(&folder_a_dir.join("sunset_a.png"), 96, 96, [240, 90, 40]);
    write_two_scene_video_mp4(&folder_a_dir.join("trip_video_a.mp4"));
    write_solid_png(&folder_b_dir.join("forest_b.png"), 96, 96, [20, 200, 40]);
    write_receipt_png(&folder_b_dir.join("receipt_b.png"), 96, 96);

    app.scan_folder_and_wait(&admin_cookie, &folder_a_id).await;
    app.scan_folder_and_wait(&admin_cookie, &folder_b_id).await;
    app.drain_stage2().await;

    // 1. Admin hybrid search for "sunset" returns photo + video with deep-link timestamp_sec > 0
    let (status, _, admin_sunset) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/search",
            &[&admin_cookie],
            Some(json!({ "query": "sunset", "mode": "hybrid" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "admin sunset: {admin_sunset}");
    assert_eq!(admin_sunset["planner_mode"], "exact_simd");
    let items = admin_sunset["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "expected photo + video hits: {items:?}");

    let video_hit = items
        .iter()
        .find(|hit| hit["asset"]["kind"] == "video")
        .expect("video hit must be returned");
    let ts_sec = video_hit["timestamp_sec"]
        .as_f64()
        .expect("video hit must include timestamp_sec");
    assert!(
        ts_sec >= 5.0,
        "expected sunset scene timestamp >= 5.0s, got {ts_sec}"
    );
    assert!(video_hit["match_reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r.as_str().unwrap_or("").starts_with("Semantic ")));

    // 2. Admin OCR + Hybrid search for "42.00" finds Folder B receipt
    let (status, _, admin_ocr) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/search",
            &[&admin_cookie],
            Some(json!({ "query": "42.00", "mode": "ocr" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "admin ocr: {admin_ocr}");
    assert_eq!(admin_ocr["planner_mode"], "sql_only");
    let ocr_items = admin_ocr["items"].as_array().unwrap();
    assert_eq!(ocr_items.len(), 1);
    assert_eq!(ocr_items[0]["asset"]["folder_id"], folder_b_id);
    assert!(ocr_items[0]["match_reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "OCR"));
    assert!(ocr_items[0]["ocr_snippet"]
        .as_str()
        .unwrap_or("")
        .contains("42.00"));

    // 3. Admin sees Folder B semantic "forest"; Viewer ([Folder A]) gets zero Folder B semantic or OCR leaks!
    let (status, _, admin_forest) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/search",
            &[&admin_cookie],
            Some(json!({ "query": "forest", "mode": "semantic" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(admin_forest["items"].as_array().unwrap().len(), 1);
    assert_eq!(admin_forest["items"][0]["asset"]["folder_id"], folder_b_id);

    let (status, _, viewer_forest) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/search",
            &[&viewer_cookie],
            Some(json!({ "query": "forest", "mode": "semantic" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(viewer_forest["items"], json!([]));

    let (status, _, viewer_ocr) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/search",
            &[&viewer_cookie],
            Some(json!({ "query": "42.00", "mode": "hybrid" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(viewer_ocr["items"], json!([]));

    let (status, _, viewer_sunset) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/search",
            &[&viewer_cookie],
            Some(json!({ "query": "sunset", "mode": "hybrid" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let v_items = viewer_sunset["items"].as_array().unwrap();
    assert_eq!(v_items.len(), 2);
    assert!(v_items
        .iter()
        .all(|hit| hit["asset"]["folder_id"] == folder_a_id));

    // 4. Deterministic SQL filters mode
    let (status, _, filter_res) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/search",
            &[&admin_cookie],
            Some(json!({ "mode": "filters", "kind": "video" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(filter_res["planner_mode"], "sql_only");
    assert_eq!(filter_res["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn zero_downtime_vision_tier_swap() {
    let app = TestHarness::new_deterministic();
    let (_admin_id, admin_cookie) = app.initialize_admin().await;

    let folder_dir = app._tmp.path().join("tier_photos");
    let folder_id = app
        .create_folder(&admin_cookie, "Tier Photos", &folder_dir)
        .await;

    write_solid_png(&folder_dir.join("sunset_1.png"), 96, 96, [240, 85, 35]);
    write_solid_png(&folder_dir.join("sunset_2.png"), 96, 96, [235, 95, 45]);

    app.scan_folder_and_wait(&admin_cookie, &folder_id).await;
    app.drain_stage2().await;

    // Confirm tier2 is active and returns 2 hits
    let (status, _, dash) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/admin/ai/models",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dash["state"]["active_vision_tier"], "tier2");
    assert_eq!(dash["state"]["staging_vision_tier"], Value::Null);
    assert_eq!(dash["queue"]["indexed_assets"], 2);

    // Trigger tier3 swap
    let (status, _, tier_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/admin/ai/tier",
            &[&admin_cookie],
            Some(json!({ "tier": "tier3" })),
        ))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "tier swap: {tier_body}");
    assert_eq!(tier_body["active_vision_tier"], "tier2");
    assert_eq!(tier_body["staging_vision_tier"], "tier3");

    // Process only 1 of 2 assets into staging tier3
    let staged = process_stage2_batch(&app.state, 1)
        .await
        .expect("partial staging batch");
    assert_eq!(staged, 1);

    // Live search MUST still work on tier2 with both assets while tier3 is mid-staging!
    let (status, _, mid_search) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/search",
            &[&admin_cookie],
            Some(json!({ "query": "sunset", "mode": "semantic" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(mid_search["items"].as_array().unwrap().len(), 2);

    let (status, _, mid_dash) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/admin/ai/models",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(mid_dash["state"]["active_vision_tier"], "tier2");
    assert_eq!(mid_dash["state"]["staging_vision_tier"], "tier3");
    assert_eq!(mid_dash["queue"]["staging_remaining"], 1);

    // Finish staging -> automatic cutover to tier3!
    let mut ws_rx = app.state.ws_tx.subscribe();
    let staged2 = process_stage2_batch(&app.state, 10)
        .await
        .expect("finish staging batch");
    assert_eq!(staged2, 1);

    let (status, _, post_dash) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/admin/ai/models",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(post_dash["state"]["active_vision_tier"], "tier3");
    assert_eq!(post_dash["state"]["staging_vision_tier"], Value::Null);
    assert_eq!(post_dash["queue"]["staging_remaining"], 0);
    assert!(app._tmp.path().join("vectors/vision_tier3.f16").is_file());

    // Live search now serves from tier3
    let (status, _, post_search) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/search",
            &[&admin_cookie],
            Some(json!({ "query": "sunset", "mode": "semantic" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(post_search["items"].as_array().unwrap().len(), 2);

    let mut saw_cutover_evt = false;
    while let Ok(evt) = ws_rx.try_recv() {
        if let WsServerEvent::AiStateChanged {
            active_vision_tier,
            staging_vision_tier,
            ..
        } = evt
        {
            if active_vision_tier == truongphoto_core::AiTier::Tier3
                && staging_vision_tier.is_none()
            {
                saw_cutover_evt = true;
            }
        }
    }
    assert!(saw_cutover_evt, "expected AiStateChanged cutover WS event");
}

#[tokio::test]
async fn people_rbac_face_thumbnails_clustering_merge_and_reassign() {
    let app = TestHarness::new_deterministic();
    let (_admin_id, admin_cookie) = app.initialize_admin().await;

    let folder_a_dir = app._tmp.path().join("people_a");
    let folder_b_dir = app._tmp.path().join("people_b");
    let folder_a_id = app
        .create_folder(&admin_cookie, "People A", &folder_a_dir)
        .await;
    let folder_b_id = app
        .create_folder(&admin_cookie, "People B", &folder_b_dir)
        .await;

    let viewer_cookie = app
        .create_user_and_login(&admin_cookie, "viewer_p", "viewer", &[&folder_a_id])
        .await;
    let contrib_cookie = app
        .create_user_and_login(&admin_cookie, "contrib_p", "contributor", &[&folder_a_id])
        .await;

    // Two warm faces in Folder A (same quantized center signature -> same person cluster)
    write_solid_png(&folder_a_dir.join("a1.png"), 96, 96, [230, 80, 40]);
    write_solid_png(&folder_a_dir.join("a2.png"), 96, 96, [232, 82, 42]);
    // Two blue faces in Folder B (different quantized center signature -> second person cluster)
    write_solid_png(&folder_b_dir.join("b1.png"), 96, 96, [30, 80, 230]);
    write_solid_png(&folder_b_dir.join("b2.png"), 96, 96, [32, 82, 232]);

    app.scan_folder_and_wait(&admin_cookie, &folder_a_id).await;
    app.scan_folder_and_wait(&admin_cookie, &folder_b_id).await;
    app.drain_stage2().await;

    // Viewer & Contributor are forbidden from Admin people mutations
    for cookie in [&viewer_cookie, &contrib_cookie] {
        let (status, _, _) = app
            .send(TestHarness::req_json(
                Method::POST,
                "/api/people/cluster",
                &[cookie],
                None,
            ))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    // Admin clusters unassigned faces -> 2 clusters, 4 faces assigned
    let (status, _, cluster_res) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/people/cluster",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "cluster: {cluster_res}");
    assert_eq!(cluster_res["clusters_created"], 2);
    assert_eq!(cluster_res["faces_assigned"], 4);

    // Admin sees both people; Viewer sees ONLY Folder A person
    let (status, _, admin_people) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/people",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let admin_items = admin_people["items"].as_array().unwrap();
    assert_eq!(admin_items.len(), 2);

    let (status, _, viewer_people) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/people",
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let viewer_items = viewer_people["items"].as_array().unwrap();
    assert_eq!(viewer_items.len(), 1);
    assert_eq!(viewer_items[0]["face_count"], 2);

    let person_a_id = viewer_items[0]["id"].as_str().unwrap().to_string();
    let face_a_id = viewer_items[0]["cover_face_id"]
        .as_str()
        .unwrap()
        .to_string();

    let person_b = admin_items
        .iter()
        .find(|p| p["id"].as_str().unwrap() != person_a_id)
        .expect("person B");
    let person_b_id = person_b["id"].as_str().unwrap().to_string();
    let face_b_id = person_b["cover_face_id"].as_str().unwrap().to_string();

    // Viewer gets 200 on Person A detail/assets & face thumb, 404 on Person B detail/assets & face thumb
    let (status, _, p_detail) = app
        .send(TestHarness::req_json(
            Method::GET,
            &format!("/api/people/{person_a_id}"),
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(p_detail["face_count"], 2);

    let (status, _, p_assets) = app
        .send(TestHarness::req_json(
            Method::GET,
            &format!("/api/people/{person_a_id}/assets"),
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(p_assets["items"].as_array().unwrap().len(), 2);

    let (status, _, _) = app
        .send(TestHarness::req_json(
            Method::GET,
            &format!("/api/people/{person_b_id}"),
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _, _) = app
        .send(TestHarness::req_json(
            Method::GET,
            &format!("/api/people/{person_b_id}/assets"),
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, headers, thumb_bytes) = app
        .send_raw(TestHarness::req_json(
            Method::GET,
            &format!("/api/media/faces/{face_a_id}/thumb"),
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get(header::CONTENT_TYPE).unwrap(), "image/webp");
    assert!(thumb_bytes.starts_with(b"RIFF"));

    let (status, _, _) = app
        .send_raw(TestHarness::req_json(
            Method::GET,
            &format!("/api/media/faces/{face_b_id}/thumb"),
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, headers, b_thumb) = app
        .send_raw(TestHarness::req_json(
            Method::GET,
            &format!("/api/media/faces/{face_b_id}/thumb"),
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get(header::CONTENT_TYPE).unwrap(), "image/webp");
    assert!(b_thumb.starts_with(b"RIFF"));

    // RBAC on PATCH /api/people/{id}, reassign, merge
    for cookie in [&viewer_cookie, &contrib_cookie] {
        let (status, _, _) = app
            .send(TestHarness::req_json(
                Method::PATCH,
                &format!("/api/people/{person_a_id}"),
                &[cookie],
                Some(json!({ "name": "Hacked" })),
            ))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, _, _) = app
            .send(TestHarness::req_json(
                Method::POST,
                "/api/people/merge",
                &[cookie],
                Some(json!({
                    "source_person_ids": [person_b_id],
                    "target_person_id": person_a_id
                })),
            ))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, _, _) = app
            .send(TestHarness::req_json(
                Method::POST,
                &format!("/api/people/{person_a_id}/faces/{face_a_id}/reassign"),
                &[cookie],
                Some(json!({ "new_person_name": "Split" })),
            ))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    // Admin renames Person A & hides Person B
    let (status, _, renamed) = app
        .send(TestHarness::req_json(
            Method::PATCH,
            &format!("/api/people/{person_a_id}"),
            &[&admin_cookie],
            Some(json!({ "name": "Alice", "is_pinned": true })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(renamed["name"], "Alice");
    assert_eq!(renamed["is_pinned"], true);

    let (status, _, hidden_b) = app
        .send(TestHarness::req_json(
            Method::PATCH,
            &format!("/api/people/{person_b_id}"),
            &[&admin_cookie],
            Some(json!({ "is_hidden": true })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hidden_b["is_hidden"], true);

    // Admin with ?include_hidden=true sees 2; default sees 1
    let (status, _, admin_vis) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/people",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(admin_vis["items"].as_array().unwrap().len(), 1);

    let (status, _, admin_all) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/people?include_hidden=true",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(admin_all["items"].as_array().unwrap().len(), 2);

    // Admin splits one face from Alice into a new person "Bob", then merges Bob back into Alice
    let (status, _, split_res) = app
        .send(TestHarness::req_json(
            Method::POST,
            &format!("/api/people/{person_a_id}/faces/{face_a_id}/reassign"),
            &[&admin_cookie],
            Some(json!({ "new_person_name": "Bob" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "reassign: {split_res}");
    let bob_id = split_res["person_id"].as_str().unwrap().to_string();
    assert_ne!(bob_id, person_a_id);

    let (status, _, merge_res) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/people/merge",
            &[&admin_cookie],
            Some(json!({
                "source_person_ids": [bob_id],
                "target_person_id": person_a_id
            })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "merge: {merge_res}");
    assert_eq!(merge_res["merged_faces"], 1);
    assert_eq!(merge_res["person"]["face_count"], 2);
}

#[tokio::test]
async fn ai_admin_endpoints_rbac_and_pause_resume_settings() {
    let app = TestHarness::new_deterministic();
    let (_admin_id, admin_cookie) = app.initialize_admin().await;

    let folder_dir = app._tmp.path().join("admin_ai");
    let folder_id = app
        .create_folder(&admin_cookie, "Admin AI", &folder_dir)
        .await;

    let viewer_cookie = app
        .create_user_and_login(&admin_cookie, "viewer_ai", "viewer", &[&folder_id])
        .await;
    let contrib_cookie = app
        .create_user_and_login(&admin_cookie, "contrib_ai", "contributor", &[&folder_id])
        .await;

    // Viewer & Contributor get 403 FORBIDDEN on all /api/admin/ai/* endpoints
    for cookie in [&viewer_cookie, &contrib_cookie] {
        for (method, path, body) in [
            (Method::GET, "/api/admin/ai/models", None),
            (
                Method::POST,
                "/api/admin/ai/models/siglip2-base/download",
                None,
            ),
            (
                Method::POST,
                "/api/admin/ai/models/siglip2-base/cancel",
                None,
            ),
            (Method::DELETE, "/api/admin/ai/models/siglip2-base", None),
            (
                Method::POST,
                "/api/admin/ai/tier",
                Some(json!({ "tier": "tier1" })),
            ),
            (
                Method::POST,
                "/api/admin/ai/reindex",
                Some(json!({ "scope": "all" })),
            ),
            (Method::POST, "/api/admin/ai/pause", None),
            (Method::POST, "/api/admin/ai/resume", None),
            (
                Method::PATCH,
                "/api/admin/ai/settings",
                Some(json!({ "idle_unload_secs": 120 })),
            ),
        ] {
            let (status, _, _) = app
                .send(TestHarness::req_json(method, path, &[cookie], body))
                .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "expected 403 for {path}");
        }
    }

    // Admin pause blocks Stage 2 worker progression; resume restores it
    let (status, _, paused_state) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/admin/ai/pause",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(paused_state["paused"], true);

    write_solid_png(&folder_dir.join("pause_test.png"), 64, 64, [240, 80, 40]);
    app.scan_folder_and_wait(&admin_cookie, &folder_id).await;

    let processed_while_paused = process_stage2_batch(&app.state, 10)
        .await
        .expect("stage2 paused");
    assert_eq!(processed_while_paused, 0);

    let (status, _, resumed_state) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/admin/ai/resume",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resumed_state["paused"], false);

    let processed_after_resume = process_stage2_batch(&app.state, 10)
        .await
        .expect("stage2 resumed");
    assert_eq!(processed_after_resume, 1);

    // Admin AI settings update & reindex
    let (status, _, settings_state) = app
        .send(TestHarness::req_json(
            Method::PATCH,
            "/api/admin/ai/settings",
            &[&admin_cookie],
            Some(json!({ "idle_unload_secs": 120, "video_max_frames": 15 })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings_state["idle_unload_secs"], 120);

    let (status, _, reindex_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/admin/ai/reindex",
            &[&admin_cookie],
            Some(json!({ "scope": "all" })),
        ))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(reindex_body["reset_count"], 1);

    // Delete & unknown model handling
    let (status, _, _) = app
        .send(TestHarness::req_json(
            Method::DELETE,
            "/api/admin/ai/models/siglip2-base",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _, _) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/admin/ai/models/nonexistent-model/download",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
