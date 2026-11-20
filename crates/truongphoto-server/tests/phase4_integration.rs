use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{header, Method, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::watch;
use tower::ServiceExt;
use truongphoto_ai::AiPipeline;
use truongphoto_core::{PowerState, ServerConfig, StoragePaths, WsServerEvent};
use truongphoto_db::DbHandle;
use truongphoto_server::{
    auth,
    backup_scheduler::{list_backup_snapshots, run_scheduled_backup_check},
    build_router,
    load_monitor::compute_throttle_status,
    mdns::MdnsHandle,
    state::AppState,
    tailscale::{TailscaleCmdOutput, TailscaleRunner},
};
use uuid::Uuid;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("tp-p4-server-test-{}", Uuid::new_v4()));
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

#[derive(Debug, Clone)]
enum MockScenario {
    NotInstalled,
    LoggedOut,
    CertsDisabled,
    Ready { serve_active: bool },
    MacAppStoreSandbox,
    WindowsUacDenied,
}

struct MockTailscaleRunner {
    scenario: Mutex<MockScenario>,
    bound_port: u16,
    invocations: Mutex<Vec<Vec<String>>>,
}

impl MockTailscaleRunner {
    fn new(scenario: MockScenario, bound_port: u16) -> Self {
        Self {
            scenario: Mutex::new(scenario),
            bound_port,
            invocations: Mutex::new(Vec::new()),
        }
    }

    fn set_scenario(&self, scenario: MockScenario) {
        *self.scenario.lock() = scenario;
    }

    fn recorded_calls(&self) -> Vec<Vec<String>> {
        self.invocations.lock().clone()
    }
}

impl TailscaleRunner for MockTailscaleRunner {
    fn discover_cli(&self) -> Option<PathBuf> {
        let guard = self.scenario.lock();
        if matches!(*guard, MockScenario::NotInstalled) {
            None
        } else {
            Some(PathBuf::from(
                "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
            ))
        }
    }

    fn run<'a>(
        &'a self,
        _cli: &'a Path,
        args: &'a [&'a str],
    ) -> Pin<Box<dyn Future<Output = Result<TailscaleCmdOutput, String>> + Send + 'a>> {
        let owned_args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        self.invocations.lock().push(owned_args.clone());

        Box::pin(async move {
            let mut scenario = self.scenario.lock();
            if owned_args.as_slice() == ["status", "--json"] {
                let stdout = match &*scenario {
                    MockScenario::NotInstalled => {
                        return Err("tailscale CLI not found".to_string());
                    }
                    MockScenario::LoggedOut => json!({
                        "BackendState": "NeedsLogin",
                        "Self": { "DNSName": "" },
                        "CertDomains": []
                    })
                    .to_string(),
                    MockScenario::CertsDisabled => json!({
                        "BackendState": "Running",
                        "Self": { "DNSName": "truong-mac.tail1234.ts.net." },
                        "CertDomains": []
                    })
                    .to_string(),
                    MockScenario::Ready { .. }
                    | MockScenario::MacAppStoreSandbox
                    | MockScenario::WindowsUacDenied => json!({
                        "BackendState": "Running",
                        "Self": { "DNSName": "truong-mac.tail1234.ts.net." },
                        "CertDomains": ["truong-mac.tail1234.ts.net"]
                    })
                    .to_string(),
                };
                return Ok(TailscaleCmdOutput {
                    success: true,
                    stdout,
                    stderr: String::new(),
                });
            }

            if owned_args.as_slice() == ["serve", "status", "--json"] {
                let serve_active = matches!(*scenario, MockScenario::Ready { serve_active: true });
                let stdout = if serve_active {
                    json!({
                        "TCP": { "443": { "HTTPS": true } },
                        "Web": {
                            "truong-mac.tail1234.ts.net:443": {
                                "Handlers": {
                                    "/": {
                                        "Proxy": format!("http://127.0.0.1:{}", self.bound_port)
                                    }
                                }
                            }
                        }
                    })
                    .to_string()
                } else {
                    "{}".to_string()
                };
                return Ok(TailscaleCmdOutput {
                    success: true,
                    stdout,
                    stderr: String::new(),
                });
            }

            if owned_args.len() >= 2 && owned_args[0] == "serve" && owned_args[1] == "--bg" {
                return match &*scenario {
                    MockScenario::MacAppStoreSandbox => Ok(TailscaleCmdOutput {
                        success: false,
                        stdout: String::new(),
                        stderr: "EncryptState: Operation not permitted".to_string(),
                    }),
                    MockScenario::WindowsUacDenied => Ok(TailscaleCmdOutput {
                        success: false,
                        stdout: String::new(),
                        stderr: "Access is denied: requires elevation".to_string(),
                    }),
                    MockScenario::Ready { .. } => {
                        *scenario = MockScenario::Ready { serve_active: true };
                        Ok(TailscaleCmdOutput {
                            success: true,
                            stdout: String::new(),
                            stderr: String::new(),
                        })
                    }
                    _ => Ok(TailscaleCmdOutput {
                        success: false,
                        stdout: String::new(),
                        stderr: "unexpected state".to_string(),
                    }),
                };
            }

            if owned_args.as_slice() == ["serve", "--https=443", "off"]
                || owned_args.as_slice() == ["serve", "reset"]
            {
                if let MockScenario::Ready { .. } = *scenario {
                    *scenario = MockScenario::Ready {
                        serve_active: false,
                    };
                }
                return Ok(TailscaleCmdOutput {
                    success: true,
                    stdout: String::new(),
                    stderr: String::new(),
                });
            }

            Ok(TailscaleCmdOutput {
                success: false,
                stdout: String::new(),
                stderr: format!("unexpected args: {owned_args:?}"),
            })
        })
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
    backup_dir: PathBuf,
}

