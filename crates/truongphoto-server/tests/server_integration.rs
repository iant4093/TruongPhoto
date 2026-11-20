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
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tower::ServiceExt;
use truongphoto_core::{PowerState, ServerConfig, WsServerEvent};
use truongphoto_db::{rusqlite::params, DbHandle};
use truongphoto_server::{auth, build_router, mdns::MdnsHandle, start_server, state::AppState};
use uuid::Uuid;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("tp-server-test-{}", Uuid::new_v4()));
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
    router: Router,
}

impl TestHarness {
    fn new(desktop_secret: Option<&str>) -> Self {
        let tmp = TempDir::new().expect("create temp dir");
        let mut config = ServerConfig::from_data_dir(tmp.path().to_path_buf());
        config.mdns_enabled = false;
        config.desktop_bootstrap_token = desktop_secret.map(str::to_string);

        let default_paths = truongphoto_core::StoragePaths::from_data_dir(config.data_dir.clone());
        default_paths.ensure_dirs().expect("ensure dirs");

        let db = DbHandle::open(&config.db_path).expect("open test db");
        let gate_key = auth::generate_process_key().expect("gate key");
        let (ws_tx, _) = tokio::sync::broadcast::channel::<WsServerEvent>(64);
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
            _tmp: tmp,
            db,
            router,
        }
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

    fn loopback_addr() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 51234)
    }

    fn lan_addr(octet: u8) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, octet)), 51234)
    }

    fn req_json(
        method: Method,
        uri: &str,
        peer: SocketAddr,
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
        req.extensions_mut().insert(ConnectInfo(peer));
        req
    }

    async fn initialize_admin(&self) -> (String, String) {
        let uploads = self._tmp.path().join("uploads");
        let thumbs = self._tmp.path().join("thumbnails");
        let cache = self._tmp.path().join("cache");
        let backups = self._tmp.path().join("backups");

        let req = Self::req_json(
            Method::POST,
            "/api/setup/initialize",
            Self::loopback_addr(),
            &[],
            &[],
            Some(json!({
                "admin_username": "ian",
                "admin_display_name": "Ian",
                "admin_profile_pin": "1234",
                "server_pin": "123456",
                "managed_uploads_dir": uploads.to_string_lossy(),
                "thumbnails_dir": thumbs.to_string_lossy(),
                "cache_dir": cache.to_string_lossy(),
                "backup_dir": backups.to_string_lossy(),
                "initial_folder_path": null,
                "initial_folder_name": null
            })),
        );
        let (status, headers, body) = self.send(req).await;
        assert_eq!(status, StatusCode::CREATED, "setup init failed: {body}");
        let session_cookie = extract_cookie_pair(&headers, "tp_session")
            .expect("tp_session cookie should be set on setup");
        let user_id = body["user"]["id"]
            .as_str()
            .expect("admin user id")
            .to_string();
        (user_id, session_cookie)
    }
}

fn find_set_cookie<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|s| s.starts_with(&format!("{name}=")))
}

fn extract_cookie_pair(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    let raw = find_set_cookie(headers, name)?;
    let pair = raw.split(';').next()?.trim();
    if pair.ends_with('=') {
        None
    } else {
        Some(pair.to_string())
    }
}

#[tokio::test]
async fn health_setup_and_409_reinitialize() {
    let app = TestHarness::new(None);

    // 1. GET /api/health returns 200, initialized: false
    let (status, _, health) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/health",
            TestHarness::loopback_addr(),
            &[],
            &[],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["status"], "ok");
    assert_eq!(health["initialized"], false);

    // Non-direct loopback setup attempt before initialization -> 403 FORBIDDEN
    let (status, _, err_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/setup/initialize",
            TestHarness::lan_addr(42),
            &[],
            &[],
            Some(json!({
                "admin_username": "ian",
                "admin_display_name": "Ian",
                "admin_profile_pin": "1234",
                "server_pin": "123456",
                "managed_uploads_dir": app._tmp.path().join("u").to_string_lossy(),
                "thumbnails_dir": app._tmp.path().join("t").to_string_lossy(),
                "cache_dir": app._tmp.path().join("c").to_string_lossy(),
                "backup_dir": app._tmp.path().join("b").to_string_lossy(),
            })),
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(err_body["error"]["code"], "FORBIDDEN");

    // 2. Direct loopback setup succeeds -> 201 + tp_session cookie
    let (_admin_id, _cookie) = app.initialize_admin().await;

    let (status, _, health_after) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/health",
            TestHarness::loopback_addr(),
            &[],
            &[],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health_after["initialized"], true);

    // Repeat setup call -> 409 ALREADY_INITIALIZED
    let (status, _, repeat_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/setup/initialize",
            TestHarness::loopback_addr(),
            &[],
            &[],
            Some(json!({
                "admin_username": "ian2",
                "admin_display_name": "Ian2",
                "admin_profile_pin": "1234",
                "server_pin": "123456",
                "managed_uploads_dir": app._tmp.path().join("u2").to_string_lossy(),
                "thumbnails_dir": app._tmp.path().join("t2").to_string_lossy(),
                "cache_dir": app._tmp.path().join("c2").to_string_lossy(),
                "backup_dir": app._tmp.path().join("b2").to_string_lossy(),
            })),
        ))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(repeat_body["error"]["code"], "ALREADY_INITIALIZED");

    app.db.shutdown().await.expect("db shutdown");
}

