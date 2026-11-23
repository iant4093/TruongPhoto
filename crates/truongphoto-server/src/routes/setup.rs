use crate::auth::{
    build_session_cookie, generate_random_token, hash_pin, hash_session_token,
    validate_profile_pin, validate_server_pin, SESSION_TTL_SECS,
};
use crate::error::{ApiJson, AppError};
use crate::extractors::PeerIdentityExtractor;
use crate::routes::auth::AuthLoginResponse;
use crate::state::AppState;
use axum::{
    extract::State,
    http::{header::USER_AGENT, HeaderMap, StatusCode},
    Json,
};
use axum_extra::extract::CookieJar;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use truongphoto_core::{AppSettings, ClientKind, DefaultPathsDto, HardwareInfo, Role, UserPublic};
use truongphoto_db::{repo, DbError};
use truongphoto_media::volume::resolve_volume_identity;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct SetupStatusResponse {
    pub initialized: bool,
    pub is_direct_loopback: bool,
    pub hardware: HardwareInfo,
    pub default_paths: DefaultPathsDto,
}

#[derive(Debug, Deserialize)]
pub struct SetupInitializeRequest {
    pub admin_username: String,
    pub admin_display_name: String,
    pub admin_profile_pin: String,
    pub server_pin: String,
    pub managed_uploads_dir: String,
    pub thumbnails_dir: String,
    pub cache_dir: String,
    pub backup_dir: String,
    #[serde(default)]
    pub initial_folder_path: Option<String>,
    #[serde(default)]
    pub initial_folder_name: Option<String>,
}

/// `GET /api/setup/status` (Public).
pub async fn setup_status(
    PeerIdentityExtractor(peer): PeerIdentityExtractor,
    State(state): State<Arc<AppState>>,
) -> Result<Json<SetupStatusResponse>, AppError> {
    let initialized = state.db.read(repo::settings::is_setup_completed).await?;

    Ok(Json(SetupStatusResponse {
        initialized,
        is_direct_loopback: peer.is_direct_loopback,
        hardware: state.hardware.clone(),
        default_paths: state.default_paths.to_default_paths_dto(),
    }))
}