impl TestHarness {
    fn with_tailscale(runner: Arc<dyn TailscaleRunner>) -> Self {
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
            .with_ai_pipeline(AiPipeline::deterministic_test())
            .with_tailscale_runner(runner),
        );
        let router = build_router(state.clone());

        Self {
            thumbs_dir: tmp.path().join("thumbnails"),
            cache_dir: tmp.path().join("cache"),
            uploads_dir: tmp.path().join("uploads"),
            backup_dir: tmp.path().join("backups"),
            _tmp: tmp,
            db,
            state,
            router,
        }
    }

    fn loopback() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 51244)
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
                "backup_dir": self.backup_dir.to_string_lossy(),
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
                    "allowed_folder_ids": []
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
}

fn extract_cookie_pair(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    for val in headers.get_all(header::SET_COOKIE) {
        if let Ok(s) = val.to_str() {
            let prefix = format!("{name}=");
            if let Some(rest) = s.split(';').next() {
                if rest.starts_with(&prefix) {
                    return Some(rest.to_string());
                }
            }
        }
    }
    None
}

#[tokio::test]
async fn tailscale_mock_lifecycle_and_rbac() {
    let runner = Arc::new(MockTailscaleRunner::new(MockScenario::NotInstalled, 38420));
    let app = TestHarness::with_tailscale(runner.clone());
    let (_admin_id, admin_cookie) = app.initialize_admin().await;
    let viewer_cookie = app
        .create_user_and_login(&admin_cookie, "viewer1", "viewer")
        .await;
    let contrib_cookie = app
        .create_user_and_login(&admin_cookie, "contrib1", "contributor")
        .await;

    // 1. RBAC: Viewer & Contributor get 403 on admin tailscale endpoints
    for cookie in [&viewer_cookie, &contrib_cookie] {
        for (method, path) in [
            (Method::GET, "/api/admin/tailscale/status"),
            (Method::POST, "/api/admin/tailscale/enable"),
            (Method::POST, "/api/admin/tailscale/disable"),
            (Method::GET, "/api/admin/backups"),
        ] {
            let (status, _, _) = app
                .send(TestHarness::req_json(method, path, &[cookie], None))
                .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
        }
    }

    // 2. Not installed
    let (status, _, body) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/admin/tailscale/status",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["installed"], false);
    assert!(body["remediation"]
        .as_str()
        .unwrap()
        .contains("Install Tailscale"));

    // 3. Logged out
    runner.set_scenario(MockScenario::LoggedOut);
    let (status, _, body) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/admin/tailscale/status",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["installed"], true);
    assert_eq!(body["logged_in"], false);
    assert!(body["remediation"].as_str().unwrap().contains("Sign in"));

    // 4. Certs disabled
    runner.set_scenario(MockScenario::CertsDisabled);
    let (status, _, body) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/admin/tailscale/status",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["installed"], true);
    assert_eq!(body["running"], true);
    assert_eq!(body["logged_in"], true);
    assert_eq!(body["https_certs_enabled"], false);
    assert_eq!(body["magic_dns_name"], "truong-mac.tail1234.ts.net");
    assert!(body["remediation"].as_str().unwrap().contains("MagicDNS"));

    // 5. Mac App Store sandbox -> 409 CONFLICT
    runner.set_scenario(MockScenario::MacAppStoreSandbox);
    let (status, _, body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/admin/tailscale/enable",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "CONFLICT");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Standalone macOS"));

    // 6. Windows UAC denied -> 403 FORBIDDEN
    runner.set_scenario(MockScenario::WindowsUacDenied);
    let (status, _, body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/admin/tailscale/enable",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "FORBIDDEN");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Administrator"));

    // 7. Ready -> enable -> disable + WS broadcast + /api/system/info tailscale_url
    let mut ws_rx = app.state.ws_tx.subscribe();
    runner.set_scenario(MockScenario::Ready {
        serve_active: false,
    });
    let (status, _, enabled_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/admin/tailscale/enable",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(enabled_body["serve_active"], true);
    assert_eq!(
        enabled_body["https_url"],
        "https://truong-mac.tail1234.ts.net"
    );

    let ws_evt = ws_rx.recv().await.expect("WS TailscaleStatusChanged");
    match ws_evt {
        WsServerEvent::TailscaleStatusChanged { status } => {
            assert!(status.serve_active);
            assert_eq!(
                status.https_url.as_deref(),
                Some("https://truong-mac.tail1234.ts.net")
            );
        }
        other => panic!("unexpected WS event: {other:?}"),
    }

    let (status, _, sys_info) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/system/info",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        sys_info["tailscale_url"],
        "https://truong-mac.tail1234.ts.net"
    );

    let (status, _, disabled_body) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/admin/tailscale/disable",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(disabled_body["serve_active"], false);
    assert!(disabled_body["https_url"].is_null());

    let calls = runner.recorded_calls();
    assert!(calls
        .iter()
        .any(|c| { c == &vec!["serve", "--bg", "https", "/", "http://127.0.0.1:38420",] }));
    assert!(calls
        .iter()
        .any(|c| c == &vec!["serve", "--https=443", "off"]));

    app.db.shutdown().await.expect("db shutdown");
}

