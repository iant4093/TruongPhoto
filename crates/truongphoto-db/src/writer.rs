use crate::error::{DbError, DbResult};
use parking_lot::Mutex;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tracing::{error, warn};

const WRITER_CHANNEL_CAPACITY: usize = 1024;
const MAX_BATCH_SIZE: usize = 500;
const MICRO_BATCH_WINDOW: Duration = Duration::from_millis(15);

type ImmediateOp = Box<dyn FnOnce(&mut Connection) + Send + 'static>;
type BatchOp = Box<dyn FnOnce(&Transaction<'_>) -> DbResult<()> + Send + 'static>;

enum WriteCommand {
    /// Immediate isolated write transaction (e.g. login, folder CRUD, override edit).
    Immediate(ImmediateOp),
    /// Coalesced batch item executed inside a shared `BEGIN IMMEDIATE` transaction with `SAVEPOINT` isolation.
    BatchItem {
        op: BatchOp,
        reply: oneshot::Sender<DbResult<()>>,
    },
    /// Flushes WAL via `PRAGMA wal_checkpoint(TRUNCATE)` and exits the writer thread cleanly.
    Shutdown(oneshot::Sender<DbResult<()>>),
}

struct BatchEntry {
    op: BatchOp,
    reply: oneshot::Sender<DbResult<()>>,
}

/// Cloneable handle to the dedicated single-writer SQLite actor thread (`tp-db-writer`).
#[derive(Clone)]
pub struct DbWriter {
    tx: mpsc::Sender<WriteCommand>,
    thread: Arc<Mutex<Option<JoinHandle<()>>>>,
    shutting_down: Arc<AtomicBool>,
}

impl DbWriter {
    /// Spawns the dedicated `tp-db-writer` OS thread owning the sole RW connection.
    pub fn spawn(conn: Connection) -> DbResult<Self> {
        let (tx, rx) = mpsc::channel(WRITER_CHANNEL_CAPACITY);
        let handle = std::thread::Builder::new()
            .name("tp-db-writer".to_string())
            .spawn(move || writer_loop(conn, rx))?;

        Ok(Self {
            tx,
            thread: Arc::new(Mutex::new(Some(handle))),
            shutting_down: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Executes an immediate isolated write closure on the writer thread.
    pub async fn write<T, F>(&self, f: F) -> DbResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> DbResult<T> + Send + 'static,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        let cmd = WriteCommand::Immediate(Box::new(move |conn| {
            let outcome = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(conn))) {
                Ok(res) => res,
                Err(_) => {
                    error!("immediate db write closure panicked; isolating failure");
                    Err(DbError::TaskJoin("db write closure panicked".to_string()))
                }
            };
            let _ = reply_tx.send(outcome);
        }));

        self.tx
            .send(cmd)
            .await
            .map_err(|_| DbError::ChannelClosed)?;

        reply_rx.await.map_err(|_| DbError::ChannelClosed)?
    }

    /// Submits a write closure to be coalesced into an adaptive `BEGIN IMMEDIATE` batch.
    pub async fn write_batched<F>(&self, f: F) -> DbResult<()>
    where
        F: FnOnce(&Transaction<'_>) -> DbResult<()> + Send + 'static,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        let cmd = WriteCommand::BatchItem {
            op: Box::new(f),
            reply: reply_tx,
        };

        self.tx
            .send(cmd)
            .await
            .map_err(|_| DbError::ChannelClosed)?;

        reply_rx.await.map_err(|_| DbError::ChannelClosed)?
    }

    /// Flushes WAL (`PRAGMA wal_checkpoint(TRUNCATE)`) and cleanly joins the writer thread.
    pub async fn shutdown(&self) -> DbResult<()> {
        if self.shutting_down.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let (reply_tx, reply_rx) = oneshot::channel();
        let res = if self.tx.send(WriteCommand::Shutdown(reply_tx)).await.is_ok() {
            reply_rx.await.map_err(|_| DbError::ChannelClosed)?
        } else {
            Ok(())
        };

        let handle = self.thread.lock().take();
        if let Some(handle) = handle {
            tokio::task::spawn_blocking(move || {
                let _ = handle.join();
            })
            .await
            .map_err(|e| DbError::TaskJoin(e.to_string()))?;
        }

        res
    }
}

