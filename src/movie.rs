//! Per-drive "movie" artifacts. Each camera is stored as dozens of one-minute
//! segments; on-demand playback stitches them with an HLS playlist and layers the
//! audio (which lives only in qcamera) over the silent full-res cameras via a
//! JavaScript sync hack. A **movie** is the opposite: all of a drive's segments
//! for one camera, concatenated and encoded **once** into a single seekable H.264
//! MP4 with the drive's audio muxed straight in. It plays natively anywhere (no
//! HLS, no audio hack) and is the clean "watchable" download alongside the raw zip.
//!
//! Movies are built eagerly in the background once a drive is fully synced (every
//! segment has the camera), tracked in the `movies` table for freshness, and
//! stored as a route-level blob (`{dongle}_{ts}--movie--{cam}.mp4`). Encoding is
//! full-res CRF (≈4 MB/min — ~8× smaller than the raw HEVC) via the same VAAPI/CPU
//! selection as the on-demand transcode, CPU as the fallback.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;
use tokio::sync::{Mutex, Notify, Semaphore};

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::transcode;

/// Live progress of the background movie builder, surfaced as a header badge like
/// the sync counter. `pending` is how many movies the current sweep still has to
/// build (it decrements as each finishes); `current` is a label for the one
/// encoding right now. Idle ⇒ pending 0, current None.
#[derive(Clone)]
pub struct MovieQueue {
    inner: Arc<Mutex<MovieQ>>,
    /// Pinged to wake the builder for an immediate sweep (e.g. a sync just
    /// finished pulling a drive), instead of waiting for the next interval tick.
    wake: Arc<Notify>,
}

impl Default for MovieQueue {
    fn default() -> Self {
        Self { inner: Arc::new(Mutex::new(MovieQ::default())), wake: Arc::new(Notify::new()) }
    }
}

#[derive(Default)]
struct MovieQ {
    pending: usize,
    current: Option<String>,
}

impl MovieQueue {
    /// Request an immediate sweep (coalesced — many requests collapse to one wake).
    pub fn request_sweep(&self) {
        self.wake.notify_one();
    }

    /// Sleep up to `secs`, returning early if a sweep was requested meanwhile.
    async fn wait_or_request(&self, secs: u64) {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(secs)) => {}
            _ = self.wake.notified() => {}
        }
    }

    async fn set_pending(&self, n: usize) {
        self.inner.lock().await.pending = n;
    }
    async fn begin(&self, label: String) {
        self.inner.lock().await.current = Some(label);
    }
    async fn finish(&self) {
        let mut s = self.inner.lock().await;
        s.current = None;
        s.pending = s.pending.saturating_sub(1);
    }
    async fn clear(&self) {
        let mut s = self.inner.lock().await;
        s.pending = 0;
        s.current = None;
    }
    /// (pending count, current label) for the API/badge.
    pub async fn stats(&self) -> (usize, Option<String>) {
        let s = self.inner.lock().await;
        (s.pending, s.current.clone())
    }
}

/// Friendly camera name for the progress label.
fn cam_label(cam: &str) -> &'static str {
    match cam {
        "qcamera" => "Road",
        "fcamera" => "Road HD",
        "dcamera" => "Driver",
        "ecamera" => "Wide",
        _ => "camera",
    }
}

/// Is background movie encoding enabled? Runtime toggle in the settings table,
/// falling back to `HC_MOVIE_ENABLED` (default on) when unset.
pub async fn is_enabled(state: &AppState) -> bool {
    let v = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(ENABLED_KEY)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten();
    match v {
        Some(s) => s == "1" || s.eq_ignore_ascii_case("true"),
        None => state.config.movie_enabled,
    }
}

/// Set the runtime on/off toggle.
pub async fn set_enabled(state: &AppState, on: bool) -> AppResult<()> {
    put_setting(state, ENABLED_KEY, if on { "1" } else { "0" }).await
}