#[tokio::test]
async fn qr_svg_endpoint_and_security_headers() {
    let runner = Arc::new(MockTailscaleRunner::new(MockScenario::NotInstalled, 38420));
    let app = TestHarness::with_tailscale(runner);
    let (_admin_id, admin_cookie) = app.initialize_admin().await;

    // 1. Security headers on /api/health
    let (status, health_headers, _) = app
        .send(TestHarness::req_json(Method::GET, "/api/health", &[], None))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        health_headers
            .get(header::X_CONTENT_TYPE_OPTIONS)
            .and_then(|v| v.to_str().ok()),
        Some("nosniff")
    );
    assert_eq!(
        health_headers
            .get(header::X_FRAME_OPTIONS)
            .and_then(|v| v.to_str().ok()),
        Some("DENY")
    );
    assert_eq!(
        health_headers
            .get("referrer-policy")
            .and_then(|v| v.to_str().ok()),
        Some("no-referrer")
    );

    // 2. Security headers on /timeline SPA fallback
    let (status, spa_headers, _) = app
        .send_raw(TestHarness::req_json(Method::GET, "/timeline", &[], None))
        .await;
    assert!(status == StatusCode::OK || status == StatusCode::NOT_FOUND);
    assert_eq!(
        spa_headers
            .get(header::X_CONTENT_TYPE_OPTIONS)
            .and_then(|v| v.to_str().ok()),
        Some("nosniff")
    );
    assert_eq!(
        spa_headers
            .get(header::X_FRAME_OPTIONS)
            .and_then(|v| v.to_str().ok()),
        Some("DENY")
    );
    assert_eq!(
        spa_headers
            .get("referrer-policy")
            .and_then(|v| v.to_str().ok()),
        Some("no-referrer")
    );

    // 3. Unauthenticated QR -> 401
    let (status, _, _) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/system/qr?url=https%3A%2F%2Ftruongphoto.ts.net",
            &[],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 4. Valid HTTPS URL -> 200 SVG with strict CSP & high-contrast colors
    let (status, qr_headers, svg_bytes) = app
        .send_raw(TestHarness::req_json(
            Method::GET,
            "/api/system/qr?url=https%3A%2F%2Ftruongphoto.ts.net",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        qr_headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("image/svg+xml; charset=utf-8")
    );
    assert_eq!(
        qr_headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("private, max-age=3600")
    );
    assert_eq!(
        qr_headers
            .get(header::CONTENT_SECURITY_POLICY)
            .and_then(|v| v.to_str().ok()),
        Some("default-src 'none'; style-src 'unsafe-inline'")
    );
    let svg_text = String::from_utf8(svg_bytes).expect("utf8 svg");
    assert!(svg_text.contains("<svg"));
    assert!(svg_text.contains("#171717"));
    assert!(svg_text.contains("#ededed"));

    // 5. Reject javascript: URI and control chars with 422 VALIDATION_ERROR
    for bad_url in [
        "/api/system/qr?url=javascript%3Aalert(1)",
        "/api/system/qr?url=ftp%3A%2F%2Fexample.com",
        "/api/system/qr?url=https%3A%2F%2Fexample.com%0Aevil",
        "/api/system/qr",
    ] {
        let (status, _, body) = app
            .send(TestHarness::req_json(
                Method::GET,
                bad_url,
                &[&admin_cookie],
                None,
            ))
            .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "expected 422 for {bad_url}: {body}"
        );
        assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
    }

    app.db.shutdown().await.expect("db shutdown");
}

