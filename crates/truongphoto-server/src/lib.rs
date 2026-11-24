//! Axum 0.8 REST & WebSocket server, Argon2id PIN authentication, rate limiter,
//! embedded SPA serving, and Bonjour/mDNS lifecycle for TruongPhoto.

pub mod ai;
pub mod assets_embed;
pub mod auth;
pub mod backup_scheduler;
pub mod error;
pub mod extractors;
pub mod ingest;
pub mod load_monitor;
pub mod mdns;
pub mod rate_limit;
pub mod routes;
pub mod state;
pub mod tailscale;
pub mod ws;

use axum::{
    extract::DefaultBodyLimit,
    http::{header, Extensions, HeaderMap, HeaderName, HeaderValue, StatusCode, Version},
    response::IntoResponse,
    Router,
};
use mdns::MdnsHandle;
use state::AppState;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tower_http::compression::{
    predicate::{DefaultPredicate, NotForContentType, Predicate},
    CompressionLayer,
};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;
use truongphoto_core::{PowerState, ServerConfig, StoragePaths, WsServerEvent};
use truongphoto_db::{repo, DbHandle};

const MAX_JSON_BODY_BYTES: usize = 4 * 1024 * 1024;
const WS_BROADCAST_CAPACITY: usize = 256;

/// Live handle to a running TruongPhoto Axum HTTP/WebSocket server.
pub struct ServerHandle {
    bound_addr: SocketAddr,
    state: Arc<AppState>,
    shutdown_token: CancellationToken,
    server_task: JoinHandle<()>,
    bg_tasks: Vec<JoinHandle<()>>,
}

impl ServerHandle {
    /// Returns the actual bound TCP socket address.
    pub fn bound_addr(&self) -> SocketAddr {
        self.bound_addr
    }

    /// Returns the actual bound TCP port (resolves port `0` ephemeral bindings).
    pub fn port(&self) -> u16 {
        self.bound_addr.port()
    }

    /// Returns the broadcast sender for pushing `WsServerEvent` notifications.
    pub fn ws_tx(&self) -> &broadcast::Sender<WsServerEvent> {
        &self.state.ws_tx
    }

    /// Returns a reference to the shared `AppState`.
    pub fn state(&self) -> &Arc<AppState> {
        &self.state
    }

    /// Executes deterministic ordered shutdown:
    /// 1. Stop OS filesystem watchers
    /// 2. Cancel shutdown token (drains Axum + background workers)
    /// 3. Await HTTP server drain (up to 3s) & join background tasks (up to 2s before aborting stragglers)
    /// 4. Shutdown mDNS advertisement
    /// 5. Evict AI sessions & persist final vector state
    pub async fn shutdown(self) {
        self.state.watcher.stop_all();
        self.shutdown_token.cancel();

        let _ = tokio::time::timeout(Duration::from_secs(3), self.server_task).await;

        let abort_handles: Vec<_> = self.bg_tasks.iter().map(|h| h.abort_handle()).collect();
        let join_all_fut = futures_util::future::join_all(self.bg_tasks);
        if tokio::time::timeout(Duration::from_secs(2), join_all_fut)
            .await
            .is_err()
        {
            for handle in abort_handles {
                handle.abort();
            }
        }

        let state_for_mdns = Arc::clone(&self.state);
        let _ = tokio::task::spawn_blocking(move || {
            let mut mdns = state_for_mdns.mdns.lock();
            mdns.shutdown();
        })
        .await;

        let state_for_ai = Arc::clone(&self.state);
        let _ = tokio::task::spawn_blocking(move || {
            state_for_ai.ai.shutdown();
        })
        .await;
    }
}

/// Rejects cross-origin state-changing HTTP requests (`POST`/`PUT`/`PATCH`/`DELETE`) using
/// `Origin` and `Sec-Fetch-Site` headers so `SameSite=Lax` cookies remain protected against CSRF.
async fn csrf_origin_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let is_safe = matches!(
        *req.method(),
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
    );
    let peer_ip = req
        .extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip());
    if !is_safe && !ws::is_same_origin_request(req.headers(), peer_ip) {
        return (StatusCode::FORBIDDEN, "Cross-origin request rejected.").into_response();
    }
    next.run(req).await
}