/// The sweep interval in seconds. Runtime setting, falling back to
/// `HC_MOVIE_INTERVAL_SECS` (default 120) when unset.
pub async fn get_interval(state: &AppState) -> u64 {
    let v = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(INTERVAL_KEY)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten();
    match v {
        Some(s) => s.parse().unwrap_or(state.config.movie_interval_secs),
        None => state.config.movie_interval_secs,
    }
}

/// Set the sweep interval (seconds).
pub async fn set_interval(state: &AppState, secs: u64) -> AppResult<()> {
    put_setting(state, INTERVAL_KEY, &secs.to_string()).await
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

/// Cameras we build movies for. qcamera already carries audio + is H.264, so its
/// movie is a stream copy; the HEVC cameras are encoded and get qcamera's audio.
pub const MOVIE_CAMS: [&str; 4] = ["qcamera", "fcamera", "dcamera", "ecamera"];

/// Comma cameras record at 20 fps; raw HEVC carries no frame rate.
const FPS: &str = "20";
/// Building a whole-drive movie can take minutes (and a backlog sweep much longer
/// per file); allow well beyond the per-segment transcode timeout.
const BUILD_TIMEOUT: Duration = Duration::from_secs(3600);
/// settings-table key for the runtime on/off toggle.
const ENABLED_KEY: &str = "movie_enabled";
/// settings-table key for the runtime sweep interval (seconds).
const INTERVAL_KEY: &str = "movie_interval";
/// Don't let the sweep run hotter than this, whatever the configured interval.
const MIN_INTERVAL_SECS: u64 = 30;

/// Route-level blob key for a camera's movie.
pub fn movie_key(dongle: &str, ts: &str, cam: &str) -> String {
    format!("{dongle}_{ts}--movie--{cam}.mp4")
}

/// The stored source filename for a camera.
fn source_file(cam: &str) -> Option<&'static str> {
    match cam {
        "qcamera" => Some("qcamera.ts"),
        "fcamera" => Some("fcamera.hevc"),
        "dcamera" => Some("dcamera.hevc"),
        "ecamera" => Some("ecamera.hevc"),
        _ => None,
    }
}

/// Movie builds are heavy; serialize them so a backlog can't swamp the box.
fn sem() -> &'static Semaphore {
    static S: OnceLock<Semaphore> = OnceLock::new();
    S.get_or_init(|| Semaphore::new(1))
}

