use axum::{
    extract::{rejection::JsonRejection, FromRequest, Request},
    http::{header::RETRY_AFTER, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use truongphoto_core::{ApiErrorCode, ApiErrorEnvelope};
use truongphoto_db::DbError;

/// Unified server error type rendered as Section 6.1 JSON envelopes.
#[derive(Debug)]
pub struct AppError {
    pub code: ApiErrorCode,
    pub message: String,
    pub details: Option<Value>,
    pub retry_after_secs: Option<u64>,
}

impl AppError {
    pub fn new(code: ApiErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
            retry_after_secs: None,
        }
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::Unauthorized, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::Forbidden, message)
    }

    pub fn desktop_pin_locked(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::DesktopPinLocked, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::NotFound, message)
    }

    pub fn already_initialized(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::AlreadyInitialized, message)
    }

    pub fn not_initialized(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::NotInitialized, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::Conflict, message)
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::ValidationError, message)
    }

    pub fn validation_with_details(message: impl Into<String>, details: Value) -> Self {
        Self {
            code: ApiErrorCode::ValidationError,
            message: message.into(),
            details: Some(details),
            retry_after_secs: None,
        }
    }

    pub fn rate_limited(retry_after_secs: u64) -> Self {
        let secs = retry_after_secs.max(1);
        Self {
            code: ApiErrorCode::RateLimited,
            message: "Too many failed attempts".to_string(),
            details: Some(json!({ "retry_after_secs": secs })),
            retry_after_secs: Some(secs),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::InternalError, message)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.code.http_status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

        let envelope = match self.details {
            Some(details) => ApiErrorEnvelope::with_details(self.code, self.message, details),
            None => ApiErrorEnvelope::new(self.code, self.message),
        };

        let mut response = (status, Json(envelope)).into_response();
        if let Some(secs) = self.retry_after_secs {
            if let Ok(val) = HeaderValue::from_str(&secs.to_string()) {
                response.headers_mut().insert(RETRY_AFTER, val);
            }
        }
        response
    }
}

impl From<DbError> for AppError {
    fn from(err: DbError) -> Self {
        if err.is_unique_violation() {
            return Self::conflict("A record with that unique value already exists.");
        }
        if err.is_foreign_key_violation() {
            return Self::validation("Referenced resource does not exist.");
        }
        match err {
            DbError::NotFound(msg) => Self::not_found(msg),
            DbError::Conflict(msg) => Self::conflict(msg),
            DbError::InvalidArgument(msg) => Self::validation(msg),
            other => {
                tracing::error!(error = %other, "database error");
                Self::internal("Database operation failed.")
            }
        }
    }
}

/// JSON request extractor that returns a unified `VALIDATION_ERROR` JSON envelope on parse errors.
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(map_json_rejection(rejection)),
        }
    }
}

fn map_json_rejection(rejection: JsonRejection) -> AppError {
    AppError::validation(rejection.body_text())
}
