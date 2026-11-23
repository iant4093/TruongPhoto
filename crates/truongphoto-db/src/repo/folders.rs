use crate::error::DbResult;
use crate::rbac_sql;
use crate::repo::{bool_to_int, now_ms};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension, Row};
use truongphoto_core::models::{FolderRecord, FolderView};
use truongphoto_core::rbac::RbacScope;

/// Parameters for creating a new watched folder row.
#[derive(Debug, Clone)]
pub struct CreateFolder {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub volume_uuid: Option<String>,
    pub volume_label: Option<String>,
    pub is_managed_uploads: bool,
    pub allow_disk_delete: bool,
}

/// Optional patch fields for updating an existing watched folder.
#[derive(Debug, Clone, Default)]
pub struct UpdateFolder {
    pub name: Option<String>,
    pub allow_disk_delete: Option<bool>,
    pub is_managed_uploads: Option<bool>,
    pub is_offline: Option<bool>,
    pub volume_uuid: Option<Option<String>>,
    pub volume_label: Option<Option<String>>,
    pub last_scanned_at_ms: Option<i64>,
}

const FOLDER_COLUMNS: &str = "
    id, name, root_path, volume_uuid, volume_label,
    is_managed_uploads, allow_disk_delete, is_offline,
    last_scanned_at_ms, created_at_ms, updated_at_ms
";

fn row_to_folder_record(row: &Row<'_>) -> rusqlite::Result<FolderRecord> {
    Ok(FolderRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        root_path: row.get(2)?,
        volume_uuid: row.get(3)?,
        volume_label: row.get(4)?,
        is_managed_uploads: row.get::<_, i64>(5)? != 0,
        allow_disk_delete: row.get::<_, i64>(6)? != 0,
        is_offline: row.get::<_, i64>(7)? != 0,
        last_scanned_at_ms: row.get(8)?,
        created_at_ms: row.get(9)?,
        updated_at_ms: row.get(10)?,
    })
}

fn row_to_folder_view(row: &Row<'_>) -> rusqlite::Result<FolderView> {
    let count: i64 = row.get(9)?;
    Ok(FolderView {
        id: row.get(0)?,
        name: row.get(1)?,
        root_path: row.get(2)?,
        volume_uuid: row.get(3)?,
        volume_label: row.get(4)?,
        is_managed_uploads: row.get::<_, i64>(5)? != 0,
        allow_disk_delete: row.get::<_, i64>(6)? != 0,
        is_offline: row.get::<_, i64>(7)? != 0,
        last_scanned_at_ms: row.get(8)?,
        asset_count: u64::try_from(count).unwrap_or(0),
        created_at_ms: row.get(10)?,
    })
}