/// Ordered segment numbers for a route that have the given camera present.
async fn segments_with_cam(state: &AppState, fullname: &str, cam: &str) -> AppResult<Vec<i64>> {
    // Static SQL per camera (sqlx 0.9 needs &'static str).
    let sql = match cam {
        "qcamera" => "SELECT number FROM segments WHERE canonical_route_name = ? AND qcam_url != '' ORDER BY number",
        "fcamera" => "SELECT number FROM segments WHERE canonical_route_name = ? AND fcam_url != '' ORDER BY number",
        "dcamera" => "SELECT number FROM segments WHERE canonical_route_name = ? AND dcam_url != '' ORDER BY number",
        "ecamera" => "SELECT number FROM segments WHERE canonical_route_name = ? AND ecam_url != '' ORDER BY number",
        _ => return Err(AppError::BadRequest(format!("unknown camera: {cam}"))),
    };
    let rows: Vec<(i64,)> = sqlx::query_as(sql).bind(fullname).fetch_all(&state.pool).await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// ffmpeg's `concat:` protocol input string for an ordered list of blob paths.
/// (Raw HEVC Annex-B and MPEG-TS both byte-concatenate cleanly, so no temp file.)
fn concat_input(paths: &[String]) -> String {
    format!("concat:{}", paths.join("|"))
}

/// Build (or rebuild) one camera's movie for a route. Returns the output size in
/// bytes. The qcamera movie is a stream copy (already H.264 + audio); the HEVC
/// cameras are encoded full-res (VAAPI if selected, CPU fallback) with qcamera's
/// audio muxed in.
pub async fn build(state: &AppState, dongle: &str, ts: &str, cam: &str) -> AppResult<u64> {
    let fullname = format!("{dongle}|{ts}");
    let src_file = source_file(cam).ok_or_else(|| AppError::BadRequest(format!("unknown camera: {cam}")))?;

    let segs = segments_with_cam(state, &fullname, cam).await?;
    let seg_count = segs.len() as i64;
    if segs.is_empty() {
        return Err(AppError::NotFound(format!("no {cam} segments for {fullname}")));
    }

    // Ordered source (video) blob paths; skip any blob that's missing or 0 bytes
    // (empty/junk segments — e.g. a 1-segment "drive" with a 0-byte qcamera — can't
    // be encoded and would just fail ffmpeg).
    let mut v_paths: Vec<String> = Vec::new();
    for &n in &segs {
        let p = state.blobs.path_for(&crate::storage::blob_key(dongle, ts, n, src_file));
        if nonempty(&p).await {
            v_paths.push(p.to_string_lossy().to_string());
        }
    }
    if v_paths.is_empty() {
        // Nothing usable — record the attempt so the sweep doesn't retry it forever.
        record(state, &fullname, cam, seg_count, 0, 0.0).await;
        return Err(AppError::NotFound(format!("no usable {cam} source for {fullname}")));
    }

    // Audio comes from qcamera (which carries the mic track) for the same segments.
    let qsegs = segments_with_cam(state, &fullname, "qcamera").await.unwrap_or_default();
    let mut a_paths: Vec<String> = Vec::new();
    for &n in &qsegs {
        let p = state.blobs.path_for(&crate::storage::blob_key(dongle, ts, n, "qcamera.ts"));
        if nonempty(&p).await {
            a_paths.push(p.to_string_lossy().to_string());
        }
    }

    let _permit = sem().acquire().await.expect("semaphore");

    let out_key = movie_key(dongle, ts, cam);
    let out_path = state.blobs.path_for(&out_key);
    if let Some(parent) = out_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| AppError::Other(e.into()))?;
    }
    let tmp = out_path.with_extension("part.mp4");
    let tmp_s = tmp.to_string_lossy().to_string();

    let v_in = concat_input(&v_paths);
    let a_in = concat_input(&a_paths);
    let have_audio = !a_paths.is_empty();
    // The comma's mic starts a couple seconds after the camera on the FIRST segment
    // of a drive (later segments are aligned). Concatenating audio separately drops
    // that lead-in, shifting the whole track early — so delay the audio input by the
    // first segment's measured audio-vs-video gap to realign it.
    let lead = if have_audio { av_lead(&a_paths[0]).await } else { 0.0 };

    let ok = if cam == "qcamera" {
        // Already H.264 + AAC interleaved in MPEG-TS (A/V offset preserved) — just
        // remux to a faststart MP4.
        run(qcamera_copy_args(&v_in, &tmp_s)).await
    } else {
        // Encode HEVC → H.264; prefer the selected GPU, fall back to CPU.
        let device = transcode::current_device(state).await;
        let mut ok = false;
        if let Some(dev) = &device {
            ok = run(vaapi_args(dev, &v_in, &a_in, have_audio, lead, &tmp_s)).await;
            if !ok {
                tracing::warn!(%cam, "movie VAAPI encode failed; falling back to CPU");
                let _ = tokio::fs::remove_file(&tmp).await;
            }
        }
        if !ok {
            ok = run(cpu_args(&v_in, &a_in, have_audio, lead, &tmp_s)).await;
        }
        ok
    };

    if !ok {
        let _ = tokio::fs::remove_file(&tmp).await;
        // Mark the failed attempt (bytes=0) so it isn't retried at this coverage.
        record(state, &fullname, cam, seg_count, 0, 0.0).await;
        return Err(AppError::Other(anyhow::anyhow!("movie encode failed for {cam}")));
    }

    tokio::fs::rename(&tmp, &out_path).await.map_err(|e| AppError::Other(e.into()))?;
    let bytes = tokio::fs::metadata(&out_path).await.map(|m| m.len()).unwrap_or(0);
    let duration = transcode::probe_duration(&out_path).await.unwrap_or(0.0);

    record(state, &fullname, cam, seg_count, bytes as i64, duration).await;
    tracing::info!(%fullname, %cam, segs = seg_count, bytes, "movie: built");
    Ok(bytes)
}

