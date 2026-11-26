//! Retention: a background task that keeps a home server's disk in check by
//! pruning data **per type**, never whole drives. Each data type (the four
//! cameras, `qlog`, `rlog`) has its own limits, any of which may be 0 (unlimited):
//!   - keep the type on drives newer than `days`
//!   - keep it on at most `max_drives` drives per device (newest kept)
//!   - keep its total size under `max_gb` (oldest drives lose it first)
//!
//! On top of that, a whole-store `max_gb` cap prunes the oldest drives'
//! **videos** (never logs) until total storage fits.
//!
//! Pruning a type uses [`purge_types`]: the drive's rows, telemetry, coords,
//! model and subtitles stay, so a drive never disappears from the list. Logs are
//! small and research depends on them, so they default to "keep forever".

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::db::now_millis;
use crate::error::AppResult;
use crate::state::AppState;
use crate::storage::{blob_key, type_files, DATA_TYPES, VIDEO_TYPES};

const SWEEP_INTERVAL: Duration = Duration::from_secs(3600); // hourly
const TYPES_KEY: &str = "retention_types";
const TOTAL_GB_KEY: &str = "retain_gb";

/// Limits for one data type. 0 = unlimited.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    #[serde(default)]
    pub days: i64,
    #[serde(default)]
    pub max_drives: i64,
    #[serde(default)]
    pub max_gb: f64,
}

impl Limits {
    pub fn is_valid(&self) -> bool {
        self.days >= 0 && self.max_drives >= 0 && self.max_gb >= 0.0 && self.max_gb.is_finite()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    /// Whole-store cap in GB (0 = unlimited). Only ever prunes videos.
    pub max_gb: f64,
    /// Limits per data type; always holds every type in [`DATA_TYPES`].
    pub types: BTreeMap<String, Limits>,
}

/// Load the effective policy. Per-type limits come from the `retention_types`
/// setting. Before that existed there was one whole-drive policy; when the
/// setting is unset its age and count limits seed the **video** types (logs
/// stay unlimited), so upgrading keeps pruning videos as before but stops
/// deleting logs.
pub async fn load_policy(state: &AppState) -> Policy {
    let max_gb = crate::settings::get(state, TOTAL_GB_KEY)
        .await
        .and_then(|v| v.parse().ok())
        .unwrap_or(state.config.retain_gb);
    let stored: Option<BTreeMap<String, Limits>> = crate::settings::get(state, TYPES_KEY)
        .await
        .and_then(|v| serde_json::from_str(&v).ok());
    let types = match stored {
        Some(map) => DATA_TYPES
            .iter()
            .map(|t| (t.to_string(), map.get(*t).copied().unwrap_or_default()))
            .collect(),
        None => {
            let days = crate::settings::get(state, "retain_days")
                .await
                .and_then(|v| v.parse().ok())
                .unwrap_or(state.config.retain_days);
            let max_drives = crate::settings::get(state, "retain_max_drives")
                .await
                .and_then(|v| v.parse().ok())
                .unwrap_or(state.config.retain_max_drives);
            let video = Limits { days, max_drives, max_gb: 0.0 };
            DATA_TYPES
                .iter()
                .map(|t| {
                    let l = if VIDEO_TYPES.contains(t) { video } else { Limits::default() };
                    (t.to_string(), l)
                })
                .collect()
        }
    };
    Policy { max_gb, types }
}

/// Save a policy. Unknown types are dropped; missing types become unlimited.
pub async fn save_policy(state: &AppState, p: &Policy) -> AppResult<()> {
    let types: BTreeMap<&str, Limits> = DATA_TYPES
        .iter()
        .map(|t| (*t, p.types.get(*t).copied().unwrap_or_default()))
        .collect();
    let json = serde_json::to_string(&types).map_err(|e| crate::error::AppError::Other(e.into()))?;
    crate::settings::set(state, TYPES_KEY, &json).await?;
    crate::settings::set(state, TOTAL_GB_KEY, &p.max_gb.to_string()).await?;
    Ok(())
}

/// Total bytes stored under the blobs + transcode directories.
pub async fn storage_bytes(state: &AppState) -> u64 {
    usage(state).await.0
}

/// Bytes held per data type (cameras include their transcode cache and movie).
pub async fn type_bytes(state: &AppState) -> BTreeMap<String, u64> {
    usage(state).await.1
}

/// Walk the store once: (total bytes, bytes per data type).
async fn usage(state: &AppState) -> (u64, BTreeMap<String, u64>) {
    let mut per: BTreeMap<String, u64> = DATA_TYPES.iter().map(|t| (t.to_string(), 0)).collect();
    let mut total = 0u64;
    for root in [state.blobs.root().to_path_buf(), state.config.transcode_dir()] {
        walk(&root, &mut |name, len| {
            total += len;
            if let Some(t) = classify(name) {
                *per.entry(t.to_string()).or_default() += len;
            }
        })
        .await;
    }
    (total, per)
}

/// Which data type a stored file belongs to, from its name.
fn classify(name: &str) -> Option<&'static str> {
    for t in DATA_TYPES {
        if VIDEO_TYPES.contains(&t)
            && (name.ends_with(&format!("--movie--{t}.mp4")) || name.ends_with(&format!("--{t}.ts")))
        {
            return Some(t);
        }
        let (names, _) = type_files(t)?;
        if names.iter().any(|n| name.ends_with(&format!("--{n}"))) {
            return Some(t);
        }
    }
    None
}

