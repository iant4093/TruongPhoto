use crate::error::DbResult;
use crate::repo::now_ms;
use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use serde::Serialize;
use truongphoto_core::config::{AppSettings, StoragePaths};

pub const KEY_APP_SETTINGS: &str = "app_settings";
pub const KEY_SETUP_COMPLETED: &str = "setup_completed";
pub const KEY_SERVER_PIN_HASH: &str = "server_pin_hash";

/// Input fields for recording an entry in `audit_log`.
#[derive(Debug, Clone)]
pub struct NewAuditLog<'a> {
    pub actor_user_id: Option<&'a str>,
    pub action: &'a str,
    pub target_type: Option<&'a str>,
    pub target_id: Option<&'a str>,
    pub detail_json: &'a str,
    pub ip_addr: Option<&'a str>,
}

/// Reads the raw JSON value stored for `key` in `settings`.
pub fn get_setting_json(conn: &Connection, key: &str) -> DbResult<Option<String>> {
    let value = conn
        .query_row(
            "SELECT value_json FROM settings WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(value)
}

/// Upserts a raw JSON string for `key` in `settings`.
pub fn set_setting_json(conn: &Connection, key: &str, value_json: &str) -> DbResult<()> {
    let ts = now_ms();
    conn.execute(
        "INSERT INTO settings (key, value_json, updated_at_ms)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET
             value_json = excluded.value_json,
             updated_at_ms = excluded.updated_at_ms",
        params![key, value_json, ts],
    )?;
    Ok(())
}

/// Reads and deserializes a typed setting value from `settings`.
pub fn get_setting<T: DeserializeOwned>(conn: &Connection, key: &str) -> DbResult<Option<T>> {
    let Some(raw) = get_setting_json(conn, key)? else {
        return Ok(None);
    };
    let parsed = serde_json::from_str(&raw)?;
    Ok(Some(parsed))
}

/// Serializes and upserts a typed setting value into `settings`.
pub fn set_setting<T: Serialize>(conn: &Connection, key: &str, value: &T) -> DbResult<()> {
    let raw = serde_json::to_string(value)?;
    set_setting_json(conn, key, &raw)
}

/// Loads `AppSettings` from `settings`, falling back to `AppSettings::from_paths(default_paths)`.
pub fn load_app_settings(conn: &Connection, default_paths: &StoragePaths) -> DbResult<AppSettings> {
    Ok(get_setting::<AppSettings>(conn, KEY_APP_SETTINGS)?
        .unwrap_or_else(|| AppSettings::from_paths(default_paths)))
}

/// Persists `AppSettings` into `settings`.
pub fn save_app_settings(conn: &Connection, settings: &AppSettings) -> DbResult<()> {
    set_setting(conn, KEY_APP_SETTINGS, settings)
}

/// Returns whether first-run setup has been completed.
pub fn is_setup_completed(conn: &Connection) -> DbResult<bool> {
    Ok(get_setting::<bool>(conn, KEY_SETUP_COMPLETED)?.unwrap_or(false))
}

/// Persists the `setup_completed` flag.
pub fn set_setup_completed(conn: &Connection, completed: bool) -> DbResult<()> {
    set_setting(conn, KEY_SETUP_COMPLETED, &completed)
}

/// Reads the Argon2id PHC hash of the 6-digit Server PIN, if configured.
pub fn get_server_pin_hash(conn: &Connection) -> DbResult<Option<String>> {
    get_setting::<String>(conn, KEY_SERVER_PIN_HASH)
}

/// Stores the Argon2id PHC hash of the 6-digit Server PIN.
pub fn set_server_pin_hash(conn: &Connection, pin_hash: &str) -> DbResult<()> {
    set_setting(conn, KEY_SERVER_PIN_HASH, &pin_hash)
}

/// Returns whether `desktop_pin_lock` is enabled in persisted `AppSettings` (defaults to `false`).
pub fn get_desktop_pin_lock(conn: &Connection) -> DbResult<bool> {
    Ok(get_setting::<AppSettings>(conn, KEY_APP_SETTINGS)?
        .map(|s| s.desktop_pin_lock)
        .unwrap_or(false))
}

/// Appends a security/admin event to `audit_log` and returns its row ID.
pub fn insert_audit_log(conn: &Connection, entry: &NewAuditLog<'_>) -> DbResult<i64> {
    let ts = now_ms();
    conn.execute(
        "INSERT INTO audit_log (
            actor_user_id, action, target_type, target_id, detail_json, ip_addr, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            entry.actor_user_id,
            entry.action,
            entry.target_type,
            entry.target_id,
            entry.detail_json,
            entry.ip_addr,
            ts,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}