/// True if the blob exists and is non-empty.
async fn nonempty(path: &std::path::Path) -> bool {
    tokio::fs::metadata(path).await.map(|m| m.len() > 0).unwrap_or(false)
}

/// How far the audio track starts after the video in a source file (seconds). On
/// the comma the mic spins up a couple seconds after the camera on the first
/// segment of a drive; this is used to delay the muxed audio so it realigns.
/// Clamped to [0, 10]; 0 on any probe failure.
async fn av_lead(path: &str) -> f64 {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_type,start_time", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .await;
    let Ok(out) = out else { return 0.0 };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut v = None;
    let mut a = None;
    for line in text.lines() {
        let mut it = line.split(',');
        match (it.next(), it.next().and_then(|s| s.trim().parse::<f64>().ok())) {
            (Some("video"), Some(t)) => v = Some(t),
            (Some("audio"), Some(t)) => a = Some(t),
            _ => {}
        }
    }
    match (v, a) {
        (Some(v), Some(a)) => (a - v).clamp(0.0, 10.0),
        _ => 0.0,
    }
}

/// Record a build outcome in the `movies` freshness table. `bytes == 0` marks a
/// failed/empty attempt (so the sweep skips it until the segment coverage changes).
async fn record(state: &AppState, fullname: &str, cam: &str, seg_count: i64, bytes: i64, duration: f64) {
    let now = crate::db::now_secs();
    let _ = sqlx::query(
        "INSERT INTO movies (fullname, cam, seg_count, bytes, duration, built_at) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(fullname, cam) DO UPDATE SET \
           seg_count = excluded.seg_count, bytes = excluded.bytes, \
           duration = excluded.duration, built_at = excluded.built_at",
    )
    .bind(fullname)
    .bind(cam)
    .bind(seg_count)
    .bind(bytes)
    .bind(duration)
    .bind(now)
    .execute(&state.pool)
    .await;
}

