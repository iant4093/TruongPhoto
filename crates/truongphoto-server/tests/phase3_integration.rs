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
use truongphoto_ai::{
    AiPipeline, ChatRouterBackend, ChatTurnInput, ClassicChatRouter, RerankCandidate, RerankResult,
};
use truongphoto_core::{
    ChatTier, PowerState, RouterPlan, ServerConfig, StoragePaths, WsServerEvent,
};
use truongphoto_db::{rusqlite::params, DbHandle};
use truongphoto_media::video::video_capabilities;
use truongphoto_server::{
    ai::worker::process_stage2_batch, auth, build_router, mdns::MdnsHandle, state::AppState,
};
use uuid::Uuid;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("tp-p3-server-test-{}", Uuid::new_v4()));
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
    maps_dir: PathBuf,
}

impl TestHarness {
    fn new_deterministic() -> Self {
        Self::with_pipeline(AiPipeline::deterministic_test())
    }

    fn with_pipeline(pipeline: AiPipeline) -> Self {
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
            .with_ai_pipeline(pipeline),
        );
        let router = build_router(state.clone());

        Self {
            thumbs_dir: tmp.path().join("thumbnails"),
            cache_dir: tmp.path().join("cache"),
            uploads_dir: tmp.path().join("uploads"),
            maps_dir: tmp.path().join("maps"),
            _tmp: tmp,
            db,
            state,
            router,
        }
    }

    fn loopback() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 51235)
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

    fn req_range(uri: &str, cookies: &[&str], range_val: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method(Method::GET).uri(uri);
        if !cookies.is_empty() {
            builder = builder.header(header::COOKIE, cookies.join("; "));
        }
        if let Some(r) = range_val {
            builder = builder.header(header::RANGE, r);
        }
        let mut req = builder.body(Body::empty()).expect("build range request");
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
        raw.push(0);
        raw.extend_from_slice(&rgb_rows[y * row_len..(y + 1) * row_len]);
    }

    let mut zlib = Vec::with_capacity(raw.len() + 64);
    zlib.extend_from_slice(&[0x78, 0x01]);
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
    ihdr.push(8);
    ihdr.push(2);
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

