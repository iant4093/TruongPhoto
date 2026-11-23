use crate::auth::{hash_pin, validate_profile_pin, validate_server_pin, verify_pin};
use crate::backup_scheduler::list_backup_snapshots;
use crate::error::{ApiJson, AppError};
use crate::extractors::RequireAdmin;
use crate::state::AppState;
use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use truongphoto_core::{
    AdminUserView, AppSettings, BackupListResponse, PartialAppSettings, Role, WsServerEvent,
};
use truongphoto_db::{repo, BackupResult, DbError};
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct AdminUserListResponse {
    pub items: Vec<AdminUserView>,
}

#[derive(Debug, Serialize)]
pub struct AdminUserResponse {
    pub user: AdminUserView,
}

#[derive(Debug, Deserialize)]
pub struct CreateAdminUserRequest {
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub profile_pin: String,
    #[serde(default)]
    pub avatar_color: Option<String>,
    #[serde(default)]
    pub allowed_folder_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAdminUserRequest {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub role: Option<Role>,
    #[serde(default)]
    pub profile_pin: Option<String>,
    #[serde(default)]
    pub avatar_color: Option<String>,
    #[serde(default)]
    pub disabled: Option<bool>,
    #[serde(default)]
    pub allowed_folder_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateServerPinRequest {
    pub current_server_pin: String,
    pub new_server_pin: String,
}

/// `GET /api/admin/users` (RequireAdmin).
pub async fn list_users(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
) -> Result<Json<AdminUserListResponse>, AppError> {
    let items = state.db.read(repo::users::list_users_with_acls).await?;
    Ok(Json(AdminUserListResponse { items }))
}

/// `POST /api/admin/users` (RequireAdmin).
pub async fn create_user(
    RequireAdmin(auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    ApiJson(req): ApiJson<CreateAdminUserRequest>,
) -> Result<(StatusCode, Json<AdminUserResponse>), AppError> {
    let username = req.username.trim().to_string();
    if username.is_empty() || username.len() > 64 || username.chars().any(char::is_whitespace) {
        return Err(AppError::validation(
            "Username must be 1..=64 non-whitespace characters.",
        ));
    }

    let display_name = req.display_name.trim().to_string();
    if display_name.is_empty() || display_name.len() > 128 {
        return Err(AppError::validation(
            "Display name must be 1..=128 characters.",
        ));
    }

    validate_profile_pin(&req.profile_pin)?;
    let pin_hash = hash_pin(req.profile_pin).await?;

    let avatar_color = req
        .avatar_color
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("#4a4a4a")
        .to_string();

    let user_id = Uuid::new_v4().to_string();
    let actor_id = auth.user.id.clone();
    let ip_addr = Some(auth.peer.ip_string());
    let allowed_folder_ids = req.allowed_folder_ids;

    let user_view = state
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let created = repo::users::create_user(
                &tx,
                &repo::users::CreateUser {
                    id: user_id.clone(),
                    username,
                    display_name,
                    role: req.role,
                    pin_hash,
                    avatar_color,
                },
            )?;

            if !allowed_folder_ids.is_empty() {
                repo::users::replace_user_folder_acls(&tx, &created.id, &allowed_folder_ids)?;
            }
            let effective_acls = repo::users::get_allowed_folder_ids(&tx, &created.id)?;

            repo::settings::insert_audit_log(
                &tx,
                &repo::settings::NewAuditLog {
                    actor_user_id: Some(&actor_id),
                    action: "admin.user.create",
                    target_type: Some("user"),
                    target_id: Some(&created.id),
                    detail_json: "{}",
                    ip_addr: ip_addr.as_deref(),
                },
            )?;
            tx.commit()?;

            Ok(AdminUserView {
                id: created.id,
                username: created.username,
                display_name: created.display_name,
                role: created.role,
                avatar_color: created.avatar_color,
                disabled_at_ms: created.disabled_at_ms,
                allowed_folder_ids: effective_acls,
                created_at_ms: created.created_at_ms,
            })
        })
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(AdminUserResponse { user: user_view }),
    ))
}

