use crate::extractors::AuthSession;
use crate::state::AppState;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use truongphoto_core::{WsClientMessage, WsServerEvent};
use truongphoto_db::repo;

/// Authenticated WebSocket upgrade endpoint (`GET /api/ws`).
pub async fn ws_handler(
    auth: AuthSession,
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state, auth))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>, auth: AuthSession) {
    let (mut sender, mut receiver) = socket.split();

    let connected = WsServerEvent::Connected {
        server_version: env!("CARGO_PKG_VERSION").to_string(),
        user_id: auth.user.id.clone(),
        role: auth.user.role,
    };
    if let Ok(json) = serde_json::to_string(&connected) {
        if sender.send(Message::text(json)).await.is_err() {
            return;
        }
    }

    let mut rx = state.ws_tx.subscribe();
    let shutdown = state.shutdown_token.clone();

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                let _ = sender.send(Message::Close(None)).await;
                break;
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
                        if let Ok(json) = serde_json::to_string(&event) {
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
