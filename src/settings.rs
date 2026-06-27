//! Tiny key/value access to the `settings` table — the single home for the
//! `SELECT value … WHERE key=?` / `INSERT … ON CONFLICT DO UPDATE` pattern that
//! was otherwise inlined across a dozen modules. Feature modules build their
//! typed getters/setters (toggles, intervals, JSON blobs) on top of these.

use crate::error::AppResult;
use crate::state::AppState;

/// Read a settings value (`None` if the key is unset or on a read error).
pub async fn get(state: &AppState, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
}

/// Upsert a settings value.
pub async fn set(state: &AppState, key: &str, value: &str) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(&state.pool)
    .await?;
    Ok(())
}
