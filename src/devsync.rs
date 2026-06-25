//! Device sync over SSH. The openpilot uploader can't be repointed at us (it's a
//! forkserver-spawned process that never inherits `continue.sh`'s `API_HOST` —
//! see CLAUDE.md), so instead of waiting to be pushed to, homeconnect **pulls**:
//! it SSHes to the device, lists `/data/media/0/realdata`, and reconciles each
//! segment file against the DB, fetching what's missing and parsing what's on
//! disk but not yet registered.
//!
//! The diff keys on **DB registration**, not blob presence: a qlog blob can be on
//! disk (e.g. a legacy manual import) yet never parsed into a route — so we still
//! register it (from the on-disk blob, no re-pull). Default (background) tier =
//! `qlog` + `qcamera`; full-res cameras + `rlog` are pulled on demand per route.

use std::collections::HashMap;
use std::time::Duration;

use crate::device_ssh;
use crate::error::AppResult;
use crate::ingest::{ingest_segment_file, register_segment_file};
use crate::models::Device;
use crate::state::AppState;
use crate::storage::blob_key;
use crate::sync_queue::QueueItem;

const REALDATA_DIR: &str = "/data/media/0/realdata";
/// settings-table key for the runtime on/off toggle.
const ENABLED_KEY: &str = "sync_enabled";
/// settings-table key for the runtime loop interval (seconds; 0 = loop off).
const INTERVAL_KEY: &str = "sync_interval";
/// Don't let the loop run hotter than this, whatever the configured interval.
const MIN_INTERVAL_SECS: u64 = 10;
/// settings-table key for the default set of data types to sync.
const TYPES_KEY: &str = "sync_types";
/// The selectable data types (qlog is always synced — the route needs it — so it
/// isn't listed here).
pub const OPTIONAL_TYPES: [&str; 5] = ["qcamera", "fcamera", "dcamera", "ecamera", "rlog"];
/// Bounded parallel work — enough to amortise scp connection setup without
/// hammering the device or the sqlite writer.
const SYNC_CONCURRENCY: usize = 4;

/// What to scan/enqueue in one `scan` call.
#[derive(Clone, Default)]
pub struct SyncOpts {
    /// `None` → resolve types per route (the route's override, else the global
    /// default). `Some(list)` → force these types for all matched files (an
    /// explicit pull). `qlog` is always pulled regardless.
    pub types: Option<Vec<String>>,
    /// Limit to a single route (the `{ts}` portion); `None` = all routes.
    pub route: Option<String>,
}

/// What the DB already knows about a segment, used to decide if a remote file
/// still needs work.
#[derive(Default, Clone)]
struct Reg {
    qcam: bool,
    fcam: bool,
    dcam: bool,
    ecam: bool,
    rlog: bool,
    qlog_parsed: bool,
}

/// Is automatic sync (connect trigger + periodic loop) currently enabled? Runtime
/// toggle in the settings table; falls back to `HC_SYNC_ENABLED` (default on)
/// when unset. The manual `POST /sync` endpoint ignores this — it's an explicit
/// user action.
pub async fn is_enabled(state: &AppState) -> bool {
    let v = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(ENABLED_KEY)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten();
    match v {
        Some(s) => s == "1" || s.eq_ignore_ascii_case("true"),
        None => state.config.sync_enabled,
    }
}

/// Set the runtime on/off toggle.
pub async fn set_enabled(state: &AppState, on: bool) -> AppResult<()> {
    put_setting(state, ENABLED_KEY, if on { "1" } else { "0" }).await
}

/// The periodic loop interval in seconds (0 = loop off). Runtime setting,
/// falling back to `HC_SYNC_INTERVAL_SECS` when unset.
pub async fn get_interval(state: &AppState) -> u64 {
    let v = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(INTERVAL_KEY)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten();
    match v {
        Some(s) => s.parse().unwrap_or(state.config.sync_interval_secs),
        None => state.config.sync_interval_secs,
    }
}

/// Set the loop interval (seconds; 0 disables the loop).
pub async fn set_interval(state: &AppState, secs: u64) -> AppResult<()> {
    put_setting(state, INTERVAL_KEY, &secs.to_string()).await
}