#[tokio::test]
async fn unauthenticated_folders_and_ws_return_401() {
    let app = TestHarness::new(None);
    let _ = app.initialize_admin().await;

    let (status, _, body) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/folders",
            TestHarness::loopback_addr(),
            &[],
            &[],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");

    let (status, _, ws_body) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/ws",
            TestHarness::loopback_addr(),
            &[],
            &[],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(ws_body["error"]["code"], "UNAUTHORIZED");

    app.db.shutdown().await.expect("db shutdown");
}

#[tokio::test]
async fn brute_force_rate_limiting_and_tailscale_isolation() {
    let app = TestHarness::new(None);
    let _ = app.initialize_admin().await;

    // 5 bad attempts for alice@ts.net -> 401 each
    for attempt in 1..=5 {
        let (status, _, body) = app
            .send(TestHarness::req_json(
                Method::POST,
                "/api/auth/server-pin",
                TestHarness::loopback_addr(),
                &[],
                &[("Tailscale-User-Login", "alice@ts.net")],
                Some(json!({ "server_pin": "000000" })),
            ))
            .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "attempt {attempt} should be 401: {body}"
        );
    }

    // 6th attempt for alice@ts.net -> 429 with Retry-After header
    let (status, headers, body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/auth/server-pin",
            TestHarness::loopback_addr(),
            &[],
            &[("Tailscale-User-Login", "alice@ts.net")],
            Some(json!({ "server_pin": "000000" })),
        ))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["code"], "RATE_LIMITED");
    let retry_after = headers
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .expect("Retry-After header on 429");
    assert!(retry_after >= 1);

    // Distinct Tailscale-User-Login on loopback does NOT share Alice's locked bucket!
    let (status, _, bob_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/auth/server-pin",
            TestHarness::loopback_addr(),
            &[],
            &[("Tailscale-User-Login", "bob@ts.net")],
            Some(json!({ "server_pin": "123456" })),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "bob should not be rate limited: {bob_body}"
    );
    assert_eq!(bob_body["profiles"].as_array().unwrap().len(), 1);

    app.db.shutdown().await.expect("db shutdown");
}