async fn walk(root: &Path, f: &mut impl FnMut(&str, u64)) {
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
                        f(&entry.file_name().to_string_lossy(), m.len());
                    }
                }
                Err(_) => {}
            }
        }
    }
}

// "Route `r` still holds data type T": a segment with T's URL set, or (for a
// camera) a built movie. `video` = any camera. sqlx wants `&'static str`, so the
// per-type queries are assembled from literals at compile time.
macro_rules! has {
    (cam $col:literal, $cam:literal) => {
        concat!(
            "(EXISTS (SELECT 1 FROM segments s WHERE s.canonical_route_name = r.fullname AND s.",
            $col,
            " != '') OR EXISTS (SELECT 1 FROM movies m WHERE m.fullname = r.fullname AND m.cam = '",
            $cam,
            "' AND m.bytes > 0))"
        )
    };
    (log $col:literal) => {
        concat!(
            "EXISTS (SELECT 1 FROM segments s WHERE s.canonical_route_name = r.fullname AND s.",
            $col,
            " != '')"
        )
    };
}

macro_rules! per_type {
    ($t:expr, $head:literal, $tail:literal) => {
        match $t {
            "qcamera" => Some(concat!($head, has!(cam "qcam_url", "qcamera"), $tail)),
            "fcamera" => Some(concat!($head, has!(cam "fcam_url", "fcamera"), $tail)),
            "dcamera" => Some(concat!($head, has!(cam "dcam_url", "dcamera"), $tail)),
            "ecamera" => Some(concat!($head, has!(cam "ecam_url", "ecamera"), $tail)),
            "qlog" => Some(concat!($head, has!(log "qlog_url"), $tail)),
            "rlog" => Some(concat!($head, has!(log "rlog_url"), $tail)),
            "video" => Some(concat!(
                $head, "(",
                has!(cam "qcam_url", "qcamera"), " OR ",
                has!(cam "fcam_url", "fcamera"), " OR ",
                has!(cam "dcam_url", "dcamera"), " OR ",
                has!(cam "ecam_url", "ecamera"), ")",
                $tail
            )),
            _ => None,
        }
    };
}