/// Every selectable data type (full set), for the "Pull full-res" / sync-all path.
pub fn all_types() -> Vec<String> {
    OPTIONAL_TYPES.iter().map(|s| s.to_string()).collect()
}

/// The default set of data types automatic sync pulls. Runtime setting; falls
/// back to `HC_SYNC_FULLRES` (all types) vs just `qcamera` when unset.
pub async fn get_sync_types(state: &AppState) -> Vec<String> {
    let v = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(TYPES_KEY)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten();
    match v {
        Some(s) => s.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect(),
        None if state.config.sync_fullres => all_types(),
        None => vec!["qcamera".to_string()],
    }
}

/// Set the default sync types (unknown tokens are dropped; order normalised).
pub async fn set_sync_types(state: &AppState, types: &[String]) -> AppResult<()> {
    put_setting(state, TYPES_KEY, &clean_types(types).join(",")).await
}

/// Normalise a requested type list to the known optional types (order preserved).
fn clean_types(types: &[String]) -> Vec<String> {
    OPTIONAL_TYPES
        .iter()
        .copied()
        .filter(|t| types.iter().any(|x| x == t))
        .map(str::to_string)
        .collect()
}

/// A route's per-drive override of the synced types: `None` = inherit the global
/// default; `Some(list)` = an explicit choice (possibly empty = qlog only).
pub async fn get_route_override(state: &AppState, fullname: &str) -> Option<Vec<String>> {
    let row: Option<Option<String>> =
        sqlx::query_scalar::<_, Option<String>>("SELECT sync_types FROM routes WHERE fullname = ?")
            .bind(fullname)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten();
    match row {
        // Row exists with a non-NULL value → explicit override.
        Some(Some(s)) => Some(s.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect()),
        // No row, or NULL → inherit the global default.
        _ => None,
    }
}

/// The effective synced types for a route: its override, else the global default.
pub async fn effective_route_types(state: &AppState, fullname: &str) -> Vec<String> {
    match get_route_override(state, fullname).await {
        Some(t) => t,
        None => get_sync_types(state).await,
    }
}

/// Set (`Some`) or clear (`None` → inherit default) a route's type override.
pub async fn set_route_override(
    state: &AppState,
    fullname: &str,
    types: Option<&[String]>,
) -> AppResult<()> {
    let value: Option<String> = types.map(|t| clean_types(t).join(","));
    sqlx::query("UPDATE routes SET sync_types = ? WHERE fullname = ?")
        .bind(value)
        .bind(fullname)
        .execute(&state.pool)
        .await?;
    Ok(())
}

/// Load every route's override for a dongle, keyed by the route's `{ts}` — used by
/// `scan` to resolve per-route types without a query per file.
async fn load_route_overrides(
    state: &AppState,
    dongle: &str,
) -> AppResult<HashMap<String, Option<Vec<String>>>> {
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT fullname, sync_types FROM routes WHERE device_dongle_id = ?")
            .bind(dongle)
            .fetch_all(&state.pool)
            .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(fullname, st)| {
            let ts = fullname.split_once('|').map(|(_, t)| t.to_string())?;
            let ov = st.map(|s| s.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect());
            Some((ts, ov))
        })
        .collect())
}

async fn put_setting(state: &AppState, key: &str, value: &str) -> AppResult<()> {
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

/// Spawn the pool of background workers that drain the sync queue. Each pulls (or
/// reconciles from disk) one file at a time; together they bound concurrency.
pub fn spawn_workers(state: AppState) {
    for _ in 0..SYNC_CONCURRENCY {
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                let item = state.sync_queue.next().await;
                let key = item.key.clone();
                process_item(&state, item).await;
                state.sync_queue.done(&key).await;
            }
        });
    }
}

