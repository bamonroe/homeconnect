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
use crate::storage::{blob_key, type_files};

const SWEEP_INTERVAL: Duration = Duration::from_secs(3600); // hourly

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub days: i64,
    pub max_drives: i64,
    pub max_gb: f64,
}

/// Load the effective policy: settings-table overrides on top of config defaults.
pub async fn load_policy(state: &AppState) -> Policy {
    let days = crate::settings::get(state, "retain_days").await.and_then(|v| v.parse().ok()).unwrap_or(state.config.retain_days);
    let max_drives = crate::settings::get(state, "retain_max_drives").await.and_then(|v| v.parse().ok()).unwrap_or(state.config.retain_max_drives);
    let max_gb = crate::settings::get(state, "retain_gb").await.and_then(|v| v.parse().ok()).unwrap_or(state.config.retain_gb);
    Policy { days, max_drives, max_gb }
}

pub async fn save_policy(state: &AppState, p: &Policy) -> AppResult<()> {
    crate::settings::set(state, "retain_days", &p.days.to_string()).await?;
    crate::settings::set(state, "retain_max_drives", &p.max_drives.to_string()).await?;
    crate::settings::set(state, "retain_gb", &p.max_gb.to_string()).await?;
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

    // Reclaim orphaned movies: rows/blobs whose route no longer exists (e.g. left
    // behind by an older prune that didn't delete movies). One-time backlog clear
    // that then stays clean because `delete_route` now removes movies inline.
    let orphans: Vec<(String, String)> = sqlx::query_as(
        "SELECT m.fullname, m.cam FROM movies m \
         LEFT JOIN routes r ON r.fullname = m.fullname \
         WHERE r.fullname IS NULL",
    )
    .fetch_all(&state.pool)
    .await?;
    for (fullname, cam) in &orphans {
        if let Some((dongle, ts)) = fullname.split_once('|') {
            crate::movie::delete(state, dongle, ts, cam).await;
        }
    }
    if !orphans.is_empty() {
        tracing::info!(count = orphans.len(), "retention: reclaimed orphaned movies");
    }

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

/// What a purge removed.
#[derive(Debug, Default, Clone, Copy)]
pub struct Purged {
    pub files: u64,
    pub bytes: u64,
}

/// Remove some data types from one drive on the **server**, leaving the drive
/// itself (its rows, telemetry, coords, model, subtitles) and every other type
/// in place. For each type this deletes the per-segment blobs and clears the
/// segment URL column; for a camera it also drops that camera's transcode cache
/// and stitched movie (both built from the deleted source). The route is then
/// re-aggregated, and its sync types are pinned to exclude what was removed so
/// devsync doesn't pull it straight back off the device. (`qlog` is always
/// pulled, so a purged qlog can come back while the device still holds it.)
pub async fn purge_types(
    state: &AppState,
    dongle: &str,
    ts: &str,
    types: &[String],
) -> AppResult<Purged> {
    let fullname = format!("{dongle}|{ts}");
    let segs: Vec<(i64,)> =
        sqlx::query_as("SELECT number FROM segments WHERE canonical_route_name = ?")
            .bind(&fullname)
            .fetch_all(&state.pool)
            .await?;

    let mut out = Purged::default();
    for (seg,) in &segs {
        for t in types {
            let Some((names, col)) = type_files(t) else { continue };
            for name in names {
                let key = blob_key(dongle, ts, *seg, name);
                if let Some(sz) = state.blobs.size(&key).await {
                    let _ = state.blobs.delete(&key).await;
                    out.files += 1;
                    out.bytes += sz;
                }
            }
            clear_segment_col(state, &fullname, *seg, col).await;
            if matches!(t.as_str(), "fcamera" | "dcamera" | "ecamera") {
                let p = state.config.transcode_dir().join(format!("{dongle}_{ts}--{seg}--{t}.ts"));
                if let Ok(m) = tokio::fs::metadata(&p).await {
                    if tokio::fs::remove_file(&p).await.is_ok() {
                        out.bytes += m.len();
                    }
                }
            }
        }
    }

    crate::parse::recompute_route(state, dongle, ts).await?;

    for t in types {
        if crate::movie::MOVIE_CAMS.contains(&t.as_str()) {
            if let Some(sz) = state.blobs.size(&crate::movie::movie_key(dongle, ts, t)).await {
                out.bytes += sz;
            }
            crate::movie::delete(state, dongle, ts, t).await;
        }
    }

    let kept: Vec<String> = crate::devsync::effective_route_types(state, &fullname)
        .await
        .into_iter()
        .filter(|t| !types.contains(t))
        .collect();
    let _ = crate::devsync::set_route_override(state, &fullname, Some(&kept)).await;
    Ok(out)
}

/// Clear a segment's URL column for a removed type (static SQL per column).
async fn clear_segment_col(state: &AppState, fullname: &str, seg: i64, col: &str) {
    let canonical = format!("{fullname}--{seg}");
    let sql = match col {
        "qcam_url" => "UPDATE segments SET qcam_url='' WHERE canonical_name=?",
        "fcam_url" => "UPDATE segments SET fcam_url='' WHERE canonical_name=?",
        "dcam_url" => "UPDATE segments SET dcam_url='' WHERE canonical_name=?",
        "ecam_url" => "UPDATE segments SET ecam_url='' WHERE canonical_name=?",
        "qlog_url" => "UPDATE segments SET qlog_url='' WHERE canonical_name=?",
        "rlog_url" => "UPDATE segments SET rlog_url='' WHERE canonical_name=?",
        _ => return,
    };
    let _ = sqlx::query(sql).bind(&canonical).execute(&state.pool).await;
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
    // `telemetry.json`/`model.json` are derived per segment and must be listed
    // here too, else they're orphaned when the route is pruned.
    let files = [
        "qcamera.ts", "fcamera.hevc", "dcamera.hevc", "ecamera.hevc",
        "qlog.bz2", "qlog.zst", "rlog.bz2", "rlog.zst",
        "coords.json", "events.json", "telemetry.json", "model.json", "sprite.jpg",
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

    // Route-level stitched movies are the single largest artifact class — they
    // are NOT keyed per segment, so they must be deleted explicitly or they leak
    // (blob + `movies` row) every time a route is pruned, and the storage-cap
    // loop can never reclaim them.
    for cam in crate::movie::MOVIE_CAMS {
        crate::movie::delete(state, dongle, ts, cam).await;
    }
    // Same for the drive's transcript — also route-level, also not per segment.
    crate::subs::delete(state, dongle, ts).await;

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
