use crate::error::DbResult;
use crate::repo::{now_ms, to_sql_err};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::collections::{HashMap, HashSet};
use truongphoto_core::models::{AdminUserView, ProfileSummary, UserRecord};
use truongphoto_core::rbac::Role;

/// Parameters for creating a new user row in `users`.
#[derive(Debug, Clone)]
pub struct CreateUser {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub pin_hash: String,
    pub avatar_color: String,
}

/// Optional fields for updating an existing user row in `users`.
#[derive(Debug, Clone, Default)]
pub struct UpdateUser {
    pub display_name: Option<String>,
    pub role: Option<Role>,
    pub pin_hash: Option<String>,
    pub avatar_color: Option<String>,
    pub disabled: Option<bool>,
}

const USER_COLUMNS: &str = "
    id, username, display_name, role, pin_hash, avatar_color,
    disabled_at_ms, created_at_ms, updated_at_ms
";

fn row_to_user_record(row: &Row<'_>) -> rusqlite::Result<UserRecord> {
    let role_str: String = row.get(3)?;
    let role = role_str.parse::<Role>().map_err(|e| to_sql_err(e, 3))?;
    Ok(UserRecord {
        id: row.get(0)?,
        username: row.get(1)?,
        display_name: row.get(2)?,
        role,
        pin_hash: row.get(4)?,
        avatar_color: row.get(5)?,
        disabled_at_ms: row.get(6)?,
        created_at_ms: row.get(7)?,
        updated_at_ms: row.get(8)?,
    })
}

/// Inserts a new user into `users`.
pub fn create_user(conn: &Connection, input: &CreateUser) -> DbResult<UserRecord> {
    let ts = now_ms();
    conn.execute(
        "INSERT INTO users (
            id, username, display_name, role, pin_hash, avatar_color,
            disabled_at_ms, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?7)",
        params![
            input.id,
            input.username,
            input.display_name,
            input.role.as_str(),
            input.pin_hash,
            input.avatar_color,
            ts,
        ],
    )?;

    Ok(UserRecord {
        id: input.id.clone(),
        username: input.username.clone(),
        display_name: input.display_name.clone(),
        role: input.role,
        pin_hash: input.pin_hash.clone(),
        avatar_color: input.avatar_color.clone(),
        disabled_at_ms: None,
        created_at_ms: ts,
        updated_at_ms: ts,
    })
}

/// Fetches a user by primary key `id`.
pub fn get_user_by_id(conn: &Connection, user_id: &str) -> DbResult<Option<UserRecord>> {
    let sql = format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?1");
    let user = conn
        .query_row(&sql, params![user_id], row_to_user_record)
        .optional()?;
    Ok(user)
}

/// Fetches a user by case-insensitive `username`.
pub fn get_user_by_username(conn: &Connection, username: &str) -> DbResult<Option<UserRecord>> {
    let sql = format!("SELECT {USER_COLUMNS} FROM users WHERE username = ?1");
    let user = conn
        .query_row(&sql, params![username], row_to_user_record)
        .optional()?;
    Ok(user)
}

/// Returns the oldest active (non-disabled) administrator account, used for desktop auto-auth.
pub fn get_primary_active_admin(conn: &Connection) -> DbResult<Option<UserRecord>> {
    let sql = format!(
        "SELECT {USER_COLUMNS}
         FROM users
         WHERE role = 'admin' AND disabled_at_ms IS NULL
         ORDER BY created_at_ms ASC, id ASC
         LIMIT 1"
    );
    let user = conn.query_row(&sql, [], row_to_user_record).optional()?;
    Ok(user)
}

/// Lists all users along with their granted `user_folder_acls` folder IDs.
pub fn list_users_with_acls(conn: &Connection) -> DbResult<Vec<AdminUserView>> {
    let sql = format!(
        "SELECT {USER_COLUMNS}
         FROM users
         ORDER BY created_at_ms ASC, id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let users = stmt
        .query_map([], row_to_user_record)?
        .collect::<Result<Vec<_>, _>>()?;

    let mut acl_stmt = conn.prepare(
        "SELECT user_id, folder_id
         FROM user_folder_acls
         ORDER BY folder_id ASC",
    )?;
    let mut acl_map: HashMap<String, Vec<String>> = HashMap::new();
    let acl_rows = acl_stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in acl_rows {
        let (uid, fid) = row?;
        acl_map.entry(uid).or_default().push(fid);
    }

    Ok(users
        .into_iter()
        .map(|u| {
            let allowed_folder_ids = acl_map.remove(&u.id).unwrap_or_default();
            AdminUserView {
                id: u.id,
                username: u.username,
                display_name: u.display_name,
                role: u.role,
                avatar_color: u.avatar_color,
                disabled_at_ms: u.disabled_at_ms,
                allowed_folder_ids,
                created_at_ms: u.created_at_ms,
            }
        })
        .collect())
}

/// Lists active (non-disabled) user profiles for step-2 login selection.
pub fn list_active_profiles(conn: &Connection) -> DbResult<Vec<ProfileSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, display_name, role, avatar_color
         FROM users
         WHERE disabled_at_ms IS NULL
         ORDER BY created_at_ms ASC, id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        let role_str: String = row.get(2)?;
        let role = role_str.parse::<Role>().map_err(|e| to_sql_err(e, 2))?;
        Ok(ProfileSummary {
            id: row.get(0)?,
            display_name: row.get(1)?,
            role,
            avatar_color: row.get(3)?,
        })
    })?;

    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Updates mutable fields on an existing user and returns the updated record if found.
