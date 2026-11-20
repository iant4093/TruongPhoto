use serde::{Deserialize, Serialize};
use std::fmt;

/// Core library errors shared across TruongPhoto workspace crates.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("validation error: {0}")]
    Validation(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("invalid role: {0}")]
    InvalidRole(String),

    #[error("invalid enum value: {0}")]
    InvalidEnum(String),
}

/// Standard API error codes returned inside `ApiErrorEnvelope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApiErrorCode {
    Unauthorized,
    Forbidden,
    DesktopPinLocked,
    NotFound,
    AlreadyInitialized,
    NotInitialized,
    Conflict,
    ValidationError,
    RateLimited,
    InternalError,
}

impl ApiErrorCode {
    /// Returns the wire-format screaming snake-case identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Forbidden => "FORBIDDEN",
            Self::DesktopPinLocked => "DESKTOP_PIN_LOCKED",
            Self::NotFound => "NOT_FOUND",
            Self::AlreadyInitialized => "ALREADY_INITIALIZED",
            Self::NotInitialized => "NOT_INITIALIZED",
            Self::Conflict => "CONFLICT",
            Self::ValidationError => "VALIDATION_ERROR",
            Self::RateLimited => "RATE_LIMITED",
            Self::InternalError => "INTERNAL_ERROR",
        }
    }

    /// Returns the canonical HTTP status code associated with this error code.
    pub const fn http_status(self) -> u16 {
        match self {
            Self::Unauthorized => 401,
            Self::Forbidden | Self::DesktopPinLocked => 403,
            Self::NotFound => 404,
            Self::AlreadyInitialized | Self::NotInitialized | Self::Conflict => 409,
            Self::ValidationError => 422,
            Self::RateLimited => 429,
            Self::InternalError => 500,
        }
    }
}

impl fmt::Display for ApiErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Inner body of a unified API error response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorBody {
    pub code: ApiErrorCode,
    pub message: String,
    #[serde(default)]
    pub details: Option<serde_json::Value>,
}

/// Unified JSON error envelope for all `/api/*` 4xx/5xx responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorEnvelope {
    pub error: ApiErrorBody,
}

impl ApiErrorEnvelope {
    pub fn new(code: ApiErrorCode, message: impl Into<String>) -> Self {
        Self {
            error: ApiErrorBody {
                code,
                message: message.into(),
                details: None,
            },
        }
    }

    pub fn with_details(
        code: ApiErrorCode,
        message: impl Into<String>,
        details: serde_json::Value,
    ) -> Self {
        Self {
            error: ApiErrorBody {
                code,
                message: message.into(),
                details: Some(details),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_error_codes_match_expected_strings_and_statuses() {
        let cases = [
            (ApiErrorCode::Unauthorized, "UNAUTHORIZED", 401),
            (ApiErrorCode::Forbidden, "FORBIDDEN", 403),
            (ApiErrorCode::DesktopPinLocked, "DESKTOP_PIN_LOCKED", 403),
            (ApiErrorCode::NotFound, "NOT_FOUND", 404),
            (ApiErrorCode::AlreadyInitialized, "ALREADY_INITIALIZED", 409),
            (ApiErrorCode::NotInitialized, "NOT_INITIALIZED", 409),
            (ApiErrorCode::Conflict, "CONFLICT", 409),
            (ApiErrorCode::ValidationError, "VALIDATION_ERROR", 422),
            (ApiErrorCode::RateLimited, "RATE_LIMITED", 429),
            (ApiErrorCode::InternalError, "INTERNAL_ERROR", 500),
        ];

        for (code, expected_str, expected_status) in cases {
            assert_eq!(code.as_str(), expected_str);
            assert_eq!(code.http_status(), expected_status);
            let json = serde_json::to_string(&code).expect("serialize error code");
            assert_eq!(json, format!("\"{expected_str}\""));
        }
    }

    #[test]
    fn api_error_envelope_serializes_expected_shape() {
        let env = ApiErrorEnvelope::new(
            ApiErrorCode::Forbidden,
            "Administrator privileges are required.",
        );
        let value = serde_json::to_value(&env).expect("serialize envelope");
        assert_eq!(value["error"]["code"], "FORBIDDEN");
        assert_eq!(
            value["error"]["message"],
            "Administrator privileges are required."
        );
        assert!(value["error"]["details"].is_null());
    }
}