/// Enqueue a default-tier scan for one device, in response to an event (the
/// device's athena socket connecting). Guarded so reconnect flaps don't re-scan.
pub async fn trigger(state: &AppState, dongle: &str) {
    if !is_enabled(state).await {
        return;
    }
    if !state.athena.try_begin_sync(dongle).await {
        return; // a scan for this dongle is already running
    }
    let device = match crate::access::load_device(state, dongle).await {
        Ok(Some(d)) => d,
        _ => {
            state.athena.end_sync(dongle).await;
            return;
        }
    };
    let opts = SyncOpts { types: None, route: None };
    match scan(state, &device, opts).await {
        Ok(n) if n > 0 => tracing::info!(dongle = %dongle, "devsync (on connect): queued {n} files"),
        Ok(_) => {}
        Err(e) => tracing::warn!(dongle = %dongle, "devsync (on connect): {e}"),
    }
    state.athena.end_sync(dongle).await;
}

/// Spawn the periodic loop: every `HC_SYNC_INTERVAL_SECS` (default 60), pull from
/// any device that's currently online. It complements the connect trigger (see
/// `trigger`), which gives instant pulls on reconnect; the loop catches a device
/// that stays continuously connected. A pass over an idle, up-to-date device is
/// one cheap `find` + diff. Set the interval to 0 to disable the loop (rely on
/// the connect trigger alone). The first tick fires immediately.
pub fn spawn(state: AppState) {
    // The loop re-reads both the on/off toggle (per `sync_all`) and the interval
    // each cycle, so both can be changed from the UI without a restart.
    // `HC_SYNC_ENABLED`/`HC_SYNC_INTERVAL_SECS` only seed the defaults.
    tracing::info!("devsync: sync on connect + periodic loop (toggle + interval set at runtime)");
    tokio::spawn(async move {
        loop {
            let secs = get_interval(&state).await;
            if secs == 0 {
                // Loop off (connect trigger still active); poll for re-enable.
                tokio::time::sleep(Duration::from_secs(60)).await;
                continue;
            }
            if let Err(e) = sync_all(&state).await {
                tracing::warn!("devsync pass error: {e}");
            }
            tokio::time::sleep(Duration::from_secs(secs.max(MIN_INTERVAL_SECS))).await;
        }
    });
}