#[tokio::test]
async fn two_step_login_and_dynamic_secure_cookie_flags() {
    let app = TestHarness::new(None);
    let (admin_id, _) = app.initialize_admin().await;

    // Step 0: auth/status without cookies hides profiles
    let (status, _, auth_status) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/auth/status",
            TestHarness::loopback_addr(),
            &[],
            &[],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(auth_status["initialized"], true);
    assert_eq!(auth_status["authenticated"], false);
    assert_eq!(auth_status["server_gate_passed"], false);
    assert!(auth_status["profiles"].is_null());

    // Step 1 on HTTP loopback -> tp_gate without Secure flag
    let (status, headers, gate_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/auth/server-pin",
            TestHarness::loopback_addr(),
            &[],
            &[],
            Some(json!({ "server_pin": "123456" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(gate_body["profiles"][0]["id"], admin_id);
    let gate_set = find_set_cookie(&headers, "tp_gate").expect("tp_gate Set-Cookie");
    assert!(gate_set.contains("HttpOnly"));
    assert!(gate_set.contains("SameSite=Strict"));
    assert!(!gate_set.contains("Secure"));
    let gate_cookie = extract_cookie_pair(&headers, "tp_gate").unwrap();

    // Step 2 on HTTP loopback -> tp_session HttpOnly; SameSite=Strict; NO Secure
    let (status, headers, login_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/auth/login",
            TestHarness::loopback_addr(),
            &[&gate_cookie],
            &[],
            Some(json!({
                "user_id": admin_id,
                "profile_pin": "1234"
            })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(login_body["user"]["id"], admin_id);
    let session_set = find_set_cookie(&headers, "tp_session").expect("tp_session Set-Cookie");
    assert!(session_set.contains("HttpOnly"));
    assert!(session_set.contains("SameSite=Strict"));
    assert!(!session_set.contains("; Secure"));

    // Step 1 + Step 2 on loopback with X-Forwarded-Proto: https (Tailscale Serve) -> Secure MUST be present
    let (status, headers, _) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/auth/server-pin",
            TestHarness::loopback_addr(),
            &[],
            &[
                ("Tailscale-User-Login", "ian@ts.net"),
                ("X-Forwarded-Proto", "https"),
            ],
            Some(json!({ "server_pin": "123456" })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let ts_gate_set = find_set_cookie(&headers, "tp_gate").unwrap();
    assert!(ts_gate_set.contains("Secure"));
    let ts_gate_cookie = extract_cookie_pair(&headers, "tp_gate").unwrap();

    let (status, headers, _) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/auth/login",
            TestHarness::loopback_addr(),
            &[&ts_gate_cookie],
            &[
                ("Tailscale-User-Login", "ian@ts.net"),
                ("X-Forwarded-Proto", "https"),
            ],
            Some(json!({
                "user_id": admin_id,
                "profile_pin": "1234"
            })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let ts_session_set = find_set_cookie(&headers, "tp_session").unwrap();
    assert!(ts_session_set.contains("HttpOnly"));
    assert!(ts_session_set.contains("SameSite=Strict"));
    assert!(ts_session_set.contains("Secure"));

    // Logout revokes session
    let session_cookie = extract_cookie_pair(&headers, "tp_session").unwrap();
    let (status, _, _) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/auth/logout",
            TestHarness::loopback_addr(),
            &[&session_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _, _) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/auth/me",
            TestHarness::loopback_addr(),
            &[&session_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    app.db.shutdown().await.expect("db shutdown");
}

#[tokio::test]
async fn desktop_bootstrap_enforces_all_four_security_checks() {
    let secret = "test-super-secret-256bit-desktop-token";
    let app = TestHarness::new(Some(secret));

    // Check 2 before setup: 409 NOT_INITIALIZED
    let (status, _, body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/auth/desktop-bootstrap",
            TestHarness::loopback_addr(),
            &[],
            &[],
            Some(json!({ "token": secret })),
        ))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "NOT_INITIALIZED");

    let (admin_id, admin_cookie) = app.initialize_admin().await;

    // Check 1a: Tailscale-User-Login header -> 403 FORBIDDEN
    let (status, _, body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/auth/desktop-bootstrap",
            TestHarness::loopback_addr(),
            &[],
            &[("Tailscale-User-Login", "ian@ts.net")],
            Some(json!({ "token": secret })),
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "FORBIDDEN");

    // Check 1b: X-Forwarded-For header -> 403 FORBIDDEN
    let (status, _, body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/auth/desktop-bootstrap",
            TestHarness::loopback_addr(),
            &[],
            &[("X-Forwarded-For", "192.168.1.99")],
            Some(json!({ "token": secret })),
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "FORBIDDEN");

    // Check 1c: Non-loopback TCP peer -> 403 FORBIDDEN
    let (status, _, body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/auth/desktop-bootstrap",
            TestHarness::lan_addr(22),
            &[],
            &[],
            Some(json!({ "token": secret })),
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "FORBIDDEN");

    // Check 4: invalid secret on direct loopback -> 401 UNAUTHORIZED
    let (status, _, body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/auth/desktop-bootstrap",
            TestHarness::loopback_addr(),
            &[],
            &[],
            Some(json!({ "token": "wrong-secret" })),
        ))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");

    // Valid secret on direct loopback -> 200 OK + tp_session cookie
    let (status, headers, body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/auth/desktop-bootstrap",
            TestHarness::loopback_addr(),
            &[],
            &[],
            Some(json!({ "token": secret })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["user"]["id"], admin_id);
    assert!(extract_cookie_pair(&headers, "tp_session").is_some());

    // Check 3: desktop_pin_lock = true -> 403 DESKTOP_PIN_LOCKED
    let (status, _, _) = app
        .send(TestHarness::req_json(
            Method::PATCH,
            "/api/admin/settings",
            TestHarness::loopback_addr(),
            &[&admin_cookie],
            &[],
            Some(json!({ "desktop_pin_lock": true })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/auth/desktop-bootstrap",
            TestHarness::loopback_addr(),
            &[],
            &[],
            Some(json!({ "token": secret })),
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "DESKTOP_PIN_LOCKED");

    app.db.shutdown().await.expect("db shutdown");
}

#[tokio::test]
async fn full_rbac_matrix_and_people_folder_isolation() {
    let app = TestHarness::new(None);
    let (admin_id, admin_cookie) = app.initialize_admin().await;

    let dir1 = app._tmp.path().join("folder1");
    let dir2 = app._tmp.path().join("folder2");
    std::fs::create_dir_all(&dir1).unwrap();
    std::fs::create_dir_all(&dir2).unwrap();

    // Admin creates Folder 1 & Folder 2
    let (s1, _, f1_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/folders",
            TestHarness::loopback_addr(),
            &[&admin_cookie],
            &[],
            Some(json!({
                "name": "Family",
                "root_path": dir1.to_string_lossy(),
                "allow_disk_delete": false,
                "is_managed_uploads": false
            })),
        ))
        .await;
    assert_eq!(s1, StatusCode::CREATED);
    let folder1_id = f1_body["folder"]["id"].as_str().unwrap().to_string();

    let (s2, _, f2_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/folders",
            TestHarness::loopback_addr(),
            &[&admin_cookie],
            &[],
            Some(json!({
                "name": "Private Archive",
                "root_path": dir2.to_string_lossy(),
                "allow_disk_delete": false,
                "is_managed_uploads": false
            })),
        ))
        .await;
    assert_eq!(s2, StatusCode::CREATED);
    let folder2_id = f2_body["folder"]["id"].as_str().unwrap().to_string();

    // Admin creates Contributor & Viewer granted ONLY Folder 1
    let (sc, _, contrib_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/admin/users",
            TestHarness::loopback_addr(),
            &[&admin_cookie],
            &[],
            Some(json!({
                "username": "contrib",
                "display_name": "Contributor",
                "role": "contributor",
                "profile_pin": "2222",
                "allowed_folder_ids": [&folder1_id]
            })),
        ))
        .await;
    assert_eq!(sc, StatusCode::CREATED);
    let contrib_id = contrib_body["user"]["id"].as_str().unwrap().to_string();

    let (sv, _, viewer_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/admin/users",
            TestHarness::loopback_addr(),
            &[&admin_cookie],
            &[],
            Some(json!({
                "username": "mom",
                "display_name": "Mom",
                "role": "viewer",
                "profile_pin": "3333",
                "allowed_folder_ids": [&folder1_id]
            })),
        ))
        .await;
    assert_eq!(sv, StatusCode::CREATED);
    let viewer_id = viewer_body["user"]["id"].as_str().unwrap().to_string();

    // Last-admin safeguards: cannot demote, disable, or delete self/last admin -> 409 CONFLICT
    let (status, _, body) = app
        .send(TestHarness::req_json(
            Method::PATCH,
            &format!("/api/admin/users/{admin_id}"),
            TestHarness::loopback_addr(),
            &[&admin_cookie],
            &[],
            Some(json!({ "role": "viewer" })),
        ))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "CONFLICT");

    let (status, _, body) = app
        .send(TestHarness::req_json(
            Method::DELETE,
            &format!("/api/admin/users/{admin_id}"),
            TestHarness::loopback_addr(),
            &[&admin_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "CONFLICT");

    // Seed assets & people across Folder 1 and Folder 2 directly in DB
    let f1_clone = folder1_id.clone();
    let f2_clone = folder2_id.clone();
    app.db
        .write(move |conn| {
            let now = 1_750_000_000_000i64;
            conn.execute(
                "INSERT INTO assets (id, folder_id, kind, capture_at_ms, created_at_ms, updated_at_ms)
                 VALUES ('asset-1', ?1, 'photo', ?3, ?3, ?3),
                        ('asset-2', ?2, 'photo', ?4, ?4, ?4)",
                params![f1_clone, f2_clone, now, now + 1000],
            )?;
            // Person A has faces in BOTH Folder 1 and Folder 2, with cover_face_id pinned in Folder 2!
            // Person B has a face ONLY in Folder 2.
            conn.execute(
                "INSERT INTO people (id, name, is_hidden, is_pinned, cover_face_id, created_at_ms, updated_at_ms)
                 VALUES ('person-a', 'Alice', 0, 0, 'face-a-f2', ?1, ?1),
                        ('person-b', 'Secret Bob', 0, 0, 'face-b-f2', ?1, ?1)",
                params![now],
            )?;
            conn.execute(
                "INSERT INTO faces (id, asset_id, person_id, bbox_x, bbox_y, bbox_w, bbox_h, det_score, assigned_by, created_at_ms, updated_at_ms)
                 VALUES ('face-a-f1', 'asset-1', 'person-a', 0.1, 0.1, 0.2, 0.2, 0.91, 'manual', ?1, ?1),
                        ('face-a-f2', 'asset-2', 'person-a', 0.1, 0.1, 0.2, 0.2, 0.99, 'manual', ?1, ?1),
                        ('face-b-f2', 'asset-2', 'person-b', 0.3, 0.1, 0.2, 0.2, 0.95, 'manual', ?1, ?1)",
                params![now],
            )?;
            Ok(())
        })
        .await
        .expect("seed assets & people");

    // Log in as Contributor and Viewer
    let (_, contrib_headers, _) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/auth/login",
            TestHarness::loopback_addr(),
            &[],
            &[],
            Some(json!({ "user_id": contrib_id, "profile_pin": "2222", "server_pin": "123456" })),
        ))
        .await;
    let contrib_cookie = extract_cookie_pair(&contrib_headers, "tp_session").unwrap();

    let (_, viewer_headers, _) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/auth/login",
            TestHarness::loopback_addr(),
            &[],
            &[],
            Some(json!({ "user_id": viewer_id, "profile_pin": "3333", "server_pin": "123456" })),
        ))
        .await;
    let viewer_cookie = extract_cookie_pair(&viewer_headers, "tp_session").unwrap();

    // Admin sees 2 folders, 2 assets, 2 people (with Person A's pinned face-a-f2 cover)
    let (_, _, admin_folders) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/folders",
            TestHarness::loopback_addr(),
            &[&admin_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(admin_folders["items"].as_array().unwrap().len(), 2);

    let (_, _, admin_people) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/people",
            TestHarness::loopback_addr(),
            &[&admin_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(admin_people["items"].as_array().unwrap().len(), 2);

    // Admin backup works
    let (status, _, backup_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/admin/backup",
            TestHarness::loopback_addr(),
            &[&admin_cookie],
            &[],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let backup_path = backup_body["path"].as_str().unwrap();
    assert!(std::path::Path::new(backup_path).exists());

    // Contributor & Viewer checks
    for (role_name, cookie) in [("contributor", &contrib_cookie), ("viewer", &viewer_cookie)] {
        // Admin-only routes -> 403
        for (method, path, payload) in [
            (
                Method::POST,
                "/api/folders",
                Some(json!({"name":"x","root_path":dir1.to_string_lossy()})),
            ),
            (Method::GET, "/api/admin/users", None),
            (Method::GET, "/api/admin/settings", None),
            (
                Method::PATCH,
                "/api/admin/settings",
                Some(json!({"desktop_pin_lock":true})),
            ),
            (Method::POST, "/api/admin/backup", None),
        ] {
            let (status, _, body) = app
                .send(TestHarness::req_json(
                    method.clone(),
                    path,
                    TestHarness::loopback_addr(),
                    &[cookie],
                    &[],
                    payload,
                ))
                .await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{role_name} should get 403 on {method} {path}: {body}"
            );
        }

        // Folders scoped to Folder 1 only
        let (status, _, folders) = app
            .send(TestHarness::req_json(
                Method::GET,
                "/api/folders",
                TestHarness::loopback_addr(),
                &[cookie],
                &[],
                None,
            ))
            .await;
        assert_eq!(status, StatusCode::OK);
        let folder_items = folders["items"].as_array().unwrap();
        assert_eq!(folder_items.len(), 1);
        assert_eq!(folder_items[0]["id"], folder1_id);

        // Assets scoped to Folder 1 only
        let (status, _, assets) = app
            .send(TestHarness::req_json(
                Method::GET,
                "/api/assets",
                TestHarness::loopback_addr(),
                &[cookie],
                &[],
                None,
            ))
            .await;
        assert_eq!(status, StatusCode::OK);
        let asset_items = assets["items"].as_array().unwrap();
        assert_eq!(asset_items.len(), 1);
        assert_eq!(asset_items[0]["id"], "asset-1");

        // People strictly scoped: sees ONLY person-a with cover_face_id = face-a-f1 (NOT face-a-f2 or person-b!)
        let (status, _, people) = app
            .send(TestHarness::req_json(
                Method::GET,
                "/api/people",
                TestHarness::loopback_addr(),
                &[cookie],
                &[],
                None,
            ))
            .await;
        assert_eq!(status, StatusCode::OK);
        let people_items = people["items"].as_array().unwrap();
        assert_eq!(people_items.len(), 1);
        assert_eq!(people_items[0]["id"], "person-a");
        assert_eq!(people_items[0]["face_count"], 1);
        assert_eq!(people_items[0]["cover_face_id"], "face-a-f1");
    }

    app.db.shutdown().await.expect("db shutdown");
}

#[tokio::test]
async fn spa_fallback_vs_api_404() {
    let app = TestHarness::new(None);

    let (status, headers, bytes) = app
        .send_raw(TestHarness::req_json(
            Method::GET,
            "/timeline",
            TestHarness::loopback_addr(),
            &[],
            &[],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("text/html"), "expected text/html, got {ct}");
    assert!(!bytes.is_empty());

    let (status, headers, body) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/nonexistent",
            TestHarness::loopback_addr(),
            &[],
            &[],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.starts_with("application/json"),
        "expected application/json, got {ct}"
    );
    assert_eq!(body["error"]["code"], "NOT_FOUND");

    app.db.shutdown().await.expect("db shutdown");
}

#[tokio::test]
async fn live_tcp_server_and_rfc6455_websocket_roundtrip() {
    let tmp = TempDir::new().expect("temp dir");
    let mut config = ServerConfig::from_data_dir(tmp.path().to_path_buf());
    config.host = "127.0.0.1".to_string();
    config.port = 0; // Ephemeral TCP port
    config.mdns_enabled = false;

    let db = DbHandle::open(&config.db_path).expect("open db");
    let (_power_tx, power_rx) = watch::channel(PowerState::default());

    let handle = start_server(config, db.clone(), power_rx)
        .await
        .expect("start server");
    let port = handle.port();
    assert_ne!(port, 0);

    // 1. Initialize via raw HTTP over TcpStream
    let init_payload = json!({
        "admin_username": "ian",
        "admin_display_name": "Ian",
        "admin_profile_pin": "1234",
        "server_pin": "123456",
        "managed_uploads_dir": tmp.path().join("uploads").to_string_lossy(),
        "thumbnails_dir": tmp.path().join("thumbnails").to_string_lossy(),
        "cache_dir": tmp.path().join("cache").to_string_lossy(),
        "backup_dir": tmp.path().join("backups").to_string_lossy(),
    })
    .to_string();

    let (status_line, headers, _) = raw_http_request(
        port,
        "POST",
        "/api/setup/initialize",
        &[],
        Some(&init_payload),
    )
    .await;
    assert!(
        status_line.contains("201"),
        "expected 201 Created, got {status_line}"
    );
    let set_cookie = headers
        .iter()
        .find(|(k, v)| k.eq_ignore_ascii_case("set-cookie") && v.starts_with("tp_session="))
        .map(|(_, v)| v.split(';').next().unwrap().trim().to_string())
        .expect("tp_session cookie from live setup");

    // 2. Unauthenticated WS upgrade on live TCP -> 401
    let (unauth_ws_status, _, _) = raw_http_request(
        port,
        "GET",
        "/api/ws",
        &[
            ("Connection", "Upgrade"),
            ("Upgrade", "websocket"),
            ("Sec-WebSocket-Version", "13"),
            ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ],
        None,
    )
    .await;
    assert!(
        unauth_ws_status.contains("401"),
        "expected 401 on unauth ws, got {unauth_ws_status}"
    );

    // 3. Authenticated RFC 6455 WebSocket handshake + connected + ping -> pong
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect tcp");
    let handshake = format!(
        "GET /api/ws HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Connection: Upgrade\r\n\
         Upgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Cookie: {set_cookie}\r\n\r\n"
    );
    stream
        .write_all(handshake.as_bytes())
        .await
        .expect("write ws handshake");

    let mut leftover = read_http_upgrade_headers(&mut stream).await;

    // Receive `connected` text frame
    let connected_text = read_ws_text_frame(&mut stream, &mut leftover).await;
    let connected_json: Value = serde_json::from_str(&connected_text).expect("valid json");
    assert_eq!(connected_json["type"], "connected");
    assert_eq!(connected_json["role"], "admin");

    // Send masked client `{"type":"ping"}` frame
    write_ws_masked_text_frame(&mut stream, r#"{"type":"ping"}"#).await;

    // Receive `pong` text frame
    let pong_text = read_ws_text_frame(&mut stream, &mut leftover).await;
    let pong_json: Value = serde_json::from_str(&pong_text).expect("valid pong json");
    assert_eq!(pong_json["type"], "pong");

    drop(stream);
    handle.shutdown().await;
    db.shutdown().await.expect("db shutdown");
}

async fn raw_http_request(
    port: u16,
    method: &str,
    path: &str,
    extra_headers: &[(&str, &str)],
    json_body: Option<&str>,
) -> (String, Vec<(String, String)>, Vec<u8>) {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    let mut req =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    for (k, v) in extra_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(body) = json_body {
        req.push_str("Content-Type: application/json\r\n");
        req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    } else {
        req.push_str("\r\n");
    }
    stream.write_all(req.as_bytes()).await.expect("write req");

    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.expect("read resp");

    let header_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("http headers end");
    let header_str = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = header_str.split("\r\n");
    let status_line = lines.next().unwrap_or("").to_string();
    let headers = lines
        .filter_map(|line| {
            let (k, v) = line.split_once(':')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect();
    let body = buf[header_end + 4..].to_vec();
    (status_line, headers, body)
}

async fn read_http_upgrade_headers(stream: &mut TcpStream) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 512];
    loop {
        let n = stream.read(&mut tmp).await.expect("read ws upgrade");
        assert!(n > 0, "EOF before WS 101 response");
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]);
            assert!(
                head.contains("101 Switching Protocols"),
                "expected 101 Switching Protocols, got:\n{head}"
            );
            return buf[pos + 4..].to_vec();
        }
    }
}

async fn read_exact_buffered(stream: &mut TcpStream, buf: &mut Vec<u8>, len: usize) -> Vec<u8> {
    while buf.len() < len {
        let mut tmp = [0u8; 512];
        let n = stream.read(&mut tmp).await.expect("read ws frame");
        assert!(n > 0, "unexpected EOF reading WS frame");
        buf.extend_from_slice(&tmp[..n]);
    }
    buf.drain(..len).collect()
}

async fn read_ws_text_frame(stream: &mut TcpStream, buf: &mut Vec<u8>) -> String {
    loop {
        let hdr = read_exact_buffered(stream, buf, 2).await;
        let opcode = hdr[0] & 0x0f;
        let masked = (hdr[1] & 0x80) != 0;
        let mut len = (hdr[1] & 0x7f) as usize;
        if len == 126 {
            let ext = read_exact_buffered(stream, buf, 2).await;
            len = u16::from_be_bytes([ext[0], ext[1]]) as usize;
        } else if len == 127 {
            let ext = read_exact_buffered(stream, buf, 8).await;
            len = u64::from_be_bytes(ext.try_into().unwrap()) as usize;
        }
        let mask_key = if masked {
            Some(read_exact_buffered(stream, buf, 4).await)
        } else {
            None
        };
        let mut payload = read_exact_buffered(stream, buf, len).await;
        if let Some(mask) = mask_key {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
        }
        if opcode == 0x1 {
            return String::from_utf8(payload).expect("utf8 text frame");
        }
    }
}

async fn write_ws_masked_text_frame(stream: &mut TcpStream, text: &str) {
    let payload = text.as_bytes();
    assert!(payload.len() < 126);
    let mask = [0x12u8, 0x34, 0x56, 0x78];
    let mut frame = Vec::with_capacity(6 + payload.len());
    frame.push(0x81); // FIN + Text
    frame.push(0x80 | (payload.len() as u8)); // Masked + len
    frame.extend_from_slice(&mask);
    for (i, b) in payload.iter().enumerate() {
        frame.push(b ^ mask[i % 4]);
    }
    stream.write_all(&frame).await.expect("write ws frame");
}
