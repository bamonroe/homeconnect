//! Retention: a background task that prunes old drives so a home server doesn't
//! fill its disk. Policy (any of which may be 0 = unlimited):
//!   - keep drives newer than `days`
//!   - keep at most `max_drives` per device (newest kept)
//!   - keep total blob storage under `max_gb` (oldest deleted first)
//!
//! Deleting a route removes its blobs (camera/log segments + coords/events/
//! sprite), its transcode cache, and its route/segment rows.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::db::now_millis;
use crate::error::AppResult;
use crate::state::AppState;
use crate::storage::blob_key;

const SWEEP_INTERVAL: Duration = Duration::from_secs(3600); // hourly

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub days: i64,
    pub max_drives: i64,
    pub max_gb: f64,
}

/// Load the effective policy: settings-table overrides on top of config defaults.
pub async fn load_policy(state: &AppState) -> Policy {
    let get = |k: &'static str| async move {
        sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
            .bind(k)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten()
    };
    let days = get("retain_days").await.and_then(|v| v.parse().ok()).unwrap_or(state.config.retain_days);
    let max_drives = get("retain_max_drives").await.and_then(|v| v.parse().ok()).unwrap_or(state.config.retain_max_drives);
    let max_gb = get("retain_gb").await.and_then(|v| v.parse().ok()).unwrap_or(state.config.retain_gb);
    Policy { days, max_drives, max_gb }
}

pub async fn save_policy(state: &AppState, p: &Policy) -> AppResult<()> {
    for (k, v) in [
        ("retain_days", p.days.to_string()),
        ("retain_max_drives", p.max_drives.to_string()),
        ("retain_gb", p.max_gb.to_string()),
    ] {
        sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
            .bind(k)
            .bind(v)
            .execute(&state.pool)
            .await?;
    }
    Ok(())
}

/// Total bytes stored under the blobs + transcode directories.
pub async fn storage_bytes(state: &AppState) -> u64 {
    dir_size(state.blobs.root()).await + dir_size(&state.config.transcode_dir()).await
}

async fn dir_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            match entry.file_type().await {
                Ok(ft) if ft.is_dir() => stack.push(entry.path()),
                Ok(_) => {
                    if let Ok(m) = entry.metadata().await {
                        total += m.len();
                    }
                }
                Err(_) => {}
            }
        }
    }
    total
}

/// Run one retention pass. Returns the number of routes deleted.
pub async fn run_once(state: &AppState) -> AppResult<usize> {
    let policy = load_policy(state).await;
    let mut to_delete: BTreeSet<String> = BTreeSet::new();

    // 1) Age cutoff.
    if policy.days > 0 {
        let cutoff = now_millis() - policy.days * 86_400_000;
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT fullname FROM routes \
             WHERE (start_time_utc_millis > 0 AND start_time_utc_millis < ?) \
                OR (start_time_utc_millis = 0 AND created_at < ?)",
        )
        .bind(cutoff)
        .bind(cutoff)
        .fetch_all(&state.pool)
        .await?;
        to_delete.extend(rows.into_iter().map(|r| r.0));
    }

    // 2) Per-device drive count cap (keep newest max_drives).
    if policy.max_drives > 0 {
        let devices: Vec<(String,)> =
            sqlx::query_as("SELECT dongle_id FROM devices").fetch_all(&state.pool).await?;
        for (dongle,) in devices {
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT fullname FROM routes WHERE device_dongle_id = ? \
                 ORDER BY start_time_utc_millis DESC, created_at DESC LIMIT -1 OFFSET ?",
            )
            .bind(&dongle)
            .bind(policy.max_drives)
            .fetch_all(&state.pool)
            .await?;
            to_delete.extend(rows.into_iter().map(|r| r.0));
        }
    }

    for fullname in &to_delete {
        delete_route(state, fullname).await?;
    }
    let mut deleted = to_delete.len();

    // 3) Storage cap: delete oldest remaining routes until under the limit.
    if policy.max_gb > 0.0 {
        let cap = (policy.max_gb * 1_000_000_000.0) as u64;
        while storage_bytes(state).await > cap {
            let oldest: Option<(String,)> = sqlx::query_as(
                "SELECT fullname FROM routes ORDER BY start_time_utc_millis ASC, created_at ASC LIMIT 1",
            )
            .fetch_optional(&state.pool)
            .await?;
            match oldest {
                Some((fullname,)) => {
                    delete_route(state, &fullname).await?;
                    deleted += 1;
                }
                None => break, // nothing left to delete
            }
        }
    }

    if deleted > 0 {
        tracing::info!(deleted, "retention pass removed routes");
    }
    Ok(deleted)
}

/// Delete a route: its blobs, transcode cache, and DB rows.
pub async fn delete_route(state: &AppState, fullname: &str) -> AppResult<()> {
    let (dongle, ts) = match fullname.split_once('|') {
        Some(p) => p,
        None => return Ok(()),
    };

    let segs: Vec<(i64,)> =
        sqlx::query_as("SELECT number FROM segments WHERE canonical_route_name = ?")
            .bind(fullname)
            .fetch_all(&state.pool)
            .await?;

    // Known per-segment artifacts (extensions tried for the compressed logs).
    let files = [
        "qcamera.ts", "fcamera.hevc", "dcamera.hevc", "ecamera.hevc",
        "qlog.bz2", "qlog.zst", "rlog.bz2", "rlog.zst",
        "coords.json", "events.json", "sprite.jpg",
    ];
    for (seg,) in &segs {
        for f in files {
            let _ = state.blobs.delete(&blob_key(dongle, ts, *seg, f)).await;
        }
        // Transcode cache (.ts per camera).
        for cam in ["fcamera", "dcamera", "ecamera"] {
            let p = state
                .config
                .transcode_dir()
                .join(format!("{dongle}_{ts}--{seg}--{cam}.ts"));
            let _ = tokio::fs::remove_file(&p).await;
        }
    }

    sqlx::query("DELETE FROM segments WHERE canonical_route_name = ?")
        .bind(fullname)
        .execute(&state.pool)
        .await?;
    sqlx::query("DELETE FROM routes WHERE fullname = ?")
        .bind(fullname)
        .execute(&state.pool)
        .await?;
    Ok(())
}

/// Spawn the periodic retention task (runs once shortly after start, then hourly).
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        // small initial delay so startup isn't competing with first requests
        tokio::time::sleep(Duration::from_secs(30)).await;
        loop {
            if let Err(e) = run_once(&state).await {
                tracing::error!("retention pass failed: {e}");
            }
            tokio::time::sleep(SWEEP_INTERVAL).await;
        }
    });
}
