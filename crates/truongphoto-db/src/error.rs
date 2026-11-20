use rusqlite::ffi;

/// Database layer errors returned by `truongphoto-db`.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("core domain error: {0}")]
    Core(#[from] truongphoto_core::CoreError),

    #[error("database writer channel closed")]
    ChannelClosed,

    #[error("database read pool semaphore closed")]
    SemaphoreClosed,

    #[error("database background task failed: {0}")]
    TaskJoin(String),

    #[error("database transaction failed: {0}")]
    Transaction(String),

    #[error("migration failed: {0}")]
    Migration(String),

    #[error("backup failed: {0}")]
    Backup(String),

    #[error("entity not found: {0}")]
    NotFound(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}

impl DbError {
    /// Returns true if the underlying SQLite error is a UNIQUE or PRIMARY KEY constraint violation.
    pub fn is_unique_violation(&self) -> bool {
        matches!(
            self,
            Self::Sqlite(rusqlite::Error::SqliteFailure(err, _))
                if err.code == ffi::ErrorCode::ConstraintViolation
                    && matches!(
                        err.extended_code,
                        ffi::SQLITE_CONSTRAINT_UNIQUE | ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                    )
        )
    }

    /// Returns true if the underlying SQLite error is a foreign key constraint violation.
    pub fn is_foreign_key_violation(&self) -> bool {
        matches!(
            self,
            Self::Sqlite(rusqlite::Error::SqliteFailure(err, _))
                if err.code == ffi::ErrorCode::ConstraintViolation
                    && err.extended_code == ffi::SQLITE_CONSTRAINT_FOREIGNKEY
        )
    }
}

/// Convenience result alias for `truongphoto-db` operations.
pub type DbResult<T> = Result<T, DbError>;
