use crate::error::DbResult;
use crate::repo::{now_ms, to_sql_err};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension, Row};
use truongphoto_core::models::{JobRecord, JobStatus};

/// Parameters for enqueuing a persistent background job.
#[derive(Debug, Clone)]
pub struct EnqueueJob {
    pub kind: String,
    pub payload_json: String,
    pub priority: i32,
    pub max_attempts: i32,
    pub run_after_ms: i64,
}

const JOB_COLUMNS: &str = "
    id, kind, payload_json, status, priority, attempts, max_attempts,
    error_message, run_after_ms, started_at_ms, finished_at_ms,
    created_at_ms, updated_at_ms
";

fn row_to_job_record(row: &Row<'_>) -> rusqlite::Result<JobRecord> {
    let status_str: String = row.get(3)?;
    let status = status_str
        .parse::<JobStatus>()
        .map_err(|e| to_sql_err(e, 3))?;

    Ok(JobRecord {
        id: row.get(0)?,
        kind: row.get(1)?,
        payload_json: row.get(2)?,
        status,
        priority: row.get(4)?,
        attempts: row.get(5)?,
        max_attempts: row.get(6)?,
        error_message: row.get(7)?,
        run_after_ms: row.get(8)?,
        started_at_ms: row.get(9)?,
        finished_at_ms: row.get(10)?,
        created_at_ms: row.get(11)?,
        updated_at_ms: row.get(12)?,
    })
}

/// Inserts a new queued job and returns the persisted `JobRecord`.
pub fn enqueue_job(conn: &Connection, input: &EnqueueJob) -> DbResult<JobRecord> {
    let ts = now_ms();
    conn.execute(
        "INSERT INTO jobs (
            kind, payload_json, status, priority, attempts, max_attempts,
            error_message, run_after_ms, started_at_ms, finished_at_ms,
            created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, 'queued', ?3, 0, ?4, NULL, ?5, NULL, NULL, ?6, ?6)",
        params![
            input.kind,
            input.payload_json,
            input.priority,
            input.max_attempts.max(1),
            input.run_after_ms,
            ts,
        ],
    )?;
    let id = conn.last_insert_rowid();

    Ok(JobRecord {
        id,
        kind: input.kind.clone(),
        payload_json: input.payload_json.clone(),
        status: JobStatus::Queued,
        priority: input.priority,
        attempts: 0,
        max_attempts: input.max_attempts.max(1),
        error_message: None,
        run_after_ms: input.run_after_ms,
        started_at_ms: None,
        finished_at_ms: None,
        created_at_ms: ts,
        updated_at_ms: ts,
    })
}

/// Atomically claims the highest-priority runnable job (optionally filtered by `kinds`).
pub fn claim_next_job(
    conn: &Connection,
    kinds: Option<&[&str]>,
    now_ms: i64,
) -> DbResult<Option<JobRecord>> {
    if let Some(k) = kinds {
        if k.is_empty() {
            return Ok(None);
        }
    }

    conn.execute_batch("SAVEPOINT claim_next_job;")?;
    let res = (|| -> DbResult<Option<JobRecord>> {
        let mut params: Vec<Value> = vec![Value::Integer(now_ms)];
        let kind_clause = match kinds {
            Some(kind_list) => {
                let start = params.len() + 1;
                let mut placeholders = Vec::with_capacity(kind_list.len());
                for (offset, kind) in kind_list.iter().enumerate() {
                    params.push(Value::Text((*kind).to_string()));
                    placeholders.push(format!("?{}", start + offset));
                }
                format!("AND kind IN ({})", placeholders.join(", "))
            }
            None => String::new(),
        };

        let select_sql = format!(
            "SELECT id
             FROM jobs
             WHERE status = 'queued'
               AND run_after_ms <= ?1
               AND attempts < max_attempts
               {kind_clause}
             ORDER BY priority DESC, run_after_ms ASC, id ASC
             LIMIT 1"
        );

        let maybe_id: Option<i64> = conn
            .query_row(&select_sql, params_from_iter(params), |row| row.get(0))
            .optional()?;

        let Some(job_id) = maybe_id else {
            return Ok(None);
        };

        conn.execute(
            "UPDATE jobs
             SET status = 'running',
                 attempts = attempts + 1,
                 started_at_ms = ?2,
                 updated_at_ms = ?2
             WHERE id = ?1",
            params![job_id, now_ms],
        )?;

        let fetch_sql = format!("SELECT {JOB_COLUMNS} FROM jobs WHERE id = ?1");
        let job = conn.query_row(&fetch_sql, params![job_id], row_to_job_record)?;
        Ok(Some(job))
    })();

    match res {
        Ok(job) => {
            conn.execute_batch("RELEASE SAVEPOINT claim_next_job;")?;
            Ok(job)
        }
        Err(err) => {
            let _ = conn.execute_batch(
                "ROLLBACK TO SAVEPOINT claim_next_job; RELEASE SAVEPOINT claim_next_job;",
            );
            Err(err)
        }
    }
}