/// qcamera: copy the concatenated H.264 + AAC into a faststart MP4.
fn qcamera_copy_args(v_in: &str, out: &str) -> Vec<String> {
    [
        "-nostdin", "-y", "-i", v_in,
        "-c", "copy", "-bsf:a", "aac_adtstoasc",
        "-movflags", "+faststart", out,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// GPU pipeline: decode the concatenated HEVC + encode H.264 on VAAPI, muxing the
/// concatenated qcamera audio (software input) when present. `lead` delays the
/// audio input to realign the first-segment mic-startup gap.
fn vaapi_args(dev: &str, v_in: &str, a_in: &str, audio: bool, lead: f64, out: &str) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-nostdin", "-y",
        "-hwaccel", "vaapi", "-hwaccel_device", dev, "-hwaccel_output_format", "vaapi",
        "-r", FPS, "-f", "hevc", "-i", v_in,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if audio {
        a.extend(["-i", a_in].iter().map(|s| s.to_string()));
    }
    a.extend(
        [
            "-vf", "scale_vaapi=format=nv12", "-map", "0:v:0",
            "-c:v", "h264_vaapi", "-qp", "28",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    if audio {
        a.extend(["-map", "1:a:0?", "-c:a", "aac", "-b:a", "96k"].iter().map(|s| s.to_string()));
        if lead > 0.01 {
            // Prepend the measured first-segment mic-startup gap as silence so the
            // audio sits where it was recorded (deterministic, unlike -itsoffset).
            a.extend(["-af".to_string(), format!("adelay={}:all=1", (lead * 1000.0) as i64)]);
        }
        a.push("-shortest".to_string());
    }
    a.extend(["-movflags", "+faststart", out].iter().map(|s| s.to_string()));
    a
}

/// CPU pipeline (libx264 veryfast crf23) with the same audio mux. `lead` delays the
/// audio input to realign the first-segment mic-startup gap.
fn cpu_args(v_in: &str, a_in: &str, audio: bool, lead: f64, out: &str) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-nostdin", "-y", "-fflags", "+genpts", "-r", FPS, "-f", "hevc", "-i", v_in,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if audio {
        a.extend(["-i", a_in].iter().map(|s| s.to_string()));
    }
    a.extend(
        [
            "-map", "0:v:0",
            "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    if audio {
        a.extend(["-map", "1:a:0?", "-c:a", "aac", "-b:a", "96k"].iter().map(|s| s.to_string()));
        if lead > 0.01 {
            a.extend(["-af".to_string(), format!("adelay={}:all=1", (lead * 1000.0) as i64)]);
        }
        a.push("-shortest".to_string());
    }
    a.extend(["-movflags", "+faststart", out].iter().map(|s| s.to_string()));
    a
}

/// Run ffmpeg with the build timeout; true on clean exit.
async fn run(args: Vec<String>) -> bool {
    matches!(
        tokio::time::timeout(
            BUILD_TIMEOUT,
            Command::new("ffmpeg")
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status(),
        )
        .await,
        Ok(Ok(s)) if s.success()
    )
}

/// Which cameras have a ready movie for a route (+ size/duration), for the UI.
pub async fn status(state: &AppState, fullname: &str) -> Vec<Value> {
    let (dongle, ts) = match fullname.split_once('|') {
        Some((d, t)) => (d, t),
        None => return Vec::new(),
    };
    let rows: Vec<(String, i64, f64)> =
        sqlx::query_as("SELECT cam, bytes, duration FROM movies WHERE fullname = ?")
            .bind(fullname)
            .fetch_all(&state.pool)
            .await
            .unwrap_or_default();
    let mut out = Vec::new();
    for cam in MOVIE_CAMS {
        let row = rows.iter().find(|(c, _, _)| c == cam);
        let ready = match row {
            Some(_) => state.blobs.exists(&movie_key(dongle, ts, cam)).await,
            None => false,
        };
        out.push(json!({
            "cam": cam,
            "ready": ready,
            "bytes": row.map(|(_, b, _)| *b).unwrap_or(0),
            "duration": row.map(|(_, _, d)| *d).unwrap_or(0.0),
        }));
    }
    out
}

/// Build any drive's movies that are complete (every segment has the camera) but
/// missing or stale (built from a different segment count). Sequential — the
/// semaphore serializes encodes regardless, and a long backlog pass just finishes
/// before the next tick. Errors per movie are logged and don't abort the sweep.
pub async fn sweep(state: &AppState) {
    let routes: Vec<(String, String)> =
        match sqlx::query_as("SELECT fullname, device_dongle_id FROM routes")
            .fetch_all(&state.pool)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("movie sweep: {e}");
                return;
            }
        };
    // (fullname, cam) → (seg_count, bytes). bytes==0 marks a known-failed attempt.
    let built: std::collections::HashMap<(String, String), (i64, i64)> =
        sqlx::query_as::<_, (String, String, i64, i64)>("SELECT fullname, cam, seg_count, bytes FROM movies")
            .fetch_all(&state.pool)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(f, c, n, b)| ((f, c), (n, b)))
            .collect();

    // First pass: gather every movie that needs (re)building, so the badge can
    // show an accurate remaining count before any slow encode starts.
    let mut todo: Vec<(String, String, String, &'static str)> = Vec::new(); // (fullname, dongle, ts, cam)
    for (fullname, dongle) in routes {
        let Some((_, ts)) = fullname.split_once('|') else { continue };
        let ts = ts.to_string();
        // Total segments in the route (one COUNT, reused for every camera).
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM segments WHERE canonical_route_name = ?")
            .bind(&fullname)
            .fetch_one(&state.pool)
            .await
            .unwrap_or(0);
        if total == 0 {
            continue;
        }
        for cam in MOVIE_CAMS {
            let with = segments_with_cam(state, &fullname, cam).await.map(|v| v.len() as i64).unwrap_or(0);
            // Only build when the camera fully covers the drive (no missing segs).
            if with == 0 || with != total {
                continue;
            }
            let key = (fullname.clone(), cam.to_string());
            let skip = match built.get(&key) {
                // Already attempted at this exact coverage:
                Some((sc, bytes)) if *sc == total => {
                    // failed/empty (bytes==0) → don't retry; success → skip iff the
                    // blob is still present (rebuild if it was deleted).
                    *bytes == 0 || state.blobs.exists(&movie_key(&dongle, &ts, cam)).await
                }
                _ => false,
            };
            if skip {
                continue;
            }
            todo.push((fullname.clone(), dongle.clone(), ts.clone(), cam));
        }
    }

    if todo.is_empty() {
        return;
    }
    // Second pass: build sequentially (the encode semaphore serializes anyway),
    // updating the progress counter so the header badge reflects the queue.
    state.movie_queue.set_pending(todo.len()).await;
    for (fullname, dongle, ts, cam) in todo {
        // Honor a toggle flipped off mid-sweep (a long backlog can take a while).
        if !is_enabled(state).await {
            break;
        }
        let short = ts.split("--").next().unwrap_or(&ts);
        state.movie_queue.begin(format!("{} · {}", cam_label(cam), short)).await;
        if let Err(e) = build(state, &dongle, &ts, cam).await {
            tracing::warn!(%fullname, %cam, "movie build: {e}");
        }
        state.movie_queue.finish().await;
    }
    state.movie_queue.clear().await;
}

