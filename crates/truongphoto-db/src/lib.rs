//! SQLite WAL + FTS5 persistence, single-writer actor, RO read pool,
//! rolling `VACUUM INTO` backups, and RBAC-scoped repositories for TruongPhoto.

pub mod backup;
pub mod error;
pub mod migrations;
pub mod rbac_sql;
pub mod reader;
pub mod repo;
pub mod writer;

use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use backup::BackupResult;
pub use error::{DbError, DbResult};
pub use reader::ReadPool;
pub use rusqlite;
pub use writer::DbWriter;

const WRITER_PRAGMAS: &str = "
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = NORMAL;
    PRAGMA foreign_keys = ON;
    PRAGMA busy_timeout = 5000;
    PRAGMA temp_store = MEMORY;
    PRAGMA cache_size = -64000;
    PRAGMA mmap_size = 268435456;
";

/// Cloneable handle to TruongPhoto's SQLite persistence layer.
///
/// Owns a single dedicated OS writer thread (`tp-db-writer`) and a bounded
/// read-only connection pool (`ReadPool`) configured with `PRAGMA query_only = ON`.
#[derive(Clone)]
pub struct DbHandle {
    db_path: Arc<PathBuf>,
    writer: DbWriter,
    reader: ReadPool,
}

impl DbHandle {
    /// Opens (or creates) the SQLite database at `path`, applies WAL pragmas,
    /// runs idempotent migrations, spawns `tp-db-writer`, and initializes `ReadPool`.
    pub fn open(path: impl AsRef<Path>) -> DbResult<Self> {
        let db_path = path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let rw_flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let mut rw_conn = Connection::open_with_flags(&db_path, rw_flags)?;
        rw_conn.execute_batch(WRITER_PRAGMAS)?;
        migrations::run_migrations(&mut rw_conn)?;

        let writer = DbWriter::spawn(rw_conn)?;
        let pool_size = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(4, 8);
        let reader = ReadPool::open(&db_path, pool_size)?;

        Ok(Self {
            db_path: Arc::new(db_path),
            writer,
            reader,
        })
    }

    /// Returns the filesystem path to the primary SQLite database file.
    pub fn db_path(&self) -> &Path {
        self.db_path.as_path()
    }

    /// Returns the current on-disk size in bytes of the primary DB file (plus any active `-wal`).
    pub fn db_size_bytes(&self) -> u64 {
        let main_len = std::fs::metadata(self.db_path.as_ref())
            .map(|m| m.len())
            .unwrap_or(0);
        let wal_path = PathBuf::from(format!("{}-wal", self.db_path.display()));
        let wal_len = std::fs::metadata(wal_path).map(|m| m.len()).unwrap_or(0);
        main_len.saturating_add(wal_len)
    }

    /// Executes an immediate isolated write closure on the single-writer thread.
    pub async fn write<T, F>(&self, f: F) -> DbResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> DbResult<T> + Send + 'static,
    {
        self.writer.write(f).await
    }

    /// Submits a write closure to be coalesced into a shared `BEGIN IMMEDIATE` transaction
    /// with per-item `SAVEPOINT` isolation.
    pub async fn write_batched<F>(&self, f: F) -> DbResult<()>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> DbResult<()> + Send + 'static,
    {
        self.writer.write_batched(f).await
    }

    /// Executes a read-only closure on a pooled `PRAGMA query_only = ON` connection.
    pub async fn read<T, F>(&self, f: F) -> DbResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> DbResult<T> + Send + 'static,
    {
        self.reader.read(f).await
    }

    /// Creates a consistent snapshot backup via `VACUUM INTO` in `backup_dir`
    /// and prunes older `library-*.db` snapshots beyond `keep_count`.
    pub async fn backup_to_dir(
        &self,
        backup_dir: impl AsRef<Path>,
        keep_count: usize,
    ) -> DbResult<BackupResult> {
        backup::backup_to_dir(
            self.db_path.as_ref().clone(),
            backup_dir.as_ref().to_path_buf(),
            keep_count,
        )
        .await
    }

    /// Checkpoints WAL (`TRUNCATE`) and cleanly shuts down the writer thread.
    pub async fn shutdown(&self) -> DbResult<()> {
        self.writer.shutdown().await
    }
}