/// `PATCH /api/admin/users/{id}` (RequireAdmin; blocks demoting/disabling the last active admin -> `409 CONFLICT`).
pub async fn update_user(
    RequireAdmin(auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    AxumPath(target_id): AxumPath<String>,
    ApiJson(req): ApiJson<UpdateAdminUserRequest>,
) -> Result<Json<AdminUserResponse>, AppError> {
    let display_name = match req.display_name {
        Some(raw) => {
            let trimmed = raw.trim().to_string();
            if trimmed.is_empty() || trimmed.len() > 128 {
                return Err(AppError::validation(
                    "Display name must be 1..=128 characters.",
                ));
            }
            Some(trimmed)
        }
        None => None,
    };

    let pin_hash = match req.profile_pin {
        Some(pin) => {
            validate_profile_pin(&pin)?;
            Some(hash_pin(pin).await?)
        }
        None => None,
    };

    let avatar_color = req
        .avatar_color
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let actor_id = auth.user.id.clone();
    let ip_addr = Some(auth.peer.ip_string());
    let role_patch = req.role;
    let disabled_patch = req.disabled;
    let acl_patch = req.allowed_folder_ids;

    let user_view = state
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let Some(existing) = repo::users::get_user_by_id(&tx, &target_id)? else {
                return Err(DbError::NotFound("User not found.".to_string()));
            };

            let currently_active_admin =
                existing.role == Role::Admin && existing.disabled_at_ms.is_none();
            let would_demote = role_patch.is_some_and(|r| r != Role::Admin);
            let would_disable = disabled_patch == Some(true);

            if currently_active_admin && (would_demote || would_disable) {
                let active_admins = repo::users::count_active_admins(&tx)?;
                if active_admins <= 1 {
                    return Err(DbError::Conflict(
                        "Cannot demote or disable the last active administrator.".to_string(),
                    ));
                }
            }

            let pin_changed = pin_hash.is_some();
            let updated = repo::users::update_user(
                &tx,
                &target_id,
                &repo::users::UpdateUser {
                    display_name,
                    role: role_patch,
                    pin_hash,
                    avatar_color,
                    disabled: disabled_patch,
                },
            )?
            .ok_or_else(|| DbError::NotFound("User not found.".to_string()))?;

            if disabled_patch == Some(true) || (pin_changed && target_id != actor_id) {
                let now_ms = Utc::now().timestamp_millis();
                repo::sessions::revoke_sessions_for_user(&tx, &target_id, now_ms)?;
            }

            if let Some(ref folder_ids) = acl_patch {
                repo::users::replace_user_folder_acls(&tx, &target_id, folder_ids)?;
            }

            let allowed_folder_ids = repo::users::get_allowed_folder_ids(&tx, &target_id)?;

            repo::settings::insert_audit_log(
                &tx,
                &repo::settings::NewAuditLog {
                    actor_user_id: Some(&actor_id),
                    action: "admin.user.update",
                    target_type: Some("user"),
                    target_id: Some(&target_id),
                    detail_json: "{}",
                    ip_addr: ip_addr.as_deref(),
                },
            )?;
            tx.commit()?;

            Ok(AdminUserView {
                id: updated.id,
                username: updated.username,
                display_name: updated.display_name,
                role: updated.role,
                avatar_color: updated.avatar_color,
                disabled_at_ms: updated.disabled_at_ms,
                allowed_folder_ids,
                created_at_ms: updated.created_at_ms,
            })
        })
        .await?;

    Ok(Json(AdminUserResponse { user: user_view }))
}

