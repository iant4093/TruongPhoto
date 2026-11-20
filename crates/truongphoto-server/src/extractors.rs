use crate::auth::{hash_session_token, SESSION_COOKIE_NAME};
use crate::error::AppError;
use crate::rate_limit::PeerIdentity;
use crate::state::AppState;
use axum::extract::{ConnectInfo, FromRef, FromRequestParts};
use axum::http::request::Parts;
use axum_extra::extract::CookieJar;
use chrono::Utc;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use truongphoto_core::{RbacScope, SessionRecord, UserRecord};
use truongphoto_db::repo;

const TOUCH_INTERVAL_MS: i64 = 60_000;

/// Extracts `PeerIdentity` from `ConnectInfo<SocketAddr>` and request headers/scheme.
///
/// Works on live TCP sockets (`into_make_service_with_connect_info::<SocketAddr>()`)
/// and in `oneshot` tests with `ConnectInfo<SocketAddr>` extension. If `ConnectInfo`
/// is omitted, defaults fail-closed to `0.0.0.0` (non-loopback).
#[derive(Debug, Clone)]
pub struct PeerIdentityExtractor(pub PeerIdentity);

impl<S> FromRequestParts<S> for PeerIdentityExtractor
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let peer_ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip())
            .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));

        let is_uri_https = parts
            .uri
            .scheme_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("https"));

        Ok(Self(PeerIdentity::from_request(
            peer_ip,
            &parts.headers,
            is_uri_https,
        )))
    }
}

/// Authenticated session context resolved from the `tp_session` HttpOnly cookie.
#[derive(Debug, Clone)]
pub struct AuthSession {
    pub session: SessionRecord,
    pub user: UserRecord,
    pub peer: PeerIdentity,
}

impl<S> FromRequestParts<S> for AuthSession
where
    Arc<AppState>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = Arc::<AppState>::from_ref(state);
        let PeerIdentityExtractor(peer) =
            PeerIdentityExtractor::from_request_parts(parts, state).await?;
        let jar = CookieJar::from_headers(&parts.headers);

        let raw_token = jar
            .get(SESSION_COOKIE_NAME)
            .map(|c| c.value())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| AppError::unauthorized("Authentication required."))?;

        let token_hash = hash_session_token(raw_token);
        let now_ms = Utc::now().timestamp_millis();

        let (session, user) = app_state
            .db
            .read(move |conn| {
                let Some(session) =
                    repo::sessions::find_active_session_by_token_hash(conn, &token_hash, now_ms)?
                else {
                    return Ok(None);
                };
                let Some(user) = repo::users::get_user_by_id(conn, &session.user_id)? else {
                    return Ok(None);
                };
                if user.disabled_at_ms.is_some() {
                    return Ok(None);
                }
                Ok(Some((session, user)))
            })
            .await?
            .ok_or_else(|| AppError::unauthorized("Session is invalid or expired."))?;

        if now_ms.saturating_sub(session.last_seen_at_ms) > TOUCH_INTERVAL_MS {
            let db = app_state.db.clone();
            let sid = session.id.clone();
            tokio::spawn(async move {
                let _ = db
                    .write(move |conn| repo::sessions::touch_session(conn, &sid, now_ms))
                    .await;
            });
        }

        Ok(Self {
            session,
            user,
            peer,
        })
    }
}

/// Extractor enforcing `Role::Admin`.
#[derive(Debug, Clone)]
pub struct RequireAdmin(pub AuthSession);

impl<S> FromRequestParts<S> for RequireAdmin
where
    Arc<AppState>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let auth = AuthSession::from_request_parts(parts, state).await?;
        if !auth.user.role.is_admin() {
            return Err(AppError::forbidden(
                "Administrator privileges are required.",
            ));
        }
        Ok(Self(auth))
    }
}

/// Extractor enforcing `Role::Admin` or `Role::Contributor`.
#[derive(Debug, Clone)]
pub struct RequireContributor(pub AuthSession);

impl<S> FromRequestParts<S> for RequireContributor
where
    Arc<AppState>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let auth = AuthSession::from_request_parts(parts, state).await?;
        if !auth.user.role.can_contribute() {
            return Err(AppError::forbidden(
                "Contributor or Administrator privileges are required.",
            ));
        }
        Ok(Self(auth))
    }
}

/// Authenticated session paired with resolved `RbacScope` (`AdminAll` or `RestrictedFolders`).
#[derive(Debug, Clone)]
pub struct RbacContext {
    pub auth: AuthSession,
    pub scope: RbacScope,
}

impl<S> FromRequestParts<S> for RbacContext
where
    Arc<AppState>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let auth = AuthSession::from_request_parts(parts, state).await?;
        let app_state = Arc::<AppState>::from_ref(state);

        let scope = if auth.user.role.is_admin() {
            RbacScope::AdminAll
        } else {
            let user_id = auth.user.id.clone();
            let role = auth.user.role;
            let allowed_ids = app_state
                .db
                .read(move |conn| repo::users::get_allowed_folder_ids(conn, &user_id))
                .await?;
            RbacScope::from_role_and_folders(role, allowed_ids)
        };

        Ok(Self { auth, scope })
    }
}
