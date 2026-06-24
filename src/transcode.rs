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

use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::storage::blob_key;

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

    let tmp = out.with_extension("tmp.ts");
    let status = tokio::time::timeout(
        TRANSCODE_TIMEOUT,
        Command::new("ffmpeg")
            .args(["-nostdin", "-y", "-fflags", "+genpts", "-r", FPS, "-f", "hevc", "-i"])
            .arg(&src)
            .args([
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "23",
                "-pix_fmt", "yuv420p", "-an", "-f", "mpegts",
            ])
            .arg(&tmp)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
    )
    .await;

    match status {
        Ok(Ok(s)) if s.success() => {
            tokio::fs::rename(&tmp, &out)
                .await
                .map_err(|e| AppError::Other(e.into()))?;
            Ok(out)
        }
        other => {
            let _ = tokio::fs::remove_file(&tmp).await;
            Err(AppError::Other(anyhow::anyhow!("ffmpeg transcode failed: {other:?}")))
        }
    }
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