/// `DELETE /api/admin/users/{id}` (RequireAdmin; blocks deleting last active admin or self -> `409 CONFLICT`).
pub async fn delete_user(
    RequireAdmin(auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    AxumPath(target_id): AxumPath<String>,
) -> Result<StatusCode, AppError> {
    if target_id == auth.user.id {
        return Err(AppError::conflict(
            "Cannot delete the currently authenticated administrator.",
        ));
    }

    let actor_id = auth.user.id.clone();
    let ip_addr = Some(auth.peer.ip_string());

    state
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let Some(existing) = repo::users::get_user_by_id(&tx, &target_id)? else {
                return Err(DbError::NotFound("User not found.".to_string()));
            };

            if existing.role == Role::Admin && existing.disabled_at_ms.is_none() {
                let active_admins = repo::users::count_active_admins(&tx)?;
                if active_admins <= 1 {
                    return Err(DbError::Conflict(
                        "Cannot delete the last active administrator.".to_string(),
                    ));
                }
            }

            repo::users::delete_user(&tx, &target_id)?;
            repo::settings::insert_audit_log(
                &tx,
                &repo::settings::NewAuditLog {
                    actor_user_id: Some(&actor_id),
                    action: "admin.user.delete",
                    target_type: Some("user"),
                    target_id: Some(&target_id),
                    detail_json: "{}",
                    ip_addr: ip_addr.as_deref(),
                },
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /api/admin/server-pin` (RequireAdmin).
pub async fn update_server_pin(
    RequireAdmin(auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    ApiJson(req): ApiJson<UpdateServerPinRequest>,
) -> Result<StatusCode, AppError> {
    if let Err(retry_after_secs) = state
        .rate_limiter
        .check_scoped("admin_server_pin", &auth.peer.rate_key)
    {
        return Err(AppError::rate_limited(retry_after_secs));
    }

    validate_server_pin(&req.new_server_pin)?;

    let stored_hash = state
        .db
        .read(repo::settings::get_server_pin_hash)
        .await?
        .ok_or_else(|| AppError::not_initialized("Server PIN is not configured."))?;

    if !verify_pin(req.current_server_pin, stored_hash).await? {
        if let Some(retry_after_secs) = state
            .rate_limiter
            .record_failure_scoped("admin_server_pin", &auth.peer.rate_key)
        {
            return Err(AppError::rate_limited(retry_after_secs));
        }
        return Err(AppError::unauthorized("Current server PIN is incorrect."));
    }
    state
        .rate_limiter
        .record_success_scoped("admin_server_pin", &auth.peer.rate_key);

    let new_hash = hash_pin(req.new_server_pin).await?;
    let actor_id = auth.user.id.clone();
    let ip_addr = Some(auth.peer.ip_string());

    state
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            repo::settings::set_server_pin_hash(&tx, &new_hash)?;
            repo::settings::insert_audit_log(
                &tx,
                &repo::settings::NewAuditLog {
                    actor_user_id: Some(&actor_id),
                    action: "admin.server_pin.update",
                    target_type: Some("settings"),
                    target_id: Some("server_pin_hash"),
                    detail_json: "{}",
                    ip_addr: ip_addr.as_deref(),
                },
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/admin/settings` (RequireAdmin).
pub async fn get_settings(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
) -> Result<Json<AppSettings>, AppError> {
    let default_paths = state.default_paths.clone();
    let settings = state
        .db
        .read(move |conn| repo::settings::load_app_settings(conn, &default_paths))
        .await?;
    Ok(Json(settings))
}

/// `PATCH /api/admin/settings` (RequireAdmin; toggles mDNS live and broadcasts `WsServerEvent::SettingsChanged`).
pub async fn update_settings(
    RequireAdmin(auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    ApiJson(patch): ApiJson<PartialAppSettings>,
) -> Result<Json<AppSettings>, AppError> {
    validate_settings_patch(&patch)?;

    let default_paths = state.default_paths.clone();
    let actor_id = auth.user.id.clone();
    let ip_addr = Some(auth.peer.ip_string());
    let mdns_changed = patch.mdns_enabled.is_some() || patch.mdns_hostname.is_some();

    let updated = state
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let mut settings = repo::settings::load_app_settings(&tx, &default_paths)?;
            settings.apply_partial(patch);
            repo::settings::save_app_settings(&tx, &settings)?;
            repo::settings::insert_audit_log(
                &tx,
                &repo::settings::NewAuditLog {
                    actor_user_id: Some(&actor_id),
                    action: "admin.settings.update",
                    target_type: Some("settings"),
                    target_id: Some("app_settings"),
                    detail_json: "{}",
                    ip_addr: ip_addr.as_deref(),
                },
            )?;
            tx.commit()?;
            Ok(settings)
        })
        .await?;

    if mdns_changed {
        let state_mdns = Arc::clone(&state);
        let hostname = updated.mdns_hostname.clone();
        let enabled = updated.mdns_enabled;
        let _ = tokio::task::spawn_blocking(move || {
            let mut mdns = state_mdns.mdns.lock();
            mdns.reconfigure(hostname, enabled);
        })
        .await;
    }

    let _ = state.ws_tx.send(WsServerEvent::SettingsChanged {
        settings: updated.clone(),
    });

    Ok(Json(updated))
}

/// `POST /api/admin/backup` (RequireAdmin; executes rolling `VACUUM INTO` snapshot backup).
pub async fn trigger_backup(
    RequireAdmin(auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
) -> Result<Json<BackupResult>, AppError> {
    let default_paths = state.default_paths.clone();
    let settings = state
        .db
        .read(move |conn| repo::settings::load_app_settings(conn, &default_paths))
        .await?;

    let keep_count = usize::try_from(settings.backup_keep_count.max(1)).unwrap_or(7);
    let result = state
        .db
        .backup_to_dir(&settings.backup_dir, keep_count)
        .await?;

    let actor_id = auth.user.id.clone();
    let ip_addr = Some(auth.peer.ip_string());
    let target_path = result.path.to_string_lossy().into_owned();
    state
        .db
        .write(move |conn| {
            repo::settings::insert_audit_log(
                conn,
                &repo::settings::NewAuditLog {
                    actor_user_id: Some(&actor_id),
                    action: "admin.backup",
                    target_type: Some("backup"),
                    target_id: Some(&target_path),
                    detail_json: "{}",
                    ip_addr: ip_addr.as_deref(),
                },
            )?;
            Ok(())
        })
        .await?;

    Ok(Json(result))
}

/// `GET /api/admin/backups` (RequireAdmin; lists rolling `library-*.db` snapshots newest-first).
pub async fn list_backups(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
) -> Result<Json<BackupListResponse>, AppError> {
    let default_paths = state.default_paths.clone();
    let settings = state
        .db
        .read(move |conn| repo::settings::load_app_settings(conn, &default_paths))
        .await?;

    let backup_dir_path = PathBuf::from(&settings.backup_dir);
    let items = tokio::task::spawn_blocking(move || list_backup_snapshots(&backup_dir_path))
        .await
        .map_err(|e| AppError::internal(format!("Failed to scan backup directory: {e}")))?;

    Ok(Json(BackupListResponse {
        items,
        backup_dir: settings.backup_dir,
        interval_hours: settings.backup_interval_hours,
        keep_count: settings.backup_keep_count,
    }))
}

fn validate_settings_patch(patch: &PartialAppSettings) -> Result<(), AppError> {
    if patch.port == Some(0) {
        return Err(AppError::validation("port must be between 1 and 65535."));
    }
    if patch.cache_limit_gb == Some(0) {
        return Err(AppError::validation("cache_limit_gb must be at least 1."));
    }
    if patch.backup_keep_count == Some(0) {
        return Err(AppError::validation(
            "backup_keep_count must be at least 1.",
        ));
    }
    if patch.video_max_frames == Some(0) {
        return Err(AppError::validation("video_max_frames must be at least 1."));
    }
    if let Some(ref host) = patch.mdns_hostname {
        if host.trim().is_empty() {
            return Err(AppError::validation("mdns_hostname cannot be empty."));
        }
    }
    for (label, opt) in [
        ("managed_uploads_dir", &patch.managed_uploads_dir),
        ("thumbnails_dir", &patch.thumbnails_dir),
        ("cache_dir", &patch.cache_dir),
        ("backup_dir", &patch.backup_dir),
    ] {
        if let Some(val) = opt {
            if val.trim().is_empty() {
                return Err(AppError::validation(format!("{label} cannot be empty.")));
            }
        }
    }
    Ok(())
}