/// One background pass over every device that is **currently online** (per
/// athena's 10s liveness) and has an address. Gating on `online` is what keeps a
/// short interval cheap: we never fire a 10s SSH timeout at a device that's
/// driven away — only `find` over the tunnel of one that's actually connected.
async fn sync_all(state: &AppState) -> AppResult<()> {
    if !is_enabled(state).await {
        return Ok(());
    }
    let devices: Vec<Device> =
        sqlx::query_as::<_, Device>("SELECT * FROM devices WHERE last_addr != '' AND online = 1")
            .fetch_all(&state.pool)
            .await?;
    for d in devices {
        // Share the connect-trigger's in-flight guard so the two never overlap.
        if !state.athena.try_begin_sync(&d.dongle_id).await {
            continue;
        }
        let opts = SyncOpts { types: None, route: None };
        match scan(state, &d, opts).await {
            Ok(n) if n > 0 => {
                tracing::info!(dongle = %d.dongle_id, "devsync (backstop): queued {n} files")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(dongle = %d.dongle_id, "devsync (backstop): {e}"),
        }
        state.athena.end_sync(&d.dongle_id).await;
    }
    Ok(())
}

/// List the device's realdata, diff against what the DB already has registered,
/// and **enqueue** the wanted, missing files (the workers do the actual pulling,
/// so callers never block on the transfer). Returns how many files were enqueued.
pub async fn scan(state: &AppState, device: &Device, opts: SyncOpts) -> AppResult<usize> {
    let addr = device.last_addr.clone();
    if addr.is_empty() {
        return Err(crate::error::AppError::BadRequest(
            "device address unknown (not seen online yet)".into(),
        ));
    }

    // One round-trip to enumerate every segment file on the device.
    let cmd = format!("find {REALDATA_DIR} -maxdepth 2 -type f 2>/dev/null");
    let listing = device_ssh::run(state, &addr, &cmd).await?;

    // What's already registered (so we only enqueue what still needs work).
    let reg = load_registration(state, &device.dongle_id).await?;

    // Type resolution: a forced list (explicit pull), or per-route (the route's
    // override, else the global default) so a drive the user trimmed in Manage
    // data isn't re-pulled.
    let forced = opts.types.as_ref();
    let global = if forced.is_none() { get_sync_types(state).await } else { Vec::new() };
    let overrides = if forced.is_none() {
        load_route_overrides(state, &device.dongle_id).await?
    } else {
        HashMap::new()
    };

    let mut items: Vec<QueueItem> = Vec::new();
    for line in listing.lines() {
        let Some((ts, seg, file)) = parse_remote_path(line.trim()) else {
            continue;
        };
        if let Some(route) = &opts.route {
            if &ts != route {
                continue;
            }
        }
        let types: &[String] = match forced {
            Some(t) => t,
            None => overrides
                .get(&ts)
                .and_then(|o| o.as_ref())
                .map(Vec::as_slice)
                .unwrap_or(global.as_slice()),
        };
        if !wanted(&file, types) {
            continue;
        }
        let canonical = format!("{}|{}--{}", device.dongle_id, ts, seg);
        if registered(reg.get(&canonical), &file) {
            continue;
        }
        let key = blob_key(&device.dongle_id, &ts, seg, &file);
        items.push(QueueItem {
            dongle: device.dongle_id.clone(),
            addr: addr.clone(),
            ts,
            seg,
            file,
            key,
        });
    }
    Ok(state.sync_queue.enqueue(items).await)
}

/// Process one queued file: if its blob is already on disk, just register it into
/// the DB (parse the qlog / set the URL — no re-pull); otherwise scp it, store it,
/// and ingest it. Failures are logged; the next scan re-enqueues them.
async fn process_item(state: &AppState, item: QueueItem) {
    let QueueItem { dongle, addr, ts, seg, file, key } = item;

    if state.blobs.exists(&key).await {
        // On disk but not registered — reconcile without touching the network.
        let body = if file.contains("qlog") {
            match state.blobs.get(&key).await {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("devsync read blob {key}: {e}");
                    return;
                }
            }
        } else {
            Vec::new() // non-qlog registration only needs the URL, not the bytes
        };
        if let Err(e) = register_segment_file(state, &dongle, &ts, seg, &file, &body).await {
            tracing::warn!("devsync register {key}: {e}");
        }
        return;
    }

    let remote = format!("{REALDATA_DIR}/{ts}--{seg}/{file}");
    let tmp_dir = state.config.data_dir.join("tmp");
    if let Err(e) = tokio::fs::create_dir_all(&tmp_dir).await {
        tracing::warn!("devsync tmp dir: {e}");
        return;
    }
    let local = tmp_dir.join(&key);

    let res = async {
        device_ssh::pull_file(state, &addr, &remote, &local)
            .await
            .map_err(|e| e.to_string())?;
        let bytes = tokio::fs::read(&local).await.map_err(|e| format!("read tmp: {e}"))?;
        ingest_segment_file(state, &dongle, &ts, seg, &file, &bytes)
            .await
            .map_err(|e| format!("ingest: {e}"))?;
        Ok::<(), String>(())
    }
    .await;

    let _ = tokio::fs::remove_file(&local).await;
    if let Err(e) = res {
        tracing::warn!("devsync pull {key}: {e}");
    }
}

/// Load each segment's registration state for a dongle.
async fn load_registration(state: &AppState, dongle: &str) -> AppResult<HashMap<String, Reg>> {
    let rows: Vec<(String, String, String, String, String, String, String)> = sqlx::query_as(
        "SELECT canonical_name, qcam_url, fcam_url, dcam_url, ecam_url, rlog_url, qlog_url \
         FROM segments WHERE canonical_route_name LIKE ?",
    )
    .bind(format!("{dongle}|%"))
    .fetch_all(&state.pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(name, qcam, fcam, dcam, ecam, rlog, qlog)| {
            (
                name,
                Reg {
                    qcam: !qcam.is_empty(),
                    fcam: !fcam.is_empty(),
                    dcam: !dcam.is_empty(),
                    ecam: !ecam.is_empty(),
                    rlog: !rlog.is_empty(),
                    // `ingest::register_segment_file` sets `qlog_url` after parsing
                    // — a GPS-independent "this qlog was parsed" marker.
                    qlog_parsed: !qlog.is_empty(),
                },
            )
        })
        .collect())
}

