use crate::error::AppError;
use crate::rate_limit::RateKey;
use argon2::{
    password_hash::{phc::PasswordHash, PasswordHasher, PasswordVerifier},
    Algorithm, Argon2, Params, Version,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::Utc;
use cookie::{time::Duration as CookieDuration, Cookie, SameSite};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const SESSION_COOKIE_NAME: &str = "tp_session";
pub const GATE_COOKIE_NAME: &str = "tp_gate";
pub const SESSION_TTL_SECS: i64 = 30 * 24 * 60 * 60;
pub const GATE_TTL_SECS: i64 = 10 * 60;

/// Validates a 6-digit ASCII Server PIN.
pub fn validate_server_pin(pin: &str) -> Result<(), AppError> {
    if pin.len() == 6 && pin.bytes().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(AppError::validation(
            "Server PIN must be exactly 6 numeric digits.",
        ))
    }
}

/// Validates a 4-digit ASCII Profile PIN.
pub fn validate_profile_pin(pin: &str) -> Result<(), AppError> {
    if pin.len() == 4 && pin.bytes().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(AppError::validation(
            "Profile PIN must be exactly 4 numeric digits.",
        ))
    }
}

/// OWASP interactive baseline Argon2id configuration (19 MiB, 2 iterations, 1 lane).
pub fn argon2_instance() -> Argon2<'static> {
    let params = Params::new(19 * 1024, 2, 1, None).expect("valid Argon2id params");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

pub fn hash_pin_blocking(pin: &str) -> Result<String, AppError> {
    let phc = argon2_instance()
        .hash_password(pin.as_bytes())
        .map_err(|e| AppError::internal(format!("argon2 hash failed: {e}")))?
        .to_string();
    Ok(phc)
}

pub fn verify_pin_blocking(pin: &str, stored_phc: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored_phc) else {
        return false;
    };
    Argon2::default()
        .verify_password(pin.as_bytes(), &parsed)
        .is_ok()
}

/// Hashes a PIN off the Tokio worker threads via `spawn_blocking`.
pub async fn hash_pin(pin: String) -> Result<String, AppError> {
    tokio::task::spawn_blocking(move || hash_pin_blocking(&pin))
        .await
        .map_err(|e| AppError::internal(format!("pin hash task failed: {e}")))?
}

/// Verifies a PIN against a stored Argon2id PHC string off the Tokio worker threads.
pub async fn verify_pin(pin: String, stored_phc: String) -> Result<bool, AppError> {
    tokio::task::spawn_blocking(move || verify_pin_blocking(&pin, &stored_phc))
        .await
        .map_err(|e| AppError::internal(format!("pin verify task failed: {e}")))
}

/// Generates a 32-byte CSPRNG session token encoded as base64url without padding.
pub fn generate_random_token() -> Result<String, AppError> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|e| AppError::internal(format!("rng failed: {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// Generates an ephemeral 32-byte process key for `tp_gate` cookie MACs.
pub fn generate_process_key() -> Result<[u8; 32], AppError> {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| AppError::internal(format!("rng failed: {e}")))?;
    Ok(key)
}

/// Computes the SHA-256 hex digest stored in `sessions.token_hash`.
pub fn hash_session_token(raw_token: &str) -> String {
    hex::encode(Sha256::digest(raw_token.as_bytes()))
}

pub fn build_session_cookie(raw_token: &str, secure: bool) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE_NAME, raw_token.to_string()))
        .http_only(true)
        .same_site(SameSite::Strict)
        .path("/")
        .max_age(CookieDuration::seconds(SESSION_TTL_SECS))
        .secure(secure)
        .build()
}

pub fn clear_session_cookie(secure: bool) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE_NAME, String::new()))
        .http_only(true)
        .same_site(SameSite::Strict)
        .path("/")
        .max_age(CookieDuration::seconds(0))
        .secure(secure)
        .build()
}

/// Issues a short-lived (`10m`) BLAKE3-keyed `tp_gate` cookie bound to `rate_key`.
pub fn issue_gate_cookie(gate_key: &[u8; 32], rate_key: &RateKey, secure: bool) -> Cookie<'static> {
    let expires_ms = Utc::now().timestamp_millis() + GATE_TTL_SECS * 1000;
    let payload = format!("{expires_ms}:{}", rate_key.canonical_str());
    let mac = blake3::keyed_hash(gate_key, payload.as_bytes()).to_hex();
    let value = format!("{expires_ms}.{mac}");

    Cookie::build((GATE_COOKIE_NAME, value))
        .http_only(true)
        .same_site(SameSite::Strict)
        .path("/api/auth")
        .max_age(CookieDuration::seconds(GATE_TTL_SECS))
        .secure(secure)
        .build()
}

pub fn clear_gate_cookie(secure: bool) -> Cookie<'static> {
    Cookie::build((GATE_COOKIE_NAME, String::new()))
        .http_only(true)
        .same_site(SameSite::Strict)
        .path("/api/auth")
        .max_age(CookieDuration::seconds(0))
        .secure(secure)
        .build()
}

/// Verifies a `tp_gate` cookie against `gate_key`, `rate_key`, and current timestamp.
pub fn verify_gate_cookie(gate_key: &[u8; 32], rate_key: &RateKey, cookie_val: &str) -> bool {
    let Some((exp_str, mac_hex)) = cookie_val.split_once('.') else {
        return false;
    };
    let Ok(expires_ms) = exp_str.parse::<i64>() else {
        return false;
    };
    if expires_ms <= Utc::now().timestamp_millis() {
        return false;
    }

    let payload = format!("{expires_ms}:{}", rate_key.canonical_str());
    let expected = blake3::keyed_hash(gate_key, payload.as_bytes()).to_hex();
    bool::from(mac_hex.as_bytes().ct_eq(expected.as_str().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn pin_validators_enforce_exact_digit_lengths() {
        assert!(validate_server_pin("123456").is_ok());
        assert!(validate_server_pin("12345").is_err());
        assert!(validate_server_pin("12345a").is_err());

        assert!(validate_profile_pin("1234").is_ok());
        assert!(validate_profile_pin("123").is_err());
        assert!(validate_profile_pin("12ab").is_err());
    }

    #[test]
    fn gate_cookie_binds_to_rate_key() {
        let key = [7u8; 32];
        let peer_a = RateKey::DirectIp(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let peer_b = RateKey::TailscaleUser("bob@ts".to_string());

        let cookie = issue_gate_cookie(&key, &peer_a, false);
        assert!(verify_gate_cookie(&key, &peer_a, cookie.value()));
        assert!(!verify_gate_cookie(&key, &peer_b, cookie.value()));
        assert!(!verify_gate_cookie(&key, &peer_a, "tampered.value"));
    }
}