/// `POST /api/setup/initialize` (Direct loopback only when `!initialized`; `409 ALREADY_INITIALIZED` thereafter).
pub async fn setup_initialize(
    PeerIdentityExtractor(peer): PeerIdentityExtractor,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    ApiJson(req): ApiJson<SetupInitializeRequest>,
) -> Result<(StatusCode, CookieJar, Json<AuthLoginResponse>), AppError> {
    let already_initialized = state.db.read(repo::settings::is_setup_completed).await?;
    if already_initialized {
        return Err(AppError::already_initialized(
            "TruongPhoto has already been initialized.",
        ));
    }

    if !peer.is_direct_loopback {
        return Err(AppError::forbidden(
            "Initial setup is only allowed from a direct localhost connection.",
        ));
    }

    let username = req.admin_username.trim().to_string();
    if username.is_empty() || username.len() > 64 || username.chars().any(char::is_whitespace) {
        return Err(AppError::validation(
            "Admin username must be 1..=64 non-whitespace characters.",
        ));
    }

    let display_name = req.admin_display_name.trim().to_string();
    if display_name.is_empty() || display_name.len() > 128 {
        return Err(AppError::validation(
            "Admin display name must be 1..=128 characters.",
        ));
    }

    validate_profile_pin(&req.admin_profile_pin)?;
    validate_server_pin(&req.server_pin)?;

    let managed_uploads_dir =
        truongphoto_core::process::normalize_folder_path(&req.managed_uploads_dir);
    let thumbnails_dir = truongphoto_core::process::normalize_folder_path(&req.thumbnails_dir);
    let cache_dir = truongphoto_core::process::normalize_folder_path(&req.cache_dir);
    let backup_dir = truongphoto_core::process::normalize_folder_path(&req.backup_dir);
    for (label, dir) in [
        ("managed_uploads_dir", &managed_uploads_dir),
        ("thumbnails_dir", &thumbnails_dir),
        ("cache_dir", &cache_dir),
        ("backup_dir", &backup_dir),
    ] {
        if dir.is_empty() {
            return Err(AppError::validation(format!("{label} cannot be empty.")));
        }
        std::fs::create_dir_all(dir).map_err(|e| {
            AppError::validation(format!("Failed to create {label} directory '{dir}': {e}"))
        })?;
    }

    let initial_folder = match req
        .initial_folder_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw_path) => {
            let norm_path = truongphoto_core::process::normalize_folder_path(raw_path);
            let path = Path::new(&norm_path);
            if !path.is_dir() {
                return Err(AppError::validation(
                    "initial_folder_path must refer to an existing directory.",
                ));
            }
            let derived_name = req
                .initial_folder_name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or("Library")
                        .to_string()
                });
            let vol = resolve_volume_identity(path);
            Some((
                Uuid::new_v4().to_string(),
                norm_path,
                derived_name,
                vol.volume_uuid,
                vol.volume_label,
            ))
        }
        None => None,
    };

    let (profile_pin_hash, server_pin_hash) =
        tokio::try_join!(hash_pin(req.admin_profile_pin), hash_pin(req.server_pin))?;

    let user_id = Uuid::new_v4().to_string();
    let session_id = Uuid::new_v4().to_string();
    let raw_token = generate_random_token()?;
    let token_hash = hash_session_token(&raw_token);
    let now_ms = Utc::now().timestamp_millis();
    let session_expires_at_ms = now_ms + SESSION_TTL_SECS * 1000;
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let ip_addr = Some(peer.ip_string());

    let mut settings = AppSettings::from_paths(&state.default_paths);
    settings.port = state.bound_port;
    settings.mdns_enabled = state.mdns.lock().is_enabled();
    settings.mdns_hostname = state.mdns.lock().hostname().to_string();
    settings.managed_uploads_dir = managed_uploads_dir;
    settings.thumbnails_dir = thumbnails_dir;
    settings.cache_dir = cache_dir;
    settings.backup_dir = backup_dir;
    settings.ai_tier_vision = state.hardware.recommended_vision_tier;
    settings.ai_tier_chat = state.hardware.recommended_chat_tier;

    let initial_folder_for_watch = initial_folder
        .as_ref()
        .map(|(fid, root, _, _, _)| (fid.clone(), std::path::PathBuf::from(root)));
    let user_id_for_write = user_id.clone();
    let username_for_write = username.clone();
    let display_name_for_write = display_name.clone();
    let user_record = state
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            if repo::settings::is_setup_completed(&tx)? {
                return Err(DbError::Conflict(
                    "TruongPhoto has already been initialized.".to_string(),
                ));
            }

            let user = repo::users::create_user(
                &tx,
                &repo::users::CreateUser {
                    id: user_id_for_write.clone(),
                    username: username_for_write,
                    display_name: display_name_for_write,
                    role: Role::Admin,
                    pin_hash: profile_pin_hash,
                    avatar_color: "#4a4a4a".to_string(),
                },
            )?;

            repo::settings::save_app_settings(&tx, &settings)?;
            repo::settings::set_server_pin_hash(&tx, &server_pin_hash)?;
            repo::settings::set_setup_completed(&tx, true)?;

            if let Some((fid, root_path, name, volume_uuid, volume_label)) = initial_folder {
                repo::folders::create_folder(
                    &tx,
                    &repo::folders::CreateFolder {
                        id: fid,
                        name,
                        root_path,
                        volume_uuid,
                        volume_label,
                        is_managed_uploads: false,
                        allow_disk_delete: false,
                    },
                )?;
            }

            repo::sessions::create_session(
                &tx,
                &repo::sessions::CreateSession {
                    id: session_id,
                    token_hash,
                    user_id: user_id_for_write.clone(),
                    client_kind: ClientKind::Web,
                    user_agent,
                    ip_addr: ip_addr.clone(),
                    expires_at_ms: session_expires_at_ms,
                },
            )?;

            repo::settings::insert_audit_log(
                &tx,
                &repo::settings::NewAuditLog {
                    actor_user_id: Some(&user_id_for_write),
                    action: "setup.initialize",
                    target_type: Some("user"),
                    target_id: Some(&user_id_for_write),
                    detail_json: "{}",
                    ip_addr: ip_addr.as_deref(),
                },
            )?;

            tx.commit()?;
            Ok(user)
        })
        .await
        .map_err(|err| match err {
            DbError::Conflict(msg) => AppError::already_initialized(msg),
            other => AppError::from(other),
        })?;

    if let Some((fid, root_path)) = initial_folder_for_watch {
        state.watcher.watch_folder(&state, fid, root_path);
    }

    let jar = jar.add(build_session_cookie(&raw_token, peer.is_https));

    Ok((
        StatusCode::CREATED,
        jar,
        Json(AuthLoginResponse {
            user: UserPublic::from(user_record),
            session_expires_at_ms,
        }),
    ))
}
