use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

/// Open the SQLite pool (creating the file if needed, WAL mode) and run migrations.
pub async fn init(db_url: &str) -> anyhow::Result<SqlitePool> {
    let opts = SqliteConnectOptions::from_str(db_url)?
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        // WAL + NORMAL is the safe, fast pairing: fsync happens only at
        // checkpoints, not on every commit. On the bulk HDD this data lives on,
        // FULL's per-commit fsync serialised the pool's writers on the single WAL
        // write lock until the 15s busy_timeout was exhausted, surfacing as
        // "database is locked" (code 5) on ingest — drives failing to sync.
        // NORMAL only risks losing the last commit(s) on OS crash/power loss,
        // never corruption.
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(std::time::Duration::from_secs(15));

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

/// Current unix time in milliseconds.
pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Current unix time in seconds (comma's last_athena_ping convention).
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