/// Marks a running job as `completed`, returning true if a row was updated.
pub fn complete_job(conn: &Connection, job_id: i64, now_ms: i64) -> DbResult<bool> {
    let affected = conn.execute(
        "UPDATE jobs
         SET status = 'completed',
             error_message = NULL,
             finished_at_ms = ?2,
             updated_at_ms = ?2
         WHERE id = ?1 AND status = 'running'",
        params![job_id, now_ms],
    )?;
    Ok(affected > 0)
}

/// Records a job failure, re-queuing if `retry_delay_ms` is provided and attempts remain.
pub fn fail_job(
    conn: &Connection,
    job_id: i64,
    error_message: &str,
    retry_delay_ms: Option<i64>,
    now_ms: i64,
) -> DbResult<Option<JobRecord>> {
    let fetch_sql = format!("SELECT {JOB_COLUMNS} FROM jobs WHERE id = ?1");
    let Some(existing) = conn
        .query_row(&fetch_sql, params![job_id], row_to_job_record)
        .optional()?
    else {
        return Ok(None);
    };

    let will_retry = retry_delay_ms.is_some() && existing.attempts < existing.max_attempts;
    if will_retry {
        let next_run = now_ms.saturating_add(retry_delay_ms.unwrap_or(0).max(0));
        conn.execute(
            "UPDATE jobs
             SET status = 'queued',
                 error_message = ?2,
                 run_after_ms = ?3,
                 updated_at_ms = ?4
             WHERE id = ?1",
            params![job_id, error_message, next_run, now_ms],
        )?;
    } else {
        conn.execute(
            "UPDATE jobs
             SET status = 'failed',
                 error_message = ?2,
                 finished_at_ms = ?3,
                 updated_at_ms = ?3
             WHERE id = ?1",
            params![job_id, error_message, now_ms],
        )?;
    }

    let updated = conn.query_row(&fetch_sql, params![job_id], row_to_job_record)?;
    Ok(Some(updated))
}

/// Lists recent jobs, optionally filtered by `status`.
pub fn list_jobs(
    conn: &Connection,
    status: Option<JobStatus>,
    limit: usize,
) -> DbResult<Vec<JobRecord>> {
    let limit = limit.clamp(1, 500) as i64;
    let mut params: Vec<Value> = Vec::new();
    let where_clause = if let Some(s) = status {
        params.push(Value::Text(s.as_str().to_string()));
        "WHERE status = ?1"
    } else {
        ""
    };
    let limit_idx = params.len() + 1;
    params.push(Value::Integer(limit));

    let sql = format!(
        "SELECT {JOB_COLUMNS}
         FROM jobs
         {where_clause}
         ORDER BY id DESC
         LIMIT ?{limit_idx}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params), row_to_job_record)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Counts jobs currently in `queued` or `running` status.
pub fn count_active_jobs(conn: &Connection) -> DbResult<u64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE status IN ('queued', 'running')",
        [],
        |row| row.get(0),
    )?;
    Ok(u64::try_from(count).unwrap_or(0))
}
