pub mod ai;
pub mod albums;
pub mod assets;
pub mod cache;
pub mod chat;
pub mod folders;
pub mod ingest;
pub mod jobs;
pub mod map;
pub mod people;
pub mod search;
pub mod sessions;
pub mod settings;
pub mod stacks;
pub mod tags;
pub mod users;

pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub(crate) fn bool_to_int(value: bool) -> i64 {
    i64::from(value)
}

pub(crate) fn to_sql_err<E: std::fmt::Display>(err: E, col_index: usize) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        col_index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            err.to_string(),
        )),
    )
}

pub(crate) fn with_savepoint<T, F>(
    conn: &rusqlite::Connection,
    name: &str,
    f: F,
) -> crate::error::DbResult<T>
where
    F: FnOnce() -> crate::error::DbResult<T>,
{
    conn.execute_batch(&format!("SAVEPOINT {name};"))?;
    match f() {
        Ok(val) => {
            conn.execute_batch(&format!("RELEASE SAVEPOINT {name};"))?;
            Ok(val)
        }
        Err(err) => {
            let _ = conn.execute_batch(&format!(
                "ROLLBACK TO SAVEPOINT {name}; RELEASE SAVEPOINT {name};"
            ));
            Err(err)
        }
    }
}