/// Builds the complete TruongPhoto Axum router (`/api/*` + embedded SPA fallback).
pub fn build_router(state: Arc<AppState>) -> Router {
    let compression_predicate = DefaultPredicate::new()
        .and(NotForContentType::const_new("video/"))
        .and(NotForContentType::const_new("application/octet-stream"))
        .and(NotForContentType::const_new("application/zip"))
        .and(
            |status: StatusCode, _ver: Version, headers: &HeaderMap, _ext: &Extensions| {
                status != StatusCode::PARTIAL_CONTENT
                    && !headers.contains_key(header::CONTENT_RANGE)
                    && !headers.contains_key(header::CONTENT_DISPOSITION)
            },
        );

    Router::new()
        .nest("/api", routes::api_router())
        .fallback(assets_embed::spa_fallback)
        .layer(axum::middleware::from_fn(csrf_origin_middleware))
        .layer(DefaultBodyLimit::max(MAX_JSON_BODY_BYTES))
        .layer(CompressionLayer::new().compress_when(compression_predicate))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("referrer-policy"),
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self' ipc: http://ipc.localhost https://ipc.localhost; \
                 img-src 'self' https://tiles.openfreemap.org data: blob:; \
                 style-src 'self' 'unsafe-inline'; \
                 font-src 'self' https://tiles.openfreemap.org data:; \
                 connect-src 'self' ws: wss: ipc: http://ipc.localhost https://ipc.localhost https://tiles.openfreemap.org; \
                 media-src 'self' blob:; \
                 worker-src 'self' blob:; \
                 child-src 'self' blob:; \
                 frame-ancestors 'none';",
            ),
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Starts the TruongPhoto HTTP/WebSocket server on `config.host:config.port`.
pub async fn start_server(
    config: ServerConfig,
    db: DbHandle,
    power_rx: watch::Receiver<PowerState>,
) -> anyhow::Result<ServerHandle> {
    let default_paths = StoragePaths::from_data_dir(config.data_dir.clone());
    default_paths.ensure_dirs()?;

    let (initialized, persisted_settings) = db
        .read({
            let default_paths = default_paths.clone();
            move |conn| {
                let initialized = repo::settings::is_setup_completed(conn)?;
                let settings = repo::settings::load_app_settings(conn, &default_paths)?;
                Ok((initialized, settings))
            }
        })
        .await?;

    let listener = tokio::net::TcpListener::bind((config.host.as_str(), config.port)).await?;
    let bound_addr = listener.local_addr()?;
    let bound_port = bound_addr.port();

    let (mdns_enabled, mdns_hostname) = if initialized {
        (
            config.mdns_enabled && persisted_settings.mdns_enabled,
            persisted_settings.mdns_hostname,
        )
    } else {
        (config.mdns_enabled, config.mdns_hostname.clone())
    };

    let mdns = MdnsHandle::start(mdns_hostname, bound_port, mdns_enabled);
    let hardware = truongphoto_ai::hardware::detect_hardware_info();
    let gate_key = auth::generate_process_key()
        .map_err(|e| anyhow::anyhow!("failed to generate gate key: {}", e.message))?;
    let (ws_tx, _) = broadcast::channel(WS_BROADCAST_CAPACITY);
    let shutdown_token = CancellationToken::new();

    let state = Arc::new(AppState::new(
        db,
        config,
        gate_key,
        ws_tx,
        power_rx,
        mdns,
        hardware,
        bound_port,
        shutdown_token.clone(),
    ));

    // Recover from any prior ungraceful shutdown: cancel stale inflight job rows and reap
    // orphaned upload staging parts / HLS temp directories.
    let now_ms = chrono::Utc::now().timestamp_millis();
    let _ = state
        .db
        .write(move |conn| truongphoto_db::repo::jobs::cancel_stale_inflight_jobs(conn, now_ms))
        .await;
    {
        let default_paths = state.default_paths.clone();
        if let Ok(settings) = state
            .db
            .read(move |conn| {
                truongphoto_db::repo::settings::load_app_settings(conn, &default_paths)
            })
            .await
        {
            let uploads_dir = std::path::PathBuf::from(settings.managed_uploads_dir);
            let cache_dir = std::path::PathBuf::from(settings.cache_dir);
            tokio::task::spawn_blocking(move || {
                ingest::trash_gc::reap_stale_temp_files(&uploads_dir, &cache_dir);
            });
        }
    }

    let mut bg_tasks = ws::spawn_background_broadcasters(state.clone());
    bg_tasks.push(ingest::trash_gc::spawn_trash_gc_task(state.clone()));
    bg_tasks.extend(ai::worker::spawn_ai_background_tasks(state.clone()));
    bg_tasks.push(backup_scheduler::spawn_backup_scheduler(
        state.clone(),
        shutdown_token.clone(),
    ));
    bg_tasks.push(load_monitor::spawn_load_monitor(
        state.clone(),
        shutdown_token.clone(),
    ));
    state.watcher.start_existing_folders(&state).await;
    let router = build_router(state.clone());
    let serve_shutdown = shutdown_token.clone();

    let server_task = tokio::spawn(async move {
        let make_svc = router.into_make_service_with_connect_info::<SocketAddr>();
        if let Err(err) = axum::serve(listener, make_svc)
            .with_graceful_shutdown(serve_shutdown.cancelled_owned())
            .await
        {
            tracing::error!(error = %err, "axum server exited with error");
        }
    });

    tracing::info!(addr = %bound_addr, "TruongPhoto server listening");

    Ok(ServerHandle {
        bound_addr,
        state,
        shutdown_token,
        server_task,
        bg_tasks,
    })
}