/// Is this remote file already registered in the DB (so we can skip it)?
fn registered(reg: Option<&Reg>, file: &str) -> bool {
    let Some(r) = reg else { return false };
    if file.contains("qlog") {
        r.qlog_parsed
    } else if file.contains("qcamera") {
        r.qcam
    } else if file.contains("fcamera") {
        r.fcam
    } else if file.contains("dcamera") {
        r.dcam
    } else if file.contains("ecamera") {
        r.ecam
    } else if file.contains("rlog") {
        r.rlog
    } else {
        true // unknown file type → nothing to do
    }
}

/// Parse a remote realdata file path into `(ts, segment, filename)`.
/// `/data/media/0/realdata/00000009--f3d1ef15b7--5/qcamera.ts`
///   → (`00000009--f3d1ef15b7`, 5, `qcamera.ts`). Non-segment paths (e.g. the
/// `boot/` dir) return `None`.
fn parse_remote_path(path: &str) -> Option<(String, i64, String)> {
    let rest = path.strip_prefix(REALDATA_DIR)?.trim_start_matches('/');
    let (dir, file) = rest.split_once('/')?;
    // dir = "<ts>--<seg>"; split the final "--<seg>" off the right.
    let (ts, seg) = dir.rsplit_once("--")?;
    let seg: i64 = seg.parse().ok()?;
    if ts.is_empty() || file.is_empty() {
        return None;
    }
    Some((ts.to_string(), seg, file.to_string()))
}

/// Map a filename to its optional data-type token (`None` for qlog/unknown).
fn file_type(file: &str) -> Option<&'static str> {
    OPTIONAL_TYPES.iter().copied().find(|t| file.contains(t))
}

/// Should this file be pulled, given the selected types? `qlog` is always pulled
/// (the route can't exist without it); everything else must be in `types`.
fn wanted(file: &str, types: &[String]) -> bool {
    if file.contains("qlog") {
        return true;
    }
    file_type(file).is_some_and(|t| types.iter().any(|x| x == t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_segment_paths() {
        assert_eq!(
            parse_remote_path("/data/media/0/realdata/00000009--f3d1ef15b7--5/qcamera.ts"),
            Some(("00000009--f3d1ef15b7".into(), 5, "qcamera.ts".into()))
        );
        assert_eq!(
            parse_remote_path("/data/media/0/realdata/00000009--f3d1ef15b7--12/qlog.zst"),
            Some(("00000009--f3d1ef15b7".into(), 12, "qlog.zst".into()))
        );
    }

    #[test]
    fn rejects_non_segment_paths() {
        assert_eq!(parse_remote_path("/data/media/0/realdata/boot/somefile.bz2"), None);
        assert_eq!(parse_remote_path("/etc/passwd"), None);
        assert_eq!(parse_remote_path("/data/media/0/realdata/00000009--abc--x/qlog.zst"), None);
    }

    #[test]
    fn type_filter() {
        let default: Vec<String> = vec!["qcamera".into()];
        let all = all_types();
        // qlog always pulled, regardless of selected types.
        assert!(wanted("qlog.zst", &[]));
        assert!(wanted("qcamera.ts", &default));
        assert!(!wanted("fcamera.hevc", &default));
        assert!(!wanted("rlog.zst", &default));
        // full set pulls everything.
        for f in ["qcamera.ts", "fcamera.hevc", "dcamera.hevc", "ecamera.hevc", "rlog.zst"] {
            assert!(wanted(f, &all), "{f} should be wanted with all types");
        }
        assert!(!wanted("random.txt", &all));
        assert_eq!(file_type("dcamera.hevc"), Some("dcamera"));
        assert_eq!(file_type("qlog.zst"), None);
    }

    #[test]
    fn registration_gate() {
        // Unknown segment → always needs work.
        assert!(!registered(None, "qlog.zst"));
        let r = Reg { qcam: true, qlog_parsed: false, ..Default::default() };
        assert!(registered(Some(&r), "qcamera.ts")); // qcam already set → skip
        assert!(!registered(Some(&r), "qlog.zst")); // qlog not parsed → do it
        let r2 = Reg { qlog_parsed: true, fcam: false, ..Default::default() };
        assert!(registered(Some(&r2), "qlog.zst"));
        assert!(!registered(Some(&r2), "fcamera.hevc"));
    }
}
