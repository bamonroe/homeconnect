//! On-demand HEVC→H.264 transcoding. The full-res cameras (fcamera/dcamera/
//! ecamera) are uploaded as raw HEVC, which browsers can't play. We transcode
//! each segment to H.264 in an MPEG-TS container (same shape as qcamera) on
//! first request and cache the result on disk, so subsequent plays are instant.
//!
//! ffmpeg/ffprobe are invoked as subprocesses (simpler and more robust than
//! linking libav). A semaphore bounds concurrent transcodes so a fresh drive
//! doesn't spawn a dozen encoders at once.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use serde::Serialize;
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::storage::blob_key;

/// Settings key for the runtime-selected transcode device.
const DEVICE_KEY: &str = "transcode_device";

/// The encoder/decoder device the admin has selected at runtime, falling back to
/// the `HC_VAAPI_DEVICE` env default. `None` = CPU (libx264).
pub async fn current_device(state: &AppState) -> Option<String> {
    let v = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(DEVICE_KEY)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten();
    match v {
        Some(s) if s == "cpu" => None,
        Some(s) if !s.is_empty() => Some(s),
        _ => state.config.vaapi_device.clone(),
    }
}

/// Persist the selected transcode device ("cpu" or a `/dev/dri/renderD*` path).
pub async fn set_device(state: &AppState, value: &str) -> AppResult<()> {
    sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
        .bind(DEVICE_KEY)
        .bind(value)
        .execute(&state.pool)
        .await?;
    Ok(())
}

#[derive(Serialize)]
pub struct DeviceOption {
    pub value: String,  // "cpu" or a DRM render node path
    pub label: String,  // human-friendly
    pub encodes: bool,   // can it H.264-encode via VAAPI?
}

/// Enumerate the transcode devices the server can use: always CPU, plus each
/// DRM render node (probed via vainfo for a friendly name + encode capability).
pub async fn list_devices() -> Vec<DeviceOption> {
    let mut out = vec![DeviceOption {
        value: "cpu".into(),
        label: "CPU (libx264, ultrafast)".into(),
        encodes: true,
    }];
    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with("renderD"))
                .unwrap_or(false)
        })
        .collect();
    nodes.sort();
    for node in nodes {
        let dev = node.to_string_lossy().to_string();
        let (label, encodes) = probe_vaapi(&dev).await;
        out.push(DeviceOption { value: dev, label, encodes });
    }
    out
}

/// Probe a render node with vainfo → (friendly label, can-H264-encode).
async fn probe_vaapi(dev: &str) -> (String, bool) {
    let node = dev.rsplit('/').next().unwrap_or(dev);
    let output = Command::new("vainfo")
        .args(["--display", "drm", "--device", dev])
        .output()
        .await;
    let Ok(out) = output else {
        return (format!("{node} (unavailable)"), false);
    };
    let text = String::from_utf8_lossy(&out.stdout);
    // Friendly name from the "Driver version:" line — prefer the "for <X> (" part.
    let mut name = node.to_string();
    if let Some(line) = text.lines().find(|l| l.contains("Driver version")) {
        if let Some(rest) = line.split(" for ").nth(1) {
            let n = rest.split('(').next().unwrap_or(rest).trim();
            if !n.is_empty() {
                name = n.to_string();
            }
        } else if line.contains("Intel") {
            name = "Intel GPU".into();
        }
    }
    let encodes = text.contains("VAProfileH264") && text.contains("EncSlice");
    (format!("{name} ({node})"), encodes)
}

/// Comma cameras record at 20 fps; raw HEVC carries no frame rate, so we set it
/// on the input.
const FPS: &str = "20";
/// Hard cap so a corrupt/huge input can't wedge a worker forever.
const TRANSCODE_TIMEOUT: Duration = Duration::from_secs(180);

fn sem() -> &'static Semaphore {
    static S: OnceLock<Semaphore> = OnceLock::new();
    S.get_or_init(|| {
        let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        Semaphore::new((n / 2).max(2))
    })
}

