use crate::extractors::AuthSession;
use crate::state::AppState;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use truongphoto_core::{Role, WsClientMessage, WsServerEvent, APP_VERSION};
use truongphoto_db::repo;

pub(crate) fn is_same_origin_request(headers: &HeaderMap) -> bool {
    if let Some(sfs) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        if sfs.eq_ignore_ascii_case("cross-site") {
            return false;
        }
    }
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let origin_trimmed = origin.trim();
    if origin_trimmed.is_empty() || origin_trimmed.eq_ignore_ascii_case("null") {
        return false;
    }
    let origin_host = origin_trimmed
        .strip_prefix("http://")
        .or_else(|| origin_trimmed.strip_prefix("https://"))
        .or_else(|| origin_trimmed.strip_prefix("tauri://"))
        .map(|rest| rest.split('/').next().unwrap_or(rest));
    let Some(origin_host) = origin_host else {
        return false;
    };
    // Allow Tauri desktop WebView origins (`tauri://localhost`, `http://tauri.localhost`, `https://tauri.localhost`)
    if origin_host.eq_ignore_ascii_case("tauri.localhost")
        || origin_trimmed.eq_ignore_ascii_case("tauri://localhost")
    {
        return true;
    }
    let Some(req_host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    origin_host.eq_ignore_ascii_case(req_host.trim())
}

/// Authenticated WebSocket upgrade endpoint (`GET /api/ws`).
pub async fn ws_handler(
    auth: AuthSession,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> Response {
    if !is_same_origin_request(&headers) {
        return (StatusCode::FORBIDDEN, "Cross-origin WebSocket rejected.").into_response();
    }
    ws.on_upgrade(move |socket| handle_socket(socket, state, auth))
}

async fn should_deliver_ws_event(
    state: &AppState,
    user_id: &str,
    role: Role,
    event: &WsServerEvent,
) -> bool {
    if role == Role::Admin {
        return true;
    }
    match event {
        WsServerEvent::SettingsChanged { .. }
        | WsServerEvent::TailscaleStatusChanged { .. }
        | WsServerEvent::AiStateChanged { .. }
        | WsServerEvent::JobProgress { .. } => false,
        WsServerEvent::FolderChanged { folder_id, .. } => {
            let uid = user_id.to_string();
            let fid = folder_id.clone();
            state
                .db
                .read(move |conn| {
                    let allowed = repo::users::get_allowed_folder_ids(conn, &uid)?;
                    Ok(allowed.iter().any(|id| id == &fid))
                })
                .await
                .unwrap_or(false)
        }
        _ => true,
    }
}

fn sanitize_ws_event_for_role(role: Role, event: WsServerEvent) -> WsServerEvent {
    if role == Role::Admin {
        return event;
    }
    match event {
        WsServerEvent::Heartbeat { uptime_secs, .. } => WsServerEvent::Heartbeat {
            uptime_secs,
            process_ram_bytes: 0,
            db_size_bytes: 0,
            active_jobs: 0,
        },
        other => other,
    }
}

async fn verify_ws_session_role(state: &AppState, token_hash: &str) -> Option<Role> {
    let hash = token_hash.to_string();
    let now_ms = chrono::Utc::now().timestamp_millis();
    state
        .db
        .read(move |conn| {
            let Some(session) =
                repo::sessions::find_active_session_by_token_hash(conn, &hash, now_ms)?
            else {
                return Ok(None);
            };
            let Some(user) = repo::users::get_user_by_id(conn, &session.user_id)? else {
                return Ok(None);
            };
            if user.disabled_at_ms.is_some() {
                return Ok(None);
            }
            Ok(Some(user.role))
        })
        .await
        .ok()
        .flatten()
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>, auth: AuthSession) {
    let (mut sender, mut receiver) = socket.split();
    let mut current_role = auth.user.role;
    let user_id = auth.user.id.clone();
    let token_hash = auth.session.token_hash.clone();

    let connected = WsServerEvent::Connected {
        server_version: APP_VERSION.to_string(),
        user_id: user_id.clone(),
        role: current_role,
    };
    if let Ok(json) = serde_json::to_string(&connected) {
        if sender.send(Message::text(json)).await.is_err() {
            return;
        }
    }

    let mut rx = state.ws_tx.subscribe();
    let shutdown = state.shutdown_token.clone();
    let mut auth_recheck = tokio::time::interval(Duration::from_secs(15));
    auth_recheck.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    auth_recheck.tick().await;

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                let _ = sender.send(Message::Close(None)).await;
                break;
            }
            _ = auth_recheck.tick() => {
                match verify_ws_session_role(&state, &token_hash).await {
                    Some(fresh_role) => {
                        current_role = fresh_role;
                    }
                    None => {
                        let _ = sender.send(Message::Close(None)).await;
                        break;
                    }
                }
            }
            inbound = receiver.next() => {
                match inbound {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(WsClientMessage::Ping) = serde_json::from_str::<WsClientMessage>(text.as_str()) {
                            if let Ok(pong) = serde_json::to_string(&WsServerEvent::Pong) {
                                if sender.send(Message::text(pong)).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                }
            }
            evt = rx.recv() => {
                match evt {
                    Ok(event) => {
                        if !should_deliver_ws_event(&state, &user_id, current_role, &event).await {
                            continue;
                        }
                        let outbound = sanitize_ws_event_for_role(current_role, event);
                        if let Ok(json) = serde_json::to_string(&outbound) {
                            if sender.send(Message::text(json)).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                }
            }
        }
    }
}

/// Spawns the 5-second `WsServerEvent::Heartbeat` task and `power_rx` watcher task.
pub fn spawn_background_broadcasters(state: Arc<AppState>) -> Vec<tokio::task::JoinHandle<()>> {
    let heartbeat_state = state.clone();
    let heartbeat_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Skip immediate t=0 tick so first heartbeat fires after 5s.
        interval.tick().await;

        loop {
            tokio::select! {
                _ = heartbeat_state.shutdown_token.cancelled() => break,
                _ = interval.tick() => {
                    let active_jobs = heartbeat_state
                        .db
                        .read(repo::jobs::count_active_jobs)
                        .await
                        .unwrap_or(0);
                    let evt = WsServerEvent::Heartbeat {
                        uptime_secs: heartbeat_state.started_at.elapsed().as_secs(),
                        process_ram_bytes: heartbeat_state.process_ram_bytes(),
                        db_size_bytes: heartbeat_state.db.db_size_bytes(),
                        active_jobs,
                    };
                    let _ = heartbeat_state.ws_tx.send(evt);
                }
            }
        }
    });

    let power_state = state.clone();
    let mut power_rx = state.power_rx.clone();
    let power_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = power_state.shutdown_token.cancelled() => break,
                changed = power_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let power = *power_rx.borrow_and_update();
                    let _ = power_state.ws_tx.send(WsServerEvent::PowerStateChanged { power });
                }
            }
        }
    });

    vec![heartbeat_task, power_task]
}