fn writer_loop(mut conn: Connection, mut rx: mpsc::Receiver<WriteCommand>) {
    while let Some(cmd) = rx.blocking_recv() {
        match cmd {
            WriteCommand::Immediate(op) => op(&mut conn),
            WriteCommand::Shutdown(reply) => {
                let res = checkpoint_truncate(&conn);
                let _ = reply.send(res);
                return;
            }
            WriteCommand::BatchItem { op, reply } => {
                let mut batch = Vec::with_capacity(64);
                batch.push(BatchEntry { op, reply });

                let mut deferred = None;
                let deadline = Instant::now() + MICRO_BATCH_WINDOW;

                while batch.len() < MAX_BATCH_SIZE {
                    match rx.try_recv() {
                        Ok(WriteCommand::BatchItem { op, reply }) => {
                            batch.push(BatchEntry { op, reply });
                        }
                        Ok(other) => {
                            deferred = Some(other);
                            break;
                        }
                        Err(mpsc::error::TryRecvError::Empty) => {
                            let now = Instant::now();
                            if now >= deadline {
                                break;
                            }
                            let remaining = deadline - now;
                            std::thread::sleep(remaining.min(Duration::from_millis(1)));
                        }
                        Err(mpsc::error::TryRecvError::Disconnected) => break,
                    }
                }

                flush_batch(&mut conn, batch);

                if let Some(next_cmd) = deferred {
                    match next_cmd {
                        WriteCommand::Immediate(op) => op(&mut conn),
                        WriteCommand::Shutdown(reply) => {
                            let res = checkpoint_truncate(&conn);
                            let _ = reply.send(res);
                            return;
                        }
                        WriteCommand::BatchItem { .. } => unreachable!(),
                    }
                }
            }
        }
    }

    if let Err(err) = checkpoint_truncate(&conn) {
        warn!(error = %err, "failed to checkpoint WAL on writer channel drop");
    }
}

fn flush_batch(conn: &mut Connection, batch: Vec<BatchEntry>) {
    let tx = match conn.transaction_with_behavior(TransactionBehavior::Immediate) {
        Ok(tx) => tx,
        Err(err) => {
            let msg = format!("failed to begin immediate transaction: {err}");
            error!("{msg}");
            for entry in batch {
                let _ = entry.reply.send(Err(DbError::Transaction(msg.clone())));
            }
            return;
        }
    };

    let mut outcomes: Vec<(oneshot::Sender<DbResult<()>>, DbResult<()>)> =
        Vec::with_capacity(batch.len());

    for entry in batch {
        if let Err(err) = tx.execute_batch("SAVEPOINT batch_item;") {
            let msg = format!("failed to create savepoint: {err}");
            outcomes.push((entry.reply, Err(DbError::Transaction(msg))));
            continue;
        }

        let item_res =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (entry.op)(&tx))) {
                Ok(res) => res,
                Err(_) => {
                    error!("batched db write closure panicked; rolling back savepoint");
                    Err(DbError::TaskJoin(
                        "batched db write closure panicked".to_string(),
                    ))
                }
            };
        match item_res {
            Ok(()) => {
                if let Err(err) = tx.execute_batch("RELEASE SAVEPOINT batch_item;") {
                    let _ = tx.execute_batch(
                        "ROLLBACK TO SAVEPOINT batch_item; RELEASE SAVEPOINT batch_item;",
                    );
                    outcomes.push((entry.reply, Err(DbError::Sqlite(err))));
                } else {
                    outcomes.push((entry.reply, Ok(())));
                }
            }
            Err(err) => {
                let _ = tx.execute_batch(
                    "ROLLBACK TO SAVEPOINT batch_item; RELEASE SAVEPOINT batch_item;",
                );
                outcomes.push((entry.reply, Err(err)));
            }
        }
    }

    match tx.commit() {
        Ok(()) => {
            for (reply, res) in outcomes {
                let _ = reply.send(res);
            }
        }
        Err(err) => {
            let msg = format!("failed to commit batch transaction: {err}");
            error!("{msg}");
            for (reply, _) in outcomes {
                let _ = reply.send(Err(DbError::Transaction(msg.clone())));
            }
        }
    }
}

fn checkpoint_truncate(conn: &Connection) -> DbResult<()> {
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(())
}