/// Lists watched folders visible under `scope` with active non-hidden asset counts.
pub fn list_folders(conn: &Connection, scope: &RbacScope) -> DbResult<Vec<FolderView>> {
    let mut params: Vec<Value> = Vec::new();
    let clause = rbac_sql::folder_column_clause("f", "id", scope, &mut params);
    let sql = format!(
        "SELECT
            f.id,
            f.name,
            f.root_path,
            f.volume_uuid,
            f.volume_label,
            f.is_managed_uploads,
            f.allow_disk_delete,
            f.is_offline,
            f.last_scanned_at_ms,
            (
                SELECT COUNT(*)
                FROM assets a
                WHERE a.folder_id = f.id
                  AND a.deleted_at_ms IS NULL
                  AND a.is_hidden = 0
            ) AS asset_count,
            f.created_at_ms
         FROM folders f
         WHERE {clause}
         ORDER BY f.is_managed_uploads DESC, f.created_at_ms ASC, f.id ASC"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params), row_to_folder_view)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Fetches a watched folder record by `id`.
pub fn get_folder_by_id(conn: &Connection, folder_id: &str) -> DbResult<Option<FolderRecord>> {
    let sql = format!("SELECT {FOLDER_COLUMNS} FROM folders WHERE id = ?1");
    let folder = conn
        .query_row(&sql, params![folder_id], row_to_folder_record)
        .optional()?;
    Ok(folder)
}

/// Fetches a watched folder view (including active asset count) by `id`.
pub fn get_folder_view_by_id(conn: &Connection, folder_id: &str) -> DbResult<Option<FolderView>> {
    let sql = "
        SELECT
            f.id,
            f.name,
            f.root_path,
            f.volume_uuid,
            f.volume_label,
            f.is_managed_uploads,
            f.allow_disk_delete,
            f.is_offline,
            f.last_scanned_at_ms,
            (
                SELECT COUNT(*)
                FROM assets a
                WHERE a.folder_id = f.id
                  AND a.deleted_at_ms IS NULL
                  AND a.is_hidden = 0
            ) AS asset_count,
            f.created_at_ms
         FROM folders f
         WHERE f.id = ?1
    ";
    let view = conn
        .query_row(sql, params![folder_id], row_to_folder_view)
        .optional()?;
    Ok(view)
}

/// Inserts a new watched folder row.
pub fn create_folder(conn: &Connection, input: &CreateFolder) -> DbResult<FolderRecord> {
    let ts = now_ms();
    conn.execute(
        "INSERT INTO folders (
            id, name, root_path, volume_uuid, volume_label,
            is_managed_uploads, allow_disk_delete, is_offline,
            last_scanned_at_ms, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, NULL, ?8, ?8)",
        params![
            input.id,
            input.name,
            input.root_path,
            input.volume_uuid,
            input.volume_label,
            bool_to_int(input.is_managed_uploads),
            bool_to_int(input.allow_disk_delete),
            ts,
        ],
    )?;

    Ok(FolderRecord {
        id: input.id.clone(),
        name: input.name.clone(),
        root_path: input.root_path.clone(),
        volume_uuid: input.volume_uuid.clone(),
        volume_label: input.volume_label.clone(),
        is_managed_uploads: input.is_managed_uploads,
        allow_disk_delete: input.allow_disk_delete,
        is_offline: false,
        last_scanned_at_ms: None,
        created_at_ms: ts,
        updated_at_ms: ts,
    })
}

/// Updates an existing watched folder and returns the updated `FolderRecord` if found.
pub fn update_folder(
    conn: &Connection,
    folder_id: &str,
    patch: &UpdateFolder,
) -> DbResult<Option<FolderRecord>> {
    let Some(mut existing) = get_folder_by_id(conn, folder_id)? else {
        return Ok(None);
    };

    if let Some(name) = &patch.name {
        existing.name = name.clone();
    }
    if let Some(allow_disk_delete) = patch.allow_disk_delete {
        existing.allow_disk_delete = allow_disk_delete;
    }
    if let Some(is_managed_uploads) = patch.is_managed_uploads {
        existing.is_managed_uploads = is_managed_uploads;
    }
    if let Some(is_offline) = patch.is_offline {
        existing.is_offline = is_offline;
    }
    if let Some(volume_uuid) = &patch.volume_uuid {
        existing.volume_uuid = volume_uuid.clone();
    }
    if let Some(volume_label) = &patch.volume_label {
        existing.volume_label = volume_label.clone();
    }
    if let Some(last_scanned_at_ms) = patch.last_scanned_at_ms {
        existing.last_scanned_at_ms = Some(last_scanned_at_ms);
    }
    existing.updated_at_ms = now_ms();

    conn.execute(
        "UPDATE folders
         SET name = ?1,
             allow_disk_delete = ?2,
             is_managed_uploads = ?3,
             is_offline = ?4,
             volume_uuid = ?5,
             volume_label = ?6,
             last_scanned_at_ms = ?7,
             updated_at_ms = ?8
         WHERE id = ?9",
        params![
            existing.name,
            bool_to_int(existing.allow_disk_delete),
            bool_to_int(existing.is_managed_uploads),
            bool_to_int(existing.is_offline),
            existing.volume_uuid,
            existing.volume_label,
            existing.last_scanned_at_ms,
            existing.updated_at_ms,
            folder_id,
        ],
    )?;

    Ok(Some(existing))
}

/// Deletes a watched folder database entry by `id`, returning true if a row was removed.
pub fn delete_folder(conn: &Connection, folder_id: &str) -> DbResult<bool> {
    let affected = conn.execute("DELETE FROM folders WHERE id = ?1", params![folder_id])?;
    Ok(affected > 0)
}

/// Counts all watched folders in the library.
pub fn count_folders(conn: &Connection) -> DbResult<u64> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM folders", [], |row| row.get(0))?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// Updates `folders.is_offline` for `folder_id`, returning true if a row was updated.
pub fn set_folder_offline(
    conn: &Connection,
    folder_id: &str,
    is_offline: bool,
    now_ms: i64,
) -> DbResult<bool> {
    let affected = conn.execute(
        "UPDATE folders SET is_offline = ?1, updated_at_ms = ?2 WHERE id = ?3",
        params![bool_to_int(is_offline), now_ms, folder_id],
    )?;
    Ok(affected > 0)
}

/// Updates `folders.last_scanned_at_ms` for `folder_id`, returning true if a row was updated.
pub fn set_folder_last_scanned(
    conn: &Connection,
    folder_id: &str,
    scanned_at_ms: i64,
) -> DbResult<bool> {
    let affected = conn.execute(
        "UPDATE folders SET last_scanned_at_ms = ?1, updated_at_ms = ?2 WHERE id = ?3",
        params![scanned_at_ms, now_ms(), folder_id],
    )?;
    Ok(affected > 0)
}

/// Returns the existing managed-uploads folder or registers one at `root_path` if missing,
/// along with `true` if a new folder record was created.
pub fn find_or_create_managed_uploads_folder(
    conn: &Connection,
    root_path: &str,
    volume_uuid: Option<&str>,
    volume_label: Option<&str>,
) -> DbResult<(FolderRecord, bool)> {
    let managed_sql = format!(
        "SELECT {FOLDER_COLUMNS}
         FROM folders
         WHERE is_managed_uploads = 1
         ORDER BY created_at_ms ASC, id ASC
         LIMIT 1"
    );
    if let Some(existing) = conn
        .query_row(&managed_sql, [], row_to_folder_record)
        .optional()?
    {
        return Ok((existing, false));
    }

    let by_path_sql = format!(
        "SELECT {FOLDER_COLUMNS}
         FROM folders
         WHERE root_path = ?1
         LIMIT 1"
    );
    if let Some(existing) = conn
        .query_row(&by_path_sql, params![root_path], row_to_folder_record)
        .optional()?
    {
        let updated = update_folder(
            conn,
            &existing.id,
            &UpdateFolder {
                is_managed_uploads: Some(true),
                volume_uuid: volume_uuid.map(|v| Some(v.to_string())),
                volume_label: volume_label.map(|v| Some(v.to_string())),
                ..Default::default()
            },
        )?;
        return Ok((updated.unwrap_or(existing), false));
    }

    let created = create_folder(
        conn,
        &CreateFolder {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Managed Uploads".to_string(),
            root_path: root_path.to_string(),
            volume_uuid: volume_uuid.map(ToString::to_string),
            volume_label: volume_label.map(ToString::to_string),
            is_managed_uploads: true,
            allow_disk_delete: true,
        },
    )?;
    Ok((created, true))
}