#[tokio::test]
async fn backup_history_system_info_and_scheduler_check() {
    let runner = Arc::new(MockTailscaleRunner::new(MockScenario::NotInstalled, 38420));
    let app = TestHarness::with_tailscale(runner);
    let (_admin_id, admin_cookie) = app.initialize_admin().await;

    // 1. Before any backup: /api/admin/backups empty & system/info last_backup null
    let (status, _, empty_backups) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/admin/backups",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty_backups["items"].as_array().unwrap().len(), 0);
    assert_eq!(empty_backups["interval_hours"], 24);

    // 2. Scheduled check with interval = 0 (manual only) creates nothing
    let (status, _, _) = app
        .send(TestHarness::req_json(
            Method::PATCH,
            "/api/admin/settings",
            &[&admin_cookie],
            Some(json!({ "backup_interval_hours": 0 })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let skipped = run_scheduled_backup_check(&app.state)
        .await
        .expect("scheduled backup check");
    assert!(skipped.is_none());

    // 3. Re-enable interval = 24 -> scheduled check creates first snapshot
    let (status, _, _) = app
        .send(TestHarness::req_json(
            Method::PATCH,
            "/api/admin/settings",
            &[&admin_cookie],
            Some(json!({ "backup_interval_hours": 24 })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let created = run_scheduled_backup_check(&app.state)
        .await
        .expect("scheduled backup check")
        .expect("initial snapshot created");
    assert!(created.filename.starts_with("library-"));
    assert!(created.size_bytes > 0);

    // Immediate second check is skipped because < 24h elapsed
    let skipped_recent = run_scheduled_backup_check(&app.state)
        .await
        .expect("second scheduled check");
    assert!(skipped_recent.is_none());

    // 4. Manual POST /api/admin/backup + GET /api/admin/backups + GET /api/system/info
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let (status, _, manual_backup) = app
        .send(TestHarness::req_json(
            Method::POST,
            "/api/admin/backup",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let manual_path = manual_backup["path"].as_str().unwrap().to_string();

    let (status, _, list_body) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/admin/backups",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let items = list_body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["path"], manual_path);

    let snapshots = list_backup_snapshots(&app.backup_dir);
    assert_eq!(snapshots.len(), 2);

    let (status, _, sys_info) = app
        .send(TestHarness::req_json(
            Method::GET,
            "/api/system/info",
            &[&admin_cookie],
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sys_info["last_backup"]["path"], manual_path);
    assert_eq!(sys_info["throttle"]["active"], false);
    assert!(sys_info["tailscale_url"].is_null());

    app.db.shutdown().await.expect("db shutdown");
}

#[test]
fn compute_throttle_status_priorities() {
    let normal_power = PowerState::default();
    let t = compute_throttle_status(&normal_power, 25.0, true, 80);
    assert!(!t.active);
    assert!(t.reason.is_none());
    assert!(!t.on_battery);

    // high_cpu at threshold
    let t = compute_throttle_status(&normal_power, 80.0, true, 80);
    assert!(t.active);
    assert_eq!(t.reason.as_deref(), Some("high_cpu"));

    // battery overrides high_cpu when pause_on_battery = true
    let battery_power = PowerState {
        on_battery: true,
        ..Default::default()
    };
    let t = compute_throttle_status(&battery_power, 95.0, true, 80);
    assert!(t.active);
    assert_eq!(t.reason.as_deref(), Some("battery"));
    assert!(t.on_battery);

    // battery ignored when pause_on_battery = false, falls back to high_cpu
    let t = compute_throttle_status(&battery_power, 95.0, false, 80);
    assert!(t.active);
    assert_eq!(t.reason.as_deref(), Some("high_cpu"));

    // sleep overrides battery and high_cpu
    let sleep_power = PowerState {
        sleeping: true,
        on_battery: true,
        ..Default::default()
    };
    let t = compute_throttle_status(&sleep_power, 99.0, true, 80);
    assert!(t.active);
    assert_eq!(t.reason.as_deref(), Some("sleep"));
}