pub fn update_user(
    conn: &Connection,
    user_id: &str,
    patch: &UpdateUser,
) -> DbResult<Option<UserRecord>> {
    let Some(mut existing) = get_user_by_id(conn, user_id)? else {
        return Ok(None);
    };

    let ts = now_ms();
    if let Some(display_name) = &patch.display_name {
        existing.display_name = display_name.clone();
    }
    if let Some(role) = patch.role {
        existing.role = role;
    }
    if let Some(pin_hash) = &patch.pin_hash {
        existing.pin_hash = pin_hash.clone();
    }
    if let Some(avatar_color) = &patch.avatar_color {
        existing.avatar_color = avatar_color.clone();
    }
    if let Some(disabled) = patch.disabled {
        existing.disabled_at_ms = if disabled {
            Some(existing.disabled_at_ms.unwrap_or(ts))
        } else {
            None
        };
    }
    existing.updated_at_ms = ts;

    conn.execute(
        "UPDATE users
         SET display_name = ?1,
             role = ?2,
             pin_hash = ?3,
             avatar_color = ?4,
             disabled_at_ms = ?5,
             updated_at_ms = ?6
         WHERE id = ?7",
        params![
            existing.display_name,
            existing.role.as_str(),
            existing.pin_hash,
            existing.avatar_color,
            existing.disabled_at_ms,
            existing.updated_at_ms,
            user_id,
        ],
    )?;

    Ok(Some(existing))
}

/// Deletes a user by ID, returning true if a row was removed.
pub fn delete_user(conn: &Connection, user_id: &str) -> DbResult<bool> {
    let affected = conn.execute("DELETE FROM users WHERE id = ?1", params![user_id])?;
    Ok(affected > 0)
}

/// Counts active (non-disabled) users with the `admin` role.
pub fn count_active_admins(conn: &Connection) -> DbResult<u64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM users WHERE role = 'admin' AND disabled_at_ms IS NULL",
        [],
        |row| row.get(0),
    )?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// Returns sorted folder IDs explicitly granted to `user_id` in `user_folder_acls`.
pub fn get_allowed_folder_ids(conn: &Connection, user_id: &str) -> DbResult<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT folder_id
         FROM user_folder_acls
         WHERE user_id = ?1
         ORDER BY folder_id ASC",
    )?;
    let rows = stmt.query_map(params![user_id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Atomically replaces all `user_folder_acls` entries for `user_id`.
pub fn replace_user_folder_acls(
    conn: &Connection,
    user_id: &str,
    folder_ids: &[String],
) -> DbResult<()> {
    conn.execute_batch("SAVEPOINT replace_user_folder_acls;")?;
    let res = (|| -> DbResult<()> {
        conn.execute(
            "DELETE FROM user_folder_acls WHERE user_id = ?1",
            params![user_id],
        )?;

        let ts = now_ms();
        let mut seen = HashSet::with_capacity(folder_ids.len());
        let mut stmt = conn.prepare(
            "INSERT INTO user_folder_acls (user_id, folder_id, granted_at_ms)
             VALUES (?1, ?2, ?3)",
        )?;
        for folder_id in folder_ids {
            if seen.insert(folder_id.as_str()) {
                stmt.execute(params![user_id, folder_id, ts])?;
            }
        }
        Ok(())
    })();

    match res {
        Ok(()) => {
            conn.execute_batch("RELEASE SAVEPOINT replace_user_folder_acls;")?;
            Ok(())
        }
        Err(err) => {
            let _ = conn.execute_batch(
                "ROLLBACK TO SAVEPOINT replace_user_folder_acls; RELEASE SAVEPOINT replace_user_folder_acls;",
            );
            Err(err)
        }
    }
}
