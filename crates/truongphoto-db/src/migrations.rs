use crate::error::DbResult;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use tracing::info;

struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "0001_initial",
        sql: include_str!("../migrations/0001_initial.sql"),
    },
    Migration {
        version: 2,
        name: "0002_phase1_media",
        sql: include_str!("../migrations/0002_phase1_media.sql"),
    },
    Migration {
        version: 3,
        name: "0003_phase2_ai",
        sql: include_str!("../migrations/0003_phase2_ai.sql"),
    },
    Migration {
        version: 4,
        name: "0004_phase3_chat_map",
        sql: include_str!("../migrations/0004_phase3_chat_map.sql"),
    },
    Migration {
        version: 5,
        name: "0005_r5_perf_indexes",
        sql: include_str!("../migrations/0005_r5_perf_indexes.sql"),
    },
];

/// Creates `schema_migrations` if missing and applies all pending migrations atomically.
///
/// Safe to invoke on every database open (already-applied versions are skipped).
pub fn run_migrations(conn: &mut Connection) -> DbResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version       INTEGER PRIMARY KEY,
            name          TEXT NOT NULL,
            applied_at_ms INTEGER NOT NULL
        );",
    )?;

    for migration in MIGRATIONS {
        let existing: Option<i64> = conn
            .query_row(
                "SELECT version FROM schema_migrations WHERE version = ?1",
                params![migration.version],
                |row| row.get(0),
            )
            .optional()?;

        if existing.is_some() {
            continue;
        }

        let tx = conn.transaction()?;
        tx.execute_batch(migration.sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
            params![
                migration.version,
                migration.name,
                Utc::now().timestamp_millis()
            ],
        )?;
        tx.commit()?;
        info!(
            version = migration.version,
            name = migration.name,
            "applied database migration"
        );
    }

    Ok(())
}