/// Camera stems we know how to transcode and their source blob filenames.
pub fn camera_source(cam: &str) -> Option<&'static str> {
    match cam {
        "fcamera" => Some("fcamera.hevc"),
        "dcamera" => Some("dcamera.hevc"),
        "ecamera" => Some("ecamera.hevc"),
        _ => None,
    }
}

fn cache_path(state: &AppState, dongle: &str, ts: &str, seg: i64, cam: &str) -> PathBuf {
    state
        .config
        .transcode_dir()
        .join(format!("{dongle}_{ts}--{seg}--{cam}.ts"))
}

/// Ensure the transcoded `.ts` for one segment-camera exists, transcoding from
/// the cached HEVC blob if necessary. Returns the cached file path.
pub async fn ensure_transcode(
    state: &AppState,
    dongle: &str,
    ts: &str,
    seg: i64,
    cam: &str,
) -> AppResult<PathBuf> {
    let src_file = camera_source(cam)
        .ok_or_else(|| AppError::BadRequest(format!("not a transcodable camera: {cam}")))?;

    let out = cache_path(state, dongle, ts, seg, cam);
    if tokio::fs::try_exists(&out).await.unwrap_or(false) {
        return Ok(out);
    }

    let src_key = blob_key(dongle, ts, seg, src_file);
    let src = state.blobs.path_for(&src_key);
    if !tokio::fs::try_exists(&src).await.unwrap_or(false) {
        return Err(AppError::NotFound(format!("no {src_file} for segment")));
    }

    if let Some(parent) = out.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| AppError::Other(e.into()))?;
    }

    let _permit = sem().acquire().await.expect("semaphore");
    // Re-check: another request may have produced it while we waited.
    if tokio::fs::try_exists(&out).await.unwrap_or(false) {
        return Ok(out);
    }

    let src = src.to_string_lossy().to_string();
    let tmp = out.with_extension("tmp.ts");
    let tmp_s = tmp.to_string_lossy().to_string();

    // Prefer GPU (VAAPI) if selected; fall back to CPU on any failure so a
    // GPU hiccup never breaks playback.
    let device = current_device(state).await;
    let mut ok = false;
    if let Some(dev) = &device {
        ok = run_ffmpeg(vaapi_args(dev, &src, &tmp_s)).await;
        if !ok {
            tracing::warn!(%cam, "VAAPI transcode failed; falling back to CPU");
            let _ = tokio::fs::remove_file(&tmp).await;
        }
    }
    if !ok {
        ok = run_ffmpeg(cpu_args(&src, &tmp_s)).await;
    }

    if ok {
        tokio::fs::rename(&tmp, &out)
            .await
            .map_err(|e| AppError::Other(e.into()))?;
        Ok(out)
    } else {
        let _ = tokio::fs::remove_file(&tmp).await;
        Err(AppError::Other(anyhow::anyhow!("ffmpeg transcode failed for {cam}")))
    }
}

/// GPU pipeline: decode HEVC + encode H.264 on the VAAPI device.
fn vaapi_args(dev: &str, src: &str, out: &str) -> Vec<String> {
    [
        "-nostdin", "-y",
        "-hwaccel", "vaapi", "-hwaccel_device", dev, "-hwaccel_output_format", "vaapi",
        "-r", FPS, "-f", "hevc", "-i", src,
        "-vf", "scale_vaapi=format=nv12",
        "-c:v", "h264_vaapi", "-qp", "24", "-an", "-f", "mpegts", out,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// CPU pipeline (libx264, ultrafast).
fn cpu_args(src: &str, out: &str) -> Vec<String> {
    [
        "-nostdin", "-y", "-fflags", "+genpts", "-r", FPS, "-f", "hevc", "-i", src,
        "-c:v", "libx264", "-preset", "ultrafast", "-crf", "23",
        "-pix_fmt", "yuv420p", "-an", "-f", "mpegts", out,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Run ffmpeg with the timeout; true on clean exit.
async fn run_ffmpeg(args: Vec<String>) -> bool {
    matches!(
        tokio::time::timeout(
            TRANSCODE_TIMEOUT,
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

/// Probe a media file's duration in seconds (best-effort).
pub async fn probe_duration(path: &std::path::Path) -> Option<f64> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1"])
        .arg(path)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse::<f64>().ok()
}