/// Delete a route's movie for one camera (blob + freshness row). Used when its
/// source segments are deleted off the server so a stale movie isn't left behind.
pub async fn delete(state: &AppState, dongle: &str, ts: &str, cam: &str) {
    let _ = state.blobs.delete(&movie_key(dongle, ts, cam)).await;
    let fullname = format!("{dongle}|{ts}");
    let _ = sqlx::query("DELETE FROM movies WHERE fullname = ? AND cam = ?")
        .bind(&fullname)
        .bind(cam)
        .execute(&state.pool)
        .await;
}

/// Spawn the background movie-builder sweep. The on/off toggle and interval are
/// re-read every cycle, so both can be changed from Settings without a restart
/// (`HC_MOVIE_ENABLED`/`HC_MOVIE_INTERVAL_SECS` only seed the defaults).
pub fn spawn(state: AppState) {
    tracing::info!("movie: background builder (runtime toggle + interval)");
    tokio::spawn(async move {
        // Clear prior failed/empty markers so a code change gets one fresh attempt
        // per restart (genuinely-empty drives just re-mark and stop retrying).
        let _ = sqlx::query("DELETE FROM movies WHERE bytes = 0").execute(&state.pool).await;
        loop {
            if !is_enabled(&state).await {
                // Encoding off; poll for re-enable without sweeping.
                tokio::time::sleep(Duration::from_secs(60)).await;
                continue;
            }
            sweep(&state).await;
            // Wait the interval, but wake immediately if a sync just drained the
            // pull queue (a freshly-synced drive shouldn't wait a full interval).
            let secs = get_interval(&state).await.max(MIN_INTERVAL_SECS);
            state.movie_queue.wait_or_request(secs).await;
        }
    });
}
