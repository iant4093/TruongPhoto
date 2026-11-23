use crate::error::{DbError, DbResult};
use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;

const READ_PRAGMAS: &str = "
    PRAGMA query_only = ON;
    PRAGMA busy_timeout = 5000;
    PRAGMA foreign_keys = ON;
    PRAGMA temp_store = MEMORY;
    PRAGMA cache_size = -16000;
";

/// Custom leak-proof read-only SQLite WAL connection pool.
#[derive(Clone)]
pub struct ReadPool {
    sem: Arc<Semaphore>,
    conns: Arc<Mutex<Vec<Connection>>>,
    closed: Arc<AtomicBool>,
}

impl ReadPool {
    /// Opens `pool_size` read-only SQLite connections configured with `PRAGMA query_only = ON`.
    pub fn open(db_path: &Path, pool_size: usize) -> DbResult<Self> {
        let pool_size = pool_size.max(1);
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let mut conns = Vec::with_capacity(pool_size);

        for _ in 0..pool_size {
            let conn = Connection::open_with_flags(db_path, flags)?;
            conn.execute_batch(READ_PRAGMAS)?;
            conns.push(conn);
        }

        Ok(Self {
            sem: Arc::new(Semaphore::new(pool_size)),
            conns: Arc::new(Mutex::new(conns)),
            closed: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Closes the read pool and drops all pooled SQLite read connections so WAL checkpoints
    /// (`PRAGMA wal_checkpoint(TRUNCATE)`) are not blocked by lingering readers.
    pub async fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.sem.close();
        let conns = std::mem::take(&mut *self.conns.lock());
        let _ = tokio::task::spawn_blocking(move || drop(conns)).await;
    }

    /// Executes a blocking read closure against a pooled read-only connection.
    pub async fn read<T, F>(&self, f: F) -> DbResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> DbResult<T> + Send + 'static,
    {
        if self.closed.load(Ordering::Acquire) {
            return Err(DbError::SemaphoreClosed);
        }
        let permit = self
            .sem
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| DbError::SemaphoreClosed)?;
        let conns = self.conns.clone();
        let closed = self.closed.clone();

        tokio::task::spawn_blocking(move || {
            // Keep `_permit` before `guard` so `guard` always drops and returns the
            // connection to `conns` before `_permit` releases the semaphore slot.
            let _permit = permit;
            if closed.load(Ordering::Acquire) {
                return Err(DbError::SemaphoreClosed);
            }
            let conn = conns
                .lock()
                .pop()
                .ok_or_else(|| DbError::TaskJoin("read pool connection missing".to_string()))?;
            let guard = ConnGuard {
                conn: Some(conn),
                pool: conns,
                closed,
            };
            let res = f(guard.conn_ref());
            drop(guard);
            res
        })
        .await
        .map_err(|e| DbError::TaskJoin(e.to_string()))?
    }
}

struct ConnGuard {
    conn: Option<Connection>,
    pool: Arc<Mutex<Vec<Connection>>>,
    closed: Arc<AtomicBool>,
}

impl ConnGuard {
    fn conn_ref(&self) -> &Connection {
        self.conn
            .as_ref()
            .expect("ConnGuard connection present before Drop")
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            if !self.closed.load(Ordering::Acquire) {
                let mut guard = self.pool.lock();
                if !self.closed.load(Ordering::Acquire) {
                    guard.push(conn);
                }
            }
        }
    }
}