fn write_two_scene_video_mp4(path: &Path) {
    let ffmpeg = video_capabilities()
        .ffmpeg_path
        .as_deref()
        .expect("ffmpeg must be installed for phase3 video chat test");
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

/// Custom test chat router that forces an unmatched person name into `RouterPlan.person_names`
/// whenever the prompt contains `"force_admin_person"` to verify server-side sentinel protection.
struct SentinelPersonTestRouter(ClassicChatRouter);

impl ChatRouterBackend for SentinelPersonTestRouter {
    fn route_turn(
        &self,
        tier: ChatTier,
        input: &ChatTurnInput,
    ) -> Result<RouterPlan, truongphoto_ai::AiError> {
        let mut plan = self.0.route_turn(tier, input)?;
        if input.user_message.contains("force_admin_person") {
            plan.person_names = vec!["SecretAdminPerson".to_string()];
            plan.semantic_query = None;
        }
        Ok(plan)
    }

    fn rerank_thumbnails(
        &self,
        _tier: ChatTier,
        _user_message: &str,
        _candidates: &[RerankCandidate],
    ) -> Result<Option<RerankResult>, truongphoto_ai::AiError> {
        Ok(None)
    }

    fn evict_idle(&self, _max_idle: Duration) -> usize {
        0
    }

    fn is_loaded(&self) -> bool {
        false
    }
}

#[tokio::test]
async fn multi_turn_chat_rbac_and_session_isolation() {
    let app = TestHarness::new_deterministic();
    let (_admin_id, admin_cookie) = app.initialize_admin().await;

    let folder_a_dir = app._tmp.path().join("chat_folder_a");
    let folder_b_dir = app._tmp.path().join("chat_folder_b");
    let folder_a_id = app
        .create_folder(&admin_cookie, "Folder A", &folder_a_dir)
        .await;
    let folder_b_id = app
        .create_folder(&admin_cookie, "Folder B", &folder_b_dir)
        .await;

    let viewer_cookie = app
        .create_user_and_login(&admin_cookie, "viewer_chat", "viewer", &[&folder_a_id])
        .await;

    // Folder A: sunset photo + multi-scene sunset video
    write_solid_png(&folder_a_dir.join("sunset_a.png"), 96, 96, [240, 90, 40]);
    write_two_scene_video_mp4(&folder_a_dir.join("sunset_video_a.mp4"));

    // Folder B: sunset photo + two blue-signature faces for an Admin-only person "SecretBob"
    write_solid_png(&folder_b_dir.join("sunset_b.png"), 96, 96, [242, 92, 42]);
    write_solid_png(&folder_b_dir.join("bob_b1.png"), 96, 96, [30, 80, 230]);
    write_solid_png(&folder_b_dir.join("bob_b2.png"), 96, 96, [32, 82, 232]);

    app.scan_folder_and_wait(&admin_cookie, &folder_a_id).await;
    app.scan_folder_and_wait(&admin_cookie, &folder_b_id).await;
    app.drain_stage2().await;

    // Cluster faces and name the Folder B person "SecretBob" (never visible to Viewer)
    let (status, _, cluster_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/people/cluster",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "cluster: {cluster_body}");

    let (status, _, admin_people) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/people",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let people_arr = admin_people["items"].as_array().unwrap();
    let (status, _, viewer_people) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/people",
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let viewer_pids: Vec<&str> = viewer_people["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["id"].as_str())
        .collect();

    let bob_person = people_arr
        .iter()
        .find(|p| !viewer_pids.contains(&p["id"].as_str().unwrap()))
        .expect("admin-only person in Folder B");
    let bob_id = bob_person["id"].as_str().unwrap();

    let (status, _, renamed) = app
        .send(TestHarness::req_json(
            Method::PATCH,
            &format!("/api/people/{bob_id}"),
            &[&admin_cookie],
            Some(json!({ "name": "SecretBob" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "rename SecretBob: {renamed}");

    // 1. Admin creates a session and searches across Folder A + Folder B
    let (status, _, admin_sess_resp) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/chat/sessions",
            &[&admin_cookie],
            Some(json!({ "title": "Admin Sunset" })),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let admin_sid = admin_sess_resp["session"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _, admin_turn) = app
        .send(TestHarness::req_json(
            Method::POST,
            &format!("/api/chat/sessions/{admin_sid}/messages"),
            &[&admin_cookie],
            Some(json!({ "content": "sunset" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "admin turn: {admin_turn}");
    let admin_hits = admin_turn["hits"].as_array().unwrap();
    assert!(
        admin_hits
            .iter()
            .any(|h| h["asset"]["folder_id"] == folder_a_id),
        "admin must see Folder A sunset hits"
    );
    assert!(
        admin_hits
            .iter()
            .any(|h| h["asset"]["folder_id"] == folder_b_id),
        "admin must see Folder B sunset hits"
    );

    // Admin queries SecretBob -> finds Bob's Folder B photos
    let (status, _, admin_bob_turn) = app
        .send(TestHarness::req_json(
            Method::POST,
            &format!("/api/chat/sessions/{admin_sid}/messages"),
            &[&admin_cookie],
            Some(json!({ "content": "start over with SecretBob" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let bob_hits = admin_bob_turn["hits"].as_array().unwrap();
    assert_eq!(bob_hits.len(), 2);
    assert!(bob_hits
        .iter()
        .all(|h| h["asset"]["folder_id"] == folder_b_id));

    // 2. Viewer session isolation: Viewer CANNOT see, read, message, or delete Admin's session (404)
    let (status, _, _) = app
        .send(TestHarness::req_json(
            Method::GET,
            &format!("/api/chat/sessions/{admin_sid}"),
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _, _) = app
        .send(TestHarness::req_json(
            Method::POST,
            &format!("/api/chat/sessions/{admin_sid}/messages"),
            &[&viewer_cookie],
            Some(json!({ "content": "sunset" })),
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _, _) = app
        .send(TestHarness::req_json(
            Method::DELETE,
            &format!("/api/chat/sessions/{admin_sid}"),
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 3. Viewer creates own session & lists sessions (only sees own)
    let (status, _, v_sess_resp) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/chat/sessions",
            &[&viewer_cookie],
            Some(json!({})),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let viewer_sid = v_sess_resp["session"]["id"].as_str().unwrap().to_string();
    assert_eq!(v_sess_resp["session"]["title"], "New Chat");

    let (status, _, v_list) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/chat/sessions",
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let v_sessions = v_list["items"].as_array().unwrap();
    assert_eq!(v_sessions.len(), 1);
    assert_eq!(v_sessions[0]["id"], viewer_sid);

    // 4. Viewer Turn 1: "sunset" -> ONLY Folder A hits (2 items), zero Folder B leaks
    let (status, _, v_turn1) = app
        .send(TestHarness::req_json(
            Method::POST,
            &format!("/api/chat/sessions/{viewer_sid}/messages"),
            &[&viewer_cookie],
            Some(json!({ "content": "sunset" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "viewer turn1: {v_turn1}");
    let v_hits1 = v_turn1["hits"].as_array().unwrap();
    assert_eq!(v_hits1.len(), 2);
    assert!(v_hits1
        .iter()
        .all(|h| h["asset"]["folder_id"] == folder_a_id));

    // 5. Viewer Turn 2: "only videos" -> carries forward semantic_query="sunset" + kind="video"
    let (status, _, v_turn2) = app
        .send(TestHarness::req_json(
            Method::POST,
            &format!("/api/chat/sessions/{viewer_sid}/messages"),
            &[&viewer_cookie],
            Some(json!({ "content": "only videos" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "viewer turn2: {v_turn2}");
    assert_eq!(v_turn2["active_filters"]["semantic_query"], "sunset");
    assert_eq!(v_turn2["active_filters"]["kind"], "video");
    let v_hits2 = v_turn2["hits"].as_array().unwrap();
    assert_eq!(v_hits2.len(), 1);
    assert_eq!(v_hits2[0]["asset"]["kind"], "video");
    assert_eq!(v_hits2[0]["asset"]["folder_id"], folder_a_id);
    assert!(v_hits2[0]["timestamp_sec"].as_f64().unwrap() >= 5.0);

    // 6. Viewer Turn 3: asks "start over with SecretBob" -> zero Folder B leaks & zero matches
    let (status, _, v_turn3) = app
        .send(TestHarness::req_json(
            Method::POST,
            &format!("/api/chat/sessions/{viewer_sid}/messages"),
            &[&viewer_cookie],
            Some(json!({ "content": "start over with SecretBob" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "viewer turn3: {v_turn3}");
    let v_hits3 = v_turn3["hits"].as_array().unwrap();
    assert!(
        v_hits3
            .iter()
            .all(|h| h["asset"]["folder_id"] == folder_a_id),
        "Folder B must never leak to viewer"
    );
    assert_eq!(
        v_hits3.len(),
        0,
        "Admin-only person SecretBob must yield zero hits for Viewer"
    );

    // 7. Hydrated session detail shows chronological turns and auto-updated title
    let (status, _, v_detail) = app
        .send(TestHarness::req_json(
            Method::GET,
            &format!("/api/chat/sessions/{viewer_sid}"),
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v_detail["session"]["title"], "sunset");
    let msgs = v_detail["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 6);
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[1]["hits"].as_array().unwrap().len(), 2);

    // 8. Delete own session -> 204, subsequent GET -> 404
    let (status, _, _) = app
        .send(TestHarness::req_json(
            Method::DELETE,
            &format!("/api/chat/sessions/{viewer_sid}"),
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _, _) = app
        .send(TestHarness::req_json(
            Method::GET,
            &format!("/api/chat/sessions/{viewer_sid}"),
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unmatched_person_sentinel_prevents_leaks_even_if_router_emits_admin_person() {
    let pipeline = AiPipeline::deterministic_test()
        .with_chat(Arc::new(SentinelPersonTestRouter(ClassicChatRouter::new())));
    let app = TestHarness::with_pipeline(pipeline);
    let (_admin_id, admin_cookie) = app.initialize_admin().await;

    let folder_a_dir = app._tmp.path().join("sentinel_a");
    let folder_a_id = app
        .create_folder(&admin_cookie, "Sentinel A", &folder_a_dir)
        .await;
    let viewer_cookie = app
        .create_user_and_login(&admin_cookie, "viewer_sent", "viewer", &[&folder_a_id])
        .await;

    write_solid_png(&folder_a_dir.join("sunset_a.png"), 96, 96, [240, 90, 40]);
    app.scan_folder_and_wait(&admin_cookie, &folder_a_id).await;
    app.drain_stage2().await;

    let (status, _, sess) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/chat/sessions",
            &[&viewer_cookie],
            Some(json!({ "title": "Sentinel" })),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let sid = sess["session"]["id"].as_str().unwrap();

    let (status, _, msg_resp) = app
        .send(TestHarness::req_json(
            Method::POST,
            &format!("/api/chat/sessions/{sid}/messages"),
            &[&viewer_cookie],
            Some(json!({ "content": "force_admin_person" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "sentinel turn: {msg_resp}");
    assert_eq!(msg_resp["hits"], json!([]));
    assert_eq!(msg_resp["total_count"], 0);
}

#[tokio::test]
async fn chat_tier_hot_swap_rbac_and_tier3_vlm_rerank() {
    let app = TestHarness::new_deterministic();
    let (_admin_id, admin_cookie) = app.initialize_admin().await;

    let folder_dir = app._tmp.path().join("rerank_folder");
    let folder_id = app
        .create_folder(&admin_cookie, "Rerank Folder", &folder_dir)
        .await;

    let viewer_cookie = app
        .create_user_and_login(&admin_cookie, "viewer_rr", "viewer", &[&folder_id])
        .await;
    let contrib_cookie = app
        .create_user_and_login(&admin_cookie, "contrib_rr", "contributor", &[&folder_id])
        .await;

    write_solid_png(&folder_dir.join("sunset_1.png"), 96, 96, [240, 90, 40]);
    write_solid_png(&folder_dir.join("sunset_2.png"), 96, 96, [238, 88, 38]);
    app.scan_folder_and_wait(&admin_cookie, &folder_id).await;
    app.drain_stage2().await;

    // Locate both asset IDs and set a title override on one so VLM rerank deterministically boosts it
    let (status, _, assets_body) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/assets",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let asset_items = assets_body["items"].as_array().unwrap();
    assert_eq!(asset_items.len(), 2);
    let boosted_id = asset_items[1]["id"].as_str().unwrap().to_string();

    let (status, _, _) = app
        .send(TestHarness::req_json(
            Method::PATCH,
            &format!("/api/assets/{boosted_id}/overrides"),
            &[&admin_cookie],
            Some(json!({ "title": "Pier Harbor" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);

    // Pre-generate 400px thumbnails so Tier 3 reranker reads real WebP bytes from disk
    for item in asset_items {
        let aid = item["id"].as_str().unwrap();
        let (status, headers, _) = app
            .send_raw(TestHarness::req_json(
                Method::GET,
                &format!("/api/media/{aid}/thumb"),
                &[&admin_cookie],
                None,
            ))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers.get(header::CONTENT_TYPE).unwrap(), "image/webp");
    }

    // Viewer & Contributor get 403 FORBIDDEN on POST /api/admin/ai/chat-tier
    for cookie in [&viewer_cookie, &contrib_cookie] {
        let (status, _, _) = app
            .send(TestHarness::req_json(
                Method::POST,
                "/api/admin/ai/chat-tier",
                &[cookie],
                Some(json!({ "tier": "tier3", "vlm_rerank_enabled": true })),
            ))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    // Admin hot-swaps chat tier to tier3 + vlm_rerank_enabled = true
    let mut ws_rx = app.state.ws_tx.subscribe();
    let (status, _, tier_resp) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/admin/ai/chat-tier",
            &[&admin_cookie],
            Some(json!({ "tier": "tier3", "vlm_rerank_enabled": true })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "chat-tier swap: {tier_resp}");
    assert_eq!(tier_resp["active_chat_tier"], "tier3");
    assert_eq!(tier_resp["vlm_rerank_enabled"], true);

    // Zero re-indexing triggered
    let (status, _, dash) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/admin/ai/models",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dash["state"]["active_chat_tier"], "tier3");
    assert_eq!(dash["state"]["vlm_rerank_enabled"], true);
    assert_eq!(dash["queue"]["pending_assets"], 0);
    assert_eq!(dash["queue"]["indexed_assets"], 2);

    let mut saw_ws = false;
    while let Ok(evt) = ws_rx.try_recv() {
        if matches!(evt, WsServerEvent::AiStateChanged { .. }) {
            saw_ws = true;
        }
    }
    assert!(saw_ws, "expected WsServerEvent::AiStateChanged");

    // Next chat turn runs Tier 3 VLM reranking and returns reranked: true
    let (status, _, sess_resp) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/chat/sessions",
            &[&admin_cookie],
            Some(json!({ "title": "Rerank Test" })),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let sid = sess_resp["session"]["id"].as_str().unwrap();

    let (status, _, chat_resp) = app
        .send(TestHarness::req_json(
            Method::POST,
            &format!("/api/chat/sessions/{sid}/messages"),
            &[&admin_cookie],
            Some(json!({ "content": "sunset pier harbor" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "rerank chat: {chat_resp}");
    assert_eq!(chat_resp["reranked"], true);
    assert_eq!(chat_resp["assistant_message"]["reranked"], true);
    let hits = chat_resp["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0]["asset"]["id"], boosted_id);
    assert!(hits[0]["match_reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "VLM Reranked"));
    assert!(chat_resp["assistant_message"]["content"]
        .as_str()
        .unwrap()
        .contains("VLM reranked"));

    // Hot-swap back to tier0 -> next turn sets reranked: false
    let (status, _, tier0_resp) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/admin/ai/chat-tier",
            &[&admin_cookie],
            Some(json!({ "tier": "tier0", "vlm_rerank_enabled": false })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tier0_resp["active_chat_tier"], "tier0");
    assert_eq!(tier0_resp["vlm_rerank_enabled"], false);

    let (status, _, chat_resp2) = app
        .send(TestHarness::req_json(
            Method::POST,
            &format!("/api/chat/sessions/{sid}/messages"),
            &[&admin_cookie],
            Some(json!({ "content": "start over sunset" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(chat_resp2["reranked"], false);
}

#[tokio::test]
async fn map_clustering_rbac_config_and_pmtiles_range_security() {
    let app = TestHarness::new_deterministic();
    let (_admin_id, admin_cookie) = app.initialize_admin().await;

    let folder_a_dir = app._tmp.path().join("map_a");
    let folder_b_dir = app._tmp.path().join("map_b");
    let folder_a_id = app
        .create_folder(&admin_cookie, "Map Folder A", &folder_a_dir)
        .await;
    let folder_b_id = app
        .create_folder(&admin_cookie, "Map Folder B", &folder_b_dir)
        .await;

    let viewer_cookie = app
        .create_user_and_login(&admin_cookie, "viewer_map", "viewer", &[&folder_a_id])
        .await;

    write_solid_png(&folder_a_dir.join("tokyo1.png"), 64, 64, [200, 100, 50]);
    write_solid_png(&folder_a_dir.join("tokyo2.png"), 64, 64, [210, 110, 60]);
    write_solid_png(&folder_b_dir.join("paris1.png"), 64, 64, [50, 100, 200]);

    app.scan_folder_and_wait(&admin_cookie, &folder_a_id).await;
    app.scan_folder_and_wait(&admin_cookie, &folder_b_id).await;

    // Assign GPS coordinates via overrides (auto-syncs asset_geo_index)
    let (status, _, all_assets) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/assets",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let items = all_assets["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);

    for item in items {
        let id = item["id"].as_str().unwrap();
        let fid = item["folder_id"].as_str().unwrap();
        let patch = if fid == folder_a_id {
            json!({
                "gps_lat": 35.6762,
                "gps_lon": 139.6503,
                "geo_city": "Tokyo",
                "geo_country": "Japan"
            })
        } else {
            json!({
                "gps_lat": 48.8566,
                "gps_lon": 2.3522,
                "geo_city": "Paris",
                "geo_country": "France"
            })
        };
        let (status, _, _) = app
            .send(TestHarness::req_json(
                Method::PATCH,
                &format!("/api/assets/{id}/overrides"),
                &[&admin_cookie],
                Some(patch),
            ))
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    // 1. Clusters & points enforce strict RBAC across Folder A vs Folder B
    let (status, _, admin_clusters) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/map/clusters?min_lat=-90&max_lat=90&min_lng=-180&max_lng=180&zoom=2",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "admin clusters: {admin_clusters}");
    assert_eq!(admin_clusters["total_in_view"], 3);
    assert_eq!(admin_clusters["clusters"].as_array().unwrap().len(), 2);

    let (status, _, viewer_clusters) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/map/clusters?min_lat=-90&max_lat=90&min_lng=-180&max_lng=180&zoom=2",
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(viewer_clusters["total_in_view"], 2);
    let v_clusters = viewer_clusters["clusters"].as_array().unwrap();
    assert_eq!(v_clusters.len(), 1);
    assert_eq!(v_clusters[0]["label"], "Tokyo");
    assert_eq!(v_clusters[0]["count"], 2);

    let (status, _, admin_points) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/map/points?min_lat=-90&max_lat=90&min_lng=-180&max_lng=180&limit=50",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(admin_points["total_count"], 3);
    assert_eq!(admin_points["items"].as_array().unwrap().len(), 3);

    let (status, _, viewer_points) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/map/points?min_lat=-90&max_lat=90&min_lng=-180&max_lng=180&limit=50",
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(viewer_points["total_count"], 2);
    let v_pts = viewer_points["items"].as_array().unwrap();
    assert_eq!(v_pts.len(), 2);
    assert!(v_pts.iter().all(|p| p["folder_id"] == folder_a_id));

    // 2. Synthetic PMTiles archive + 206 Partial Content Range + Config
    let synthetic: Vec<u8> = (0..512u32).map(|i| ((i * 31 + 7) % 256) as u8).collect();
    std::fs::create_dir_all(&app.maps_dir).expect("create maps dir");
    std::fs::write(app.maps_dir.join("world.pmtiles"), &synthetic).expect("write world.pmtiles");

    let (status, _, admin_cfg) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/map/config",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "map config: {admin_cfg}");
    assert_eq!(admin_cfg["tile_mode"], "offline_pmtiles");
    assert_eq!(admin_cfg["available_pmtiles"], json!(["world.pmtiles"]));
    assert_eq!(admin_cfg["pmtiles_url"], "/api/map/tiles/world.pmtiles");
    assert_eq!(
        admin_cfg["online_style_url"],
        "https://tiles.openfreemap.org/styles/dark"
    );
    assert_eq!(admin_cfg["total_geotagged"], 3);

    let (status, _, viewer_cfg) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/map/config",
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(viewer_cfg["total_geotagged"], 2);

    // Admin toggles map_tile_mode to online_openfreemap and back
    let (status, _, _) = app
        .send(TestHarness::req_json(
            Method::PATCH,
            "/api/admin/settings",
            &[&admin_cookie],
            Some(json!({ "map_tile_mode": "online_openfreemap" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, online_cfg) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/map/config",
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(online_cfg["tile_mode"], "online_openfreemap");

    let (status, _, _) = app
        .send(TestHarness::req_json(
            Method::PATCH,
            "/api/admin/settings",
            &[&admin_cookie],
            Some(json!({ "map_tile_mode": "offline_pmtiles" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, offline_cfg) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/map/config",
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(offline_cfg["tile_mode"], "offline_pmtiles");

    // 3. PMTiles 206 Partial Content Range streaming & 200 full fetch
    let (status, headers, range_bytes) = app
        .send_raw(TestHarness::req_range(
            "/api/map/tiles/world.pmtiles",
            &[&viewer_cookie],
            Some("bytes=0-127"),
        ))
        .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers.get(header::ACCEPT_RANGES).unwrap(), "bytes");
    assert_eq!(
        headers.get(header::CONTENT_RANGE).unwrap(),
        "bytes 0-127/512"
    );
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "application/octet-stream"
    );
    assert_eq!(range_bytes.len(), 128);
    assert_eq!(range_bytes, synthetic[0..128]);

    let (status, headers, full_bytes) = app
        .send_raw(TestHarness::req_range(
            "/api/map/tiles/world.pmtiles",
            &[&viewer_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get(header::ACCEPT_RANGES).unwrap(), "bytes");
    assert_eq!(full_bytes, synthetic);

    // 4. Path traversal & invalid extension rejection (400 / 404)
    for bad_path in [
        "/api/map/tiles/..%2F..%2Flibrary.db",
        "/api/map/tiles/bad.txt",
        "/api/map/tiles/..%5C..%5Clibrary.pmtiles",
        "/api/map/tiles/nonexistent.pmtiles",
    ] {
        let (status, _, _) = app
            .send_raw(TestHarness::req_range(bad_path, &[&viewer_cookie], None))
            .await;
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
            "expected 400 or 404 for {bad_path}, got {status}"
        );
    }

    // 5. Unauthenticated access rejected with 401
    for unauth_uri in [
        "/api/chat/sessions",
        "/api/map/config",
        "/api/map/clusters?min_lat=-90&max_lat=90&min_lng=-180&max_lng=180&zoom=2",
        "/api/map/points?min_lat=-90&max_lat=90&min_lng=-180&max_lng=180",
        "/api/map/tiles/world.pmtiles",
    ] {
        let (status, _, _) = app
            .send(TestHarness::req_json(Method::GET, unauth_uri, &[], None))
            .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "expected 401 for {unauth_uri}"
        );
    }
}

#[tokio::test]
#[ignore = "live real-model AI eval against TP_EVAL_DATA_DIR"]
async fn live_vague_ai_eval() {
    let eval_dir = PathBuf::from(
        std::env::var("TP_EVAL_DATA_DIR")
            .expect("set TP_EVAL_DATA_DIR to a cloned scratch data dir"),
    );
    let mut config = ServerConfig::from_data_dir(eval_dir.clone());
    let bootstrap_secret = "eval-bootstrap-secret-1234567890".to_string();
    config.desktop_bootstrap_token = Some(bootstrap_secret.clone());

    let db = DbHandle::open(&config.db_path).expect("open cloned eval db");
    let gate_key = auth::generate_process_key().expect("gate key");
    let (ws_tx, _) = tokio::sync::broadcast::channel::<WsServerEvent>(64);
    let (_power_tx, power_rx) = watch::channel(PowerState::default());
    let mdns = MdnsHandle::start(config.mdns_hostname.clone(), 38435, false);
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
        38435,
        shutdown_token,
    ));
    let router = build_router(state.clone());

    let send = |req: Request<Body>| {
        let router = router.clone();
        async move {
            let resp = router.oneshot(req).await.expect("oneshot");
            let status = resp.status();
            let headers = resp.headers().clone();
            let bytes = resp.into_body().collect().await.expect("body").to_bytes();
            let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            (status, headers, json)
        }
    };

    // Authenticate cleanly via official desktop bootstrap on loopback.
    let (status, headers, _) = send(TestHarness::req_json(
        Method::POST,
        "/api/auth/desktop-bootstrap",
        &[],
        Some(json!({ "token": bootstrap_secret })),
    ))
    .await;
    assert_eq!(status, StatusCode::OK, "desktop bootstrap failed");
    let cookie = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|s| s.split(';').next().filter(|p| p.starts_with("tp_session=")))
        .expect("tp_session cookie")
        .to_string();

    let cases: &[(&str, &str, i64, i64, &[&str])] = &[
        (
            "who are in my pics",
            "answer",
            0,
            0,
            &["sebastian", "aj", "nate", "carson", "colin", "david"],
        ),
        (
            "who is in my photos?",
            "answer",
            0,
            0,
            &["sebastian", "aj", "nate"],
        ),
        (
            "what's in my library?",
            "answer",
            0,
            0,
            &["453", "zoo", "grad", "canon"],
        ),
        (
            "tell me about my photos",
            "answer",
            0,
            0,
            &["453", "canon", "zoo", "grad"],
        ),
        (
            "what trips or folders do I have?",
            "answer",
            0,
            0,
            &["zoo", "grad", "bald", "multnomah", "helens"],
        ),
        ("how many photos do I have?", "answer", 0, 0, &["453"]),
        (
            "what gear do I use?",
            "answer",
            0,
            0,
            &["canon", "r7", "17-40", "100-400"],
        ),
        ("hi", "answer", 0, 0, &[]),
        ("show me cool pics", "search", 10, 500, &[]),
        ("show me my best photos", "search", 10, 500, &[]),
        ("surprise me", "search", 10, 500, &[]),
        ("what did I shoot recently?", "search", 10, 500, &[]),
        ("show me people", "search", 10, 500, &[]),
        ("show me my friends", "search", 10, 500, &[]),
        ("show me nature", "search", 1, 500, &[]),
        ("show me outdoors", "search", 1, 500, &[]),
        ("show me animals", "search", 1, 500, &[]),
        ("show me cool mountain pics", "search", 1, 500, &[]),
        ("show me graduation stuff", "search", 1, 500, &[]),
        ("show me pics at 12mm", "search", 1, 500, &[]),
        ("photos of a dinosaur", "search", 0, 0, &[]),
    ];

    let mut passed = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for &(prompt, exp_intent, min_hits, max_hits, mention_any) in cases {
        let (st_c, _, sess_body) = send(TestHarness::req_json(
            Method::POST,
            "/api/chat/sessions",
            &[&cookie],
            Some(json!({ "title": "Eval" })),
        ))
        .await;
        assert_eq!(st_c, StatusCode::CREATED);
        let sid = sess_body["session"]["id"].as_str().unwrap();

        let (st_m, _, resp) = send(TestHarness::req_json(
            Method::POST,
            &format!("/api/chat/sessions/{sid}/messages"),
            &[&cookie],
            Some(json!({ "content": prompt })),
        ))
        .await;
        assert_eq!(st_m, StatusCode::OK, "chat failed for {prompt}: {resp}");

        let plan = &resp["active_filters"];
        let hits = resp["total_count"].as_i64().unwrap_or(-1);
        let reply = resp["assistant_message"]["content"].as_str().unwrap_or("");
        let intent = plan["intent"].as_str().unwrap_or("search");
        let ms = resp["elapsed_ms"].as_u64().unwrap_or(0);

        let mut errs: Vec<String> = Vec::new();
        if intent != exp_intent {
            errs.push(format!("intent={intent} expected={exp_intent}"));
        }
        if hits < min_hits || hits > max_hits {
            errs.push(format!("hits={hits} expected=[{min_hits}..={max_hits}]"));
        }
        if !mention_any.is_empty() {
            let low = reply.to_ascii_lowercase();
            if !mention_any.iter().any(|m| low.contains(m)) {
                errs.push(format!("reply missing any of {mention_any:?}"));
            }
        }
        if exp_intent == "answer"
            && (!plan["semantic_query"].is_null()
                || !plan["person_names"].as_array().is_none_or(|a| a.is_empty())
                || !plan["location_query"].is_null()
                || !plan["has_people"].is_null()
                || !plan["sort_by"].is_null())
        {
            errs.push(format!("answer turn leaked filters: {plan}"));
        }

        let tag = if errs.is_empty() {
            passed += 1;
            "PASS"
        } else {
            failures.push(format!("{prompt} => {}", errs.join("; ")));
            "FAIL"
        };
        println!(
            "[{tag}] ({ms}ms) Q={prompt:?} intent={intent} hits={hits} sem={:?} loc={:?} sort={:?} people={:?} has_p={:?}",
            plan["semantic_query"].as_str(),
            plan["location_query"].as_str(),
            plan["sort_by"].as_str(),
            plan["person_names"],
            plan["has_people"],
        );
        println!("       reply: {reply}");
        if !errs.is_empty() {
            println!("       ERRS:  {}", errs.join("; "));
        }
    }

    println!("\nLIVE EVAL SUMMARY: {passed}/{} passed", cases.len());
    assert!(
        failures.is_empty(),
        "Live vague AI eval failures:\n{}",
        failures.join("\n")
    );
}
