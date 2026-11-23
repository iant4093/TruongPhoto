use crate::auth::{
    build_session_cookie, clear_gate_cookie, clear_session_cookie, generate_random_token,
    hash_session_token, issue_gate_cookie, verify_gate_cookie, verify_pin, GATE_COOKIE_NAME,
    SESSION_COOKIE_NAME, SESSION_TTL_SECS,
};
use crate::error::{ApiJson, AppError};
use crate::extractors::{AuthSession, PeerIdentityExtractor, RbacContext};
use crate::state::AppState;
use axum::{
    extract::State,
    http::{header::USER_AGENT, HeaderMap, StatusCode},
    Json,
};
use axum_extra::extract::CookieJar;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use truongphoto_core::{ClientKind, ProfileSummary, UserPublic};
use truongphoto_db::repo;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct AuthStatusResponse {
    pub initialized: bool,
    pub authenticated: bool,
    pub server_gate_passed: bool,
    pub desktop_pin_lock: bool,
    pub user: Option<UserPublic>,
    pub profiles: Option<Vec<ProfileSummary>>,
}

#[derive(Debug, Deserialize)]
pub struct ServerPinRequest {
    pub server_pin: String,
}

#[derive(Debug, Serialize)]
pub struct ServerPinResponse {
    pub profiles: Vec<ProfileSummary>,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub user_id: String,
    pub profile_pin: String,
    #[serde(default)]
    pub server_pin: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AuthLoginResponse {
    pub user: UserPublic,
    pub session_expires_at_ms: i64,
}

#[derive(Debug, Deserialize)]
pub struct DesktopBootstrapRequest {
    pub token: String,
}

#[derive(Debug, Serialize)]
pub struct MeResponse {
    pub user: UserPublic,
    pub allowed_folder_ids: Option<Vec<String>>,
}

/// `GET /api/auth/status` (Public).
pub async fn auth_status(
    PeerIdentityExtractor(peer): PeerIdentityExtractor,
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
) -> Result<Json<AuthStatusResponse>, AppError> {
    let raw_session = jar
        .get(SESSION_COOKIE_NAME)
        .map(|c| c.value().to_string())
        .filter(|s| !s.is_empty());
    let session_hash = raw_session.as_deref().map(hash_session_token);
    let now_ms = Utc::now().timestamp_millis();

    let gate_passed = jar
        .get(GATE_COOKIE_NAME)
        .map(|c| verify_gate_cookie(&state.gate_key, &peer.rate_key, c.value()))
        .unwrap_or(false);

    let (initialized, desktop_pin_lock, auth_user, profiles) = state
        .db
        .read(move |conn| {
            let initialized = repo::settings::is_setup_completed(conn)?;
            let desktop_pin_lock = repo::settings::get_desktop_pin_lock(conn)?;
            if !initialized {
                return Ok((false, desktop_pin_lock, None, None));
            }

            let mut authed_user = None;
            if let Some(ref hash) = session_hash {
                if let Some(session) =
                    repo::sessions::find_active_session_by_token_hash(conn, hash, now_ms)?
                {
                    if let Some(user) = repo::users::get_user_by_id(conn, &session.user_id)? {
                        if user.disabled_at_ms.is_none() {
                            authed_user = Some(UserPublic::from(user));
                        }
                    }
                }
            }

            let profiles = if authed_user.is_none() && gate_passed {
                Some(repo::users::list_active_profiles(conn)?)
            } else {
                None
            };

            Ok((true, desktop_pin_lock, authed_user, profiles))
        })
        .await?;

    let authenticated = auth_user.is_some();
    Ok(Json(AuthStatusResponse {
        initialized,
        authenticated,
        server_gate_passed: authenticated || gate_passed,
        desktop_pin_lock,
        user: auth_user,
        profiles,
    }))
}

/// `POST /api/auth/server-pin` (Public, rate-limited).
pub async fn server_pin(
    PeerIdentityExtractor(peer): PeerIdentityExtractor,
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    ApiJson(req): ApiJson<ServerPinRequest>,
) -> Result<(CookieJar, Json<ServerPinResponse>), AppError> {
    if let Err(retry_after_secs) = state
        .rate_limiter
        .check_scoped("server_pin", &peer.rate_key)
    {
        return Err(AppError::rate_limited(retry_after_secs));
    }

    let (initialized, server_pin_hash) = state
        .db
        .read(|conn| {
            let initialized = repo::settings::is_setup_completed(conn)?;
            let hash = repo::settings::get_server_pin_hash(conn)?;
            Ok((initialized, hash))
        })
        .await?;

    if !initialized {
        return Err(AppError::not_initialized(
            "TruongPhoto has not been initialized yet.",
        ));
    }

    let Some(stored_hash) = server_pin_hash else {
        return Err(AppError::internal("Server PIN hash is missing."));
    };

    let valid = verify_pin(req.server_pin, stored_hash).await?;
    if !valid {
        if let Some(retry_after_secs) = state
            .rate_limiter
            .record_failure_scoped("server_pin", &peer.rate_key)
        {
            return Err(AppError::rate_limited(retry_after_secs));
        }
        return Err(AppError::unauthorized("Invalid server PIN."));
    }

    state
        .rate_limiter
        .record_success_scoped("server_pin", &peer.rate_key);
    let profiles = state.db.read(repo::users::list_active_profiles).await?;
    let gate_cookie = issue_gate_cookie(&state.gate_key, &peer.rate_key, peer.is_https);

    Ok((jar.add(gate_cookie), Json(ServerPinResponse { profiles })))
}

/// `POST /api/auth/login` (Requires valid `tp_gate` cookie OR inline `server_pin`; rate-limited).
pub async fn login(
    PeerIdentityExtractor(peer): PeerIdentityExtractor,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    ApiJson(req): ApiJson<LoginRequest>,
) -> Result<(CookieJar, Json<AuthLoginResponse>), AppError> {
    let target_user_id = req.user_id.trim().to_string();
    if target_user_id.is_empty() || target_user_id.len() > 128 {
        return Err(AppError::validation("Invalid user_id."));
    }
    let user_scope = format!("login_user:{target_user_id}");
    if let Err(retry_after_secs) = state.rate_limiter.check_scoped("login", &peer.rate_key) {
        return Err(AppError::rate_limited(retry_after_secs));
    }
    if let Err(retry_after_secs) = state.rate_limiter.check_scoped(&user_scope, &peer.rate_key) {
        return Err(AppError::rate_limited(retry_after_secs));
    }

    let has_valid_gate = jar
        .get(GATE_COOKIE_NAME)
        .map(|c| verify_gate_cookie(&state.gate_key, &peer.rate_key, c.value()))
        .unwrap_or(false);

    let target_user_id_db = target_user_id.clone();
    let (initialized, server_pin_hash, user_opt) = state
        .db
        .read(move |conn| {
            let initialized = repo::settings::is_setup_completed(conn)?;
            let server_hash = repo::settings::get_server_pin_hash(conn)?;
            let user = repo::users::get_user_by_id(conn, &target_user_id_db)?;
            Ok((initialized, server_hash, user))
        })
        .await?;

    if !initialized {
        return Err(AppError::not_initialized(
            "TruongPhoto has not been initialized yet.",
        ));
    }

    if !has_valid_gate {
        if let Err(retry_after_secs) = state
            .rate_limiter
            .check_scoped("server_pin", &peer.rate_key)
        {
            return Err(AppError::rate_limited(retry_after_secs));
        }
        let Some(inline_server_pin) = req.server_pin else {
            return Err(AppError::unauthorized(
                "Server PIN verification is required before profile login.",
            ));
        };
        let Some(stored_server_hash) = server_pin_hash else {
            return Err(AppError::internal("Server PIN hash is missing."));
        };
        if !verify_pin(inline_server_pin, stored_server_hash).await? {
            if let Some(retry_after_secs) = state
                .rate_limiter
                .record_failure_scoped("server_pin", &peer.rate_key)
            {
                return Err(AppError::rate_limited(retry_after_secs));
            }
            return Err(AppError::unauthorized("Invalid server PIN."));
        }
        state
            .rate_limiter
            .record_success_scoped("server_pin", &peer.rate_key);
    }

    let Some(user) = user_opt.filter(|u| u.disabled_at_ms.is_none()) else {
        let r1 = state
            .rate_limiter
            .record_failure_scoped("login", &peer.rate_key);
        let r2 = state
            .rate_limiter
            .record_failure_scoped(&user_scope, &peer.rate_key);
        if let Some(retry_after_secs) = r1.max(r2) {
            return Err(AppError::rate_limited(retry_after_secs));
        }
        return Err(AppError::unauthorized("Invalid profile or PIN."));
    };

    if !verify_pin(req.profile_pin, user.pin_hash.clone()).await? {
        let r1 = state
            .rate_limiter
            .record_failure_scoped("login", &peer.rate_key);
        let r2 = state
            .rate_limiter
            .record_failure_scoped(&user_scope, &peer.rate_key);
        if let Some(retry_after_secs) = r1.max(r2) {
            return Err(AppError::rate_limited(retry_after_secs));
        }
        return Err(AppError::unauthorized("Invalid profile or PIN."));
    }

    state
        .rate_limiter
        .record_success_scoped("login", &peer.rate_key);
    state
        .rate_limiter
        .record_success_scoped(&user_scope, &peer.rate_key);

    let raw_token = generate_random_token()?;
    let token_hash = hash_session_token(&raw_token);
    let session_id = Uuid::new_v4().to_string();
    let now_ms = Utc::now().timestamp_millis();
    let session_expires_at_ms = now_ms + SESSION_TTL_SECS * 1000;
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let ip_addr = Some(peer.ip_string());
    let client_kind = peer.client_kind();
    let user_id_for_db = user.id.clone();

    state
        .db
        .write(move |conn| {
            repo::sessions::create_session(
                conn,
                &repo::sessions::CreateSession {
                    id: session_id,
                    token_hash,
                    user_id: user_id_for_db.clone(),
                    client_kind,
                    user_agent,
                    ip_addr: ip_addr.clone(),
                    expires_at_ms: session_expires_at_ms,
                },
            )?;
            repo::settings::insert_audit_log(
                conn,
                &repo::settings::NewAuditLog {
                    actor_user_id: Some(&user_id_for_db),
                    action: "auth.login",
                    target_type: Some("user"),
                    target_id: Some(&user_id_for_db),
                    detail_json: "{}",
                    ip_addr: ip_addr.as_deref(),
                },
            )?;
            Ok(())
        })
        .await?;

    let jar = jar
        .add(clear_gate_cookie(peer.is_https))
        .add(build_session_cookie(&raw_token, peer.is_https));

    Ok((
        jar,
        Json(AuthLoginResponse {
            user: UserPublic::from(user),
            session_expires_at_ms,
        }),
    ))
}

/// `POST /api/auth/desktop-bootstrap` (Enforces all 4 Section 5.4 security checks in order).
pub async fn desktop_bootstrap(
    PeerIdentityExtractor(peer): PeerIdentityExtractor,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    ApiJson(req): ApiJson<DesktopBootstrapRequest>,
) -> Result<(CookieJar, Json<AuthLoginResponse>), AppError> {
    // Check 1: direct loopback TCP peer with NO Tailscale-User-Login or X-Forwarded-For headers.
    if !peer.is_direct_loopback {
        return Err(AppError::forbidden(
            "Desktop bootstrap is restricted to direct loopback connections.",
        ));
    }

    let (initialized, desktop_pin_lock, admin_opt) = state
        .db
        .read(|conn| {
            let initialized = repo::settings::is_setup_completed(conn)?;
            let pin_lock = repo::settings::get_desktop_pin_lock(conn)?;
            let admin = repo::users::get_primary_active_admin(conn)?;
            Ok((initialized, pin_lock, admin))
        })
        .await?;

    // Check 2: setup_completed == true.
    if !initialized {
        return Err(AppError::not_initialized(
            "Initial setup has not been completed.",
        ));
    }

    // Check 3: desktop_pin_lock != true.
    if desktop_pin_lock {
        return Err(AppError::desktop_pin_locked("Desktop PIN lock is enabled."));
    }

    // Check 4: constant-time token comparison against process RAM secret.
    let expected = state
        .desktop_secret
        .as_deref()
        .ok_or_else(|| AppError::unauthorized("Desktop bootstrap secret is not configured."))?;
    if !bool::from(req.token.as_bytes().ct_eq(expected.as_bytes())) {
        return Err(AppError::unauthorized("Invalid desktop bootstrap token."));
    }

    let admin = admin_opt
        .ok_or_else(|| AppError::unauthorized("No active administrator account exists."))?;

    let raw_token = generate_random_token()?;
    let token_hash = hash_session_token(&raw_token);
    let session_id = Uuid::new_v4().to_string();
    let now_ms = Utc::now().timestamp_millis();
    let session_expires_at_ms = now_ms + SESSION_TTL_SECS * 1000;
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let ip_addr = Some(peer.ip_string());
    let admin_id = admin.id.clone();

    state
        .db
        .write(move |conn| {
            repo::sessions::create_session(
                conn,
                &repo::sessions::CreateSession {
                    id: session_id,
                    token_hash,
                    user_id: admin_id.clone(),
                    client_kind: ClientKind::TauriDesktop,
                    user_agent,
                    ip_addr: ip_addr.clone(),
                    expires_at_ms: session_expires_at_ms,
                },
            )?;
            repo::settings::insert_audit_log(
                conn,
                &repo::settings::NewAuditLog {
                    actor_user_id: Some(&admin_id),
                    action: "auth.desktop_bootstrap",
                    target_type: Some("user"),
                    target_id: Some(&admin_id),
                    detail_json: "{}",
                    ip_addr: ip_addr.as_deref(),
                },
            )?;
            Ok(())
        })
        .await?;

    let jar = jar.add(build_session_cookie(&raw_token, peer.is_https));

    Ok((
        jar,
        Json(AuthLoginResponse {
            user: UserPublic::from(admin),
            session_expires_at_ms,
        }),
    ))
}

/// `POST /api/auth/logout` (Authenticated).
pub async fn logout(
    auth: AuthSession,
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
) -> Result<(StatusCode, CookieJar), AppError> {
    let now_ms = Utc::now().timestamp_millis();
    let session_id = auth.session.id.clone();
    let user_id = auth.user.id.clone();
    let ip_addr = Some(auth.peer.ip_string());

    state
        .db
        .write(move |conn| {
            repo::sessions::revoke_session_by_id(conn, &session_id, now_ms)?;
            repo::settings::insert_audit_log(
                conn,
                &repo::settings::NewAuditLog {
                    actor_user_id: Some(&user_id),
                    action: "auth.logout",
                    target_type: Some("session"),
                    target_id: Some(&session_id),
                    detail_json: "{}",
                    ip_addr: ip_addr.as_deref(),
                },
            )?;
            Ok(())
        })
        .await?;

    Ok((
        StatusCode::NO_CONTENT,
        jar.add(clear_session_cookie(auth.peer.is_https)),
    ))
}

/// `GET /api/auth/me` (Authenticated + RbacContext).
pub async fn me(rbac: RbacContext) -> Json<MeResponse> {
    let allowed_folder_ids = rbac.scope.allowed_folder_ids().map(|ids| ids.to_vec());
    Json(MeResponse {
        user: UserPublic::from(rbac.auth.user),
        allowed_folder_ids,
    })
}