/// Drives holding type `t` that started before `cutoff` (unix ms).
async fn older_than(state: &AppState, t: &str, cutoff: i64) -> AppResult<Vec<String>> {
    let Some(sql) = per_type!(
        t,
        "SELECT r.fullname FROM routes r \
         WHERE ((r.start_time_utc_millis > 0 AND r.start_time_utc_millis < ?) \
             OR (r.start_time_utc_millis = 0 AND r.created_at < ?)) AND ",
        ""
    ) else {
        return Ok(Vec::new());
    };
    let rows: Vec<(String,)> =
        sqlx::query_as(sql).bind(cutoff).bind(cutoff).fetch_all(&state.pool).await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// A device's drives holding type `t`, beyond the newest `keep`.
async fn beyond_newest(state: &AppState, t: &str, dongle: &str, keep: i64) -> AppResult<Vec<String>> {
    let Some(sql) = per_type!(
        t,
        "SELECT r.fullname FROM routes r WHERE r.device_dongle_id = ? AND ",
        " ORDER BY r.start_time_utc_millis DESC, r.created_at DESC LIMIT -1 OFFSET ?"
    ) else {
        return Ok(Vec::new());
    };
    let rows: Vec<(String,)> =
        sqlx::query_as(sql).bind(dongle).bind(keep).fetch_all(&state.pool).await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// The oldest drive still holding type `t` (or any video, for `"video"`).
async fn oldest_with(state: &AppState, t: &str) -> AppResult<Option<String>> {
    let Some(sql) = per_type!(
        t,
        "SELECT r.fullname FROM routes r WHERE ",
        " ORDER BY r.start_time_utc_millis ASC, r.created_at ASC LIMIT 1"
    ) else {
        return Ok(None);
    };
    let row: Option<(String,)> = sqlx::query_as(sql).fetch_optional(&state.pool).await?;
    Ok(row.map(|r| r.0))
}

/// What one retention pass removed.
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Report {
    /// (drive, type) pairs pruned.
    pub pruned: usize,
    /// Distinct drives that lost something.
    pub drives: usize,
    pub freed_bytes: u64,
}

/// Run one retention pass.
pub async fn run_once(state: &AppState) -> AppResult<Report> {
    let policy = load_policy(state).await;
    let mut report = Report::default();
    let mut touched: BTreeSet<String> = BTreeSet::new();

    let mut prune = async |fullname: &str, types: &[String], report: &mut Report| -> AppResult<u64> {
        let Some((dongle, ts)) = fullname.split_once('|') else { return Ok(0) };
        let p = purge_types(state, dongle, ts, types).await?;
        report.pruned += types.len();
        report.freed_bytes += p.bytes;
        touched.insert(fullname.to_string());
        tracing::info!(%fullname, ?types, bytes = p.bytes, "retention: pruned");
        Ok(p.bytes)
    };

    let devices: Vec<(String,)> =
        sqlx::query_as("SELECT dongle_id FROM devices").fetch_all(&state.pool).await?;
    let mut used = type_bytes(state).await;

    for t in DATA_TYPES {
        let lim = policy.types.get(t).copied().unwrap_or_default();
        let one = [t.to_string()];
        let mut victims: BTreeSet<String> = BTreeSet::new();

        // 1) Age cutoff.
        if lim.days > 0 {
            victims.extend(older_than(state, t, now_millis() - lim.days * 86_400_000).await?);
        }
        // 2) Per-device count cap among drives that still hold this type.
        if lim.max_drives > 0 {
            for (dongle,) in &devices {
                victims.extend(beyond_newest(state, t, dongle, lim.max_drives).await?);
            }
        }
        for v in &victims {
            let freed = prune(v, &one, &mut report).await?;
            let e = used.entry(t.to_string()).or_default();
            *e = e.saturating_sub(freed);
        }
        // 3) Size cap for this type: oldest drives lose it first.
        if lim.max_gb > 0.0 {
            let cap = (lim.max_gb * 1_000_000_000.0) as u64;
            let mut seen = BTreeSet::new();
            while used.get(t).copied().unwrap_or(0) > cap {
                let Some(v) = oldest_with(state, t).await? else { break };
                if !seen.insert(v.clone()) {
                    break; // purge didn't clear it; don't spin
                }
                let freed = prune(&v, &one, &mut report).await?;
                let e = used.entry(t.to_string()).or_default();
                *e = e.saturating_sub(freed);
            }
        }
    }

    // Reclaim orphaned movies: rows/blobs whose route no longer exists (left
    // behind by an older prune that didn't delete movies).
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

    // 4) Whole-store cap: the oldest drives lose their videos, never their logs.
    if policy.max_gb > 0.0 {
        let cap = (policy.max_gb * 1_000_000_000.0) as u64;
        let mut total = storage_bytes(state).await;
        let videos: Vec<String> = VIDEO_TYPES.iter().map(|t| t.to_string()).collect();
        let mut seen = BTreeSet::new();
        while total > cap {
            let Some(v) = oldest_with(state, "video").await? else { break };
            if !seen.insert(v.clone()) {
                break;
            }
            let freed = prune(&v, &videos, &mut report).await?;
            total = total.saturating_sub(freed);
        }
    }

    report.drives = touched.len();
    if report.pruned > 0 {
        tracing::info!(pruned = report.pruned, drives = report.drives, freed = report.freed_bytes, "retention pass");
    }
    Ok(report)
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
