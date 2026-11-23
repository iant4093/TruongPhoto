use crate::error::DbResult;
use crate::repo::{to_sql_err, with_savepoint};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension, Row};
use truongphoto_core::models::{CacheEntryKind, LruCacheEntry};

const CACHE_COLUMNS: &str = "
    cache_key, asset_id, kind, rel_path, size_bytes, last_accessed_ms, created_at_ms
";

fn row_to_cache_entry(row: &Row<'_>) -> rusqlite::Result<LruCacheEntry> {
    let kind_str: String = row.get(2)?;
    let kind = kind_str
        .parse::<CacheEntryKind>()
        .map_err(|e| to_sql_err(e, 2))?;
    let size_bytes: i64 = row.get(4)?;
    Ok(LruCacheEntry {
        cache_key: row.get(0)?,
        asset_id: row.get(1)?,
        kind,
        rel_path: row.get(3)?,
        size_bytes: u64::try_from(size_bytes).unwrap_or(0),
        last_accessed_ms: row.get(5)?,
        created_at_ms: row.get(6)?,
    })
}

/// Inserts or updates an `lru_cache_entries` record.
pub fn upsert_cache_entry(conn: &Connection, entry: &LruCacheEntry) -> DbResult<()> {
    let size_i64 = i64::try_from(entry.size_bytes).unwrap_or(i64::MAX);
    conn.execute(
        "INSERT INTO lru_cache_entries (
            cache_key, asset_id, kind, rel_path, size_bytes, last_accessed_ms, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(cache_key) DO UPDATE SET
            asset_id = excluded.asset_id,
            kind = excluded.kind,
            rel_path = excluded.rel_path,
            size_bytes = excluded.size_bytes,
            last_accessed_ms = excluded.last_accessed_ms",
        params![
            entry.cache_key,
            entry.asset_id,
            entry.kind.as_str(),
            entry.rel_path,
            size_i64,
            entry.last_accessed_ms,
            entry.created_at_ms,
        ],
    )?;
    Ok(())
}

/// Updates `last_accessed_ms` on a cached entry for LRU tracking.
pub fn touch_cache_entry(conn: &Connection, cache_key: &str, now_ms: i64) -> DbResult<()> {
    conn.execute(
        "UPDATE lru_cache_entries SET last_accessed_ms = ?1 WHERE cache_key = ?2",
        params![now_ms, cache_key],
    )?;
    Ok(())
}

/// Fetches a single cache entry by `cache_key`.
pub fn get_cache_entry(conn: &Connection, cache_key: &str) -> DbResult<Option<LruCacheEntry>> {
    let sql = format!("SELECT {CACHE_COLUMNS} FROM lru_cache_entries WHERE cache_key = ?1");
    let entry = conn
        .query_row(&sql, params![cache_key], row_to_cache_entry)
        .optional()?;
    Ok(entry)
}

/// Returns the total tracked size in bytes across all `lru_cache_entries`.
pub fn total_cache_bytes(conn: &Connection) -> DbResult<u64> {
    let total: i64 = conn.query_row(
        "SELECT COALESCE(SUM(size_bytes), 0) FROM lru_cache_entries",
        [],
        |row| row.get(0),
    )?;
    Ok(u64::try_from(total).unwrap_or(0))
}

/// Pops and deletes the oldest cache entries until at least `bytes_to_free` bytes are reclaimed.
///
/// Entries are ordered oldest-first by `(last_accessed_ms ASC, created_at_ms ASC, cache_key ASC)`.
/// When `exclude_key` is provided, that key is never evicted (preventing self-eviction of a
/// newly inserted cache entry whose size exceeds the configured cap).
pub fn pop_oldest_entries_to_free(
    conn: &Connection,
    bytes_to_free: u64,
    exclude_key: Option<&str>,
) -> DbResult<Vec<LruCacheEntry>> {
    if bytes_to_free == 0 {
        return Ok(Vec::new());
    }

    with_savepoint(conn, "pop_oldest_cache", || {
        let sql = format!(
            "SELECT {CACHE_COLUMNS}
             FROM lru_cache_entries
             ORDER BY last_accessed_ms ASC, created_at_ms ASC, cache_key ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_cache_entry)?;

        let mut victims = Vec::new();
        let mut freed: u64 = 0;
        for row in rows {
            let entry = row?;
            if exclude_key.is_some_and(|k| k == entry.cache_key) {
                continue;
            }
            freed = freed.saturating_add(entry.size_bytes);
            victims.push(entry);
            if freed >= bytes_to_free {
                break;
            }
        }
        drop(stmt);

        const CHUNK_SIZE: usize = 400;
        for chunk in victims.chunks(CHUNK_SIZE) {
            let mut params: Vec<Value> = Vec::with_capacity(chunk.len());
            let mut placeholders = Vec::with_capacity(chunk.len());
            for (i, entry) in chunk.iter().enumerate() {
                params.push(Value::Text(entry.cache_key.clone()));
                placeholders.push(format!("?{}", i + 1));
            }
            let del_sql = format!(
                "DELETE FROM lru_cache_entries WHERE cache_key IN ({})",
                placeholders.join(", ")
            );
            conn.execute(&del_sql, params_from_iter(params))?;
        }

        Ok(victims)
    })
}

/// Removes and returns all `lru_cache_entries` rows associated with `asset_id`.
pub fn remove_cache_entries_for_asset(
    conn: &Connection,
    asset_id: &str,
) -> DbResult<Vec<LruCacheEntry>> {
    with_savepoint(conn, "remove_asset_cache", || {
        let sql = format!(
            "SELECT {CACHE_COLUMNS}
             FROM lru_cache_entries
             WHERE asset_id = ?1
             ORDER BY cache_key ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let entries = stmt
            .query_map(params![asset_id], row_to_cache_entry)?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        if !entries.is_empty() {
            conn.execute(
                "DELETE FROM lru_cache_entries WHERE asset_id = ?1",
                params![asset_id],
            )?;
        }
        Ok(entries)
    })
}
