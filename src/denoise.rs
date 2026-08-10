//! Speech enhancement for a drive's audio.
//!
//! The comma's microphone sits in a moving car: the recording is mostly road,
//! wind and HVAC noise with occasional talking. `movie.rs` muxes that raw track
//! straight into the stitched MP4, which is honest but hard to listen to. This
//! module runs the stitched audio through a resident DeepFilterNet server
//! (`POST /denoise`, WAV in → WAV out) before it's encoded.
//!
//! Like `subs.rs` it chunks rather than uploading a whole drive at once — a
//! 40-minute drive is a ~230 MB WAV at the model's native 48 kHz, which is a lot
//! to push through one request. DeepFilterNet is a short-window model, so cutting
//! on chunk boundaries doesn't smear anything across the joins.
//!
//! The result is a plain WAV on disk that the caller feeds to ffmpeg as its audio
//! input. Anything that fails — server down, a chunk rejected — returns `None`
//! and the caller keeps the original audio: a noisy movie beats no movie.

use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// settings-table keys for the runtime toggle + endpoint.
const ENABLED_KEY: &str = "denoise_enabled";
const URL_KEY: &str = "denoise_url";
const ATTEN_KEY: &str = "denoise_atten_db";

/// How much audio goes in one enhancement request (seconds).
const CHUNK_SECS: f64 = 300.0;
/// DeepFilterNet's native rate — resampling here rather than server-side keeps
/// what we send identical to what the model consumes.
const RATE: &str = "48000";
/// A five-minute chunk is seconds of work on a GPU, but the server is shared.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(900);

/// Is denoising enabled? Runtime toggle, seeded from `HC_DENOISE_ENABLED`
/// (default off — it needs a DeepFilterNet server to talk to).
pub async fn is_enabled(state: &AppState) -> bool {
    match crate::settings::get(state, ENABLED_KEY).await {
        Some(s) => s == "1" || s.eq_ignore_ascii_case("true"),
        None => state.config.denoise_enabled,
    }
}

pub async fn set_enabled(state: &AppState, on: bool) -> AppResult<()> {
    crate::settings::set(state, ENABLED_KEY, if on { "1" } else { "0" }).await
}

/// The DeepFilterNet server base URL (no trailing slash). Runtime setting,
/// seeded from `HC_DENOISE_URL`.
pub async fn denoise_url(state: &AppState) -> String {
    let raw = crate::settings::get(state, URL_KEY)
        .await
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| state.config.denoise_url.clone());
    raw.trim().trim_end_matches('/').to_string()
}

pub async fn set_denoise_url(state: &AppState, url: &str) -> AppResult<()> {
    let url = url.trim();
    if !url.is_empty() && !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(AppError::BadRequest("denoise url must be http(s)".into()));
    }
    crate::settings::set(state, URL_KEY, url).await
}

/// How hard to denoise, as DeepFilterNet's `atten_lim_db` — the maximum amount of
/// noise the model may subtract. Lower is gentler and keeps more of the original
/// (road noise included); `None` means full enhancement, which on a car mic can
/// chew into the speech itself. Runtime setting, seeded from `HC_DENOISE_ATTEN_DB`.
pub async fn atten_db(state: &AppState) -> Option<f64> {
    match crate::settings::get(state, ATTEN_KEY).await {
        Some(s) => s.trim().parse().ok(),
        None => state.config.denoise_atten_db,
    }
}

/// `None`/empty clears the limit (full enhancement). The service takes dB of
/// attenuation, so only non-negative values mean anything.
pub async fn set_atten_db(state: &AppState, db: Option<f64>) -> AppResult<()> {
    let v = match db {
        Some(v) if !(0.0..=100.0).contains(&v) => {
            return Err(AppError::BadRequest("denoise attenuation must be 0-100 dB".into()));
        }
        Some(v) => v.to_string(),
        None => String::new(),
    };
    crate::settings::set(state, ATTEN_KEY, &v).await
}

/// Is the server up? Used by the Settings page so a misconfigured URL is obvious
/// before a sweep silently falls back to raw audio.
pub async fn probe(state: &AppState) -> bool {
    let url = denoise_url(state).await;
    if url.is_empty() {
        return false;
    }
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(5)).build() {
        Ok(c) => c,
        Err(_) => return false,
    };
    client
        .get(format!("{url}/health"))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Current denoise settings as JSON (for the Settings UI).
pub async fn settings_json(state: &AppState) -> Value {
    json!({
        "enabled": is_enabled(state).await,
        "denoise_url": denoise_url(state).await,
        "atten_db": atten_db(state).await,
        "reachable": probe(state).await,
    })
}

/// Enhance a drive's audio into a standalone WAV under `dir`.
///
/// `paths` are the qcamera segments in order and `lead` is the first-segment
/// mic-startup gap (`movie::av_lead`) — it's baked in here as leading silence, so
/// the caller muxes the result with no further delay. Returns the WAV's path, or
/// `None` if denoising is off, unconfigured, or failed anywhere along the way
/// (the caller then falls back to the raw audio).
pub async fn build_wav(
    state: &AppState,
    paths: &[String],
    lead: f64,
    dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    if paths.is_empty() || !is_enabled(state).await {
        return None;
    }
    let url = denoise_url(state).await;
    if url.is_empty() {
        return None;
    }
    let atten = atten_db(state).await;
    if tokio::fs::create_dir_all(dir).await.is_err() {
        return None;
    }

    let raw = dir.join("raw.wav");
    if !extract_wav(paths, lead, &raw).await {
        tracing::warn!("denoise: audio extract failed");
        return None;
    }
    let total = crate::transcode::probe_duration(&raw).await.unwrap_or(0.0);

    let client = match reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build() {
        Ok(c) => c,
        Err(_) => return None,
    };

    // Enhance chunk by chunk, collecting the cleaned pieces in order.
    let mut parts: Vec<std::path::PathBuf> = Vec::new();
    let mut offset = 0.0_f64;
    while offset < total.max(0.001) {
        let chunk = dir.join(format!("chunk-{}.wav", offset as i64));
        if !slice_wav(&raw, offset, CHUNK_SECS, &chunk).await {
            break;
        }
        let clean = dir.join(format!("clean-{}.wav", offset as i64));
        match enhance(&client, &url, atten, &chunk, &clean).await {
            Ok(()) => parts.push(clean),
            Err(e) => {
                tracing::warn!("denoise: {e} — falling back to raw audio");
                return None;
            }
        }
        let _ = tokio::fs::remove_file(&chunk).await;
        offset += CHUNK_SECS;
    }
    if parts.is_empty() {
        return None;
    }

    let out = dir.join("clean.wav");
    if !concat_wavs(&parts, &out, dir).await {
        tracing::warn!("denoise: joining enhanced chunks failed — falling back to raw audio");
        return None;
    }
    let _ = tokio::fs::remove_file(&raw).await;
    for p in &parts {
        let _ = tokio::fs::remove_file(p).await;
    }
    tracing::info!(secs = total as i64, chunks = parts.len(), "denoise: enhanced drive audio");
    Some(out)
}

/// Scratch directory for one build, under the data dir (a drive's WAV is
/// hundreds of megabytes — too big for a container's /tmp).
pub fn workdir(state: &AppState, fullname: &str) -> std::path::PathBuf {
    let safe: String =
        fullname.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    state.config.data_dir.join("denoise-work").join(safe)
}

pub async fn cleanup(dir: &std::path::Path) {
    let _ = tokio::fs::remove_dir_all(dir).await;
}

/// Concatenate the segments' audio into one 48 kHz mono WAV, prepending `lead`
/// seconds of silence so the track still lines up with the video.
async fn extract_wav(paths: &[String], lead: f64, out: &std::path::Path) -> bool {
    let input = format!("concat:{}", paths.join("|"));
    let mut args: Vec<String> = ["-nostdin", "-y", "-fflags", "+genpts", "-i", &input, "-vn"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if lead > 0.01 {
        args.extend(["-af".to_string(), format!("adelay={}:all=1", (lead * 1000.0) as i64)]);
    }
    args.extend(["-ac", "1", "-ar", RATE, "-c:a", "pcm_s16le"].iter().map(|s| s.to_string()));
    args.push(out.to_string_lossy().to_string());
    run_ffmpeg(args).await && nonempty(out).await
}

/// Cut `len` seconds starting at `start` out of the extracted WAV (a PCM copy).
/// False when the range is past the end (nothing was written).
async fn slice_wav(src: &std::path::Path, start: f64, len: f64, out: &std::path::Path) -> bool {
    let args: Vec<String> = vec![
        "-nostdin".into(), "-y".into(),
        "-ss".into(), format!("{start}"),
        "-t".into(), format!("{len}"),
        "-i".into(), src.to_string_lossy().to_string(),
        "-c".into(), "copy".into(),
        out.to_string_lossy().to_string(),
    ];
    // A WAV header alone is 44 bytes; require some actual samples.
    run_ffmpeg(args).await && tokio::fs::metadata(out).await.map(|m| m.len() > 1024).unwrap_or(false)
}

/// Join the enhanced chunks back into one WAV (concat demuxer — every part has
/// identical PCM parameters, so this is a stream copy).
async fn concat_wavs(parts: &[std::path::PathBuf], out: &std::path::Path, dir: &std::path::Path) -> bool {
    let list = dir.join("parts.txt");
    let body: String =
        parts.iter().map(|p| format!("file '{}'\n", p.to_string_lossy())).collect();
    if tokio::fs::write(&list, body).await.is_err() {
        return false;
    }
    let args: Vec<String> = vec![
        "-nostdin".into(), "-y".into(),
        "-f".into(), "concat".into(), "-safe".into(), "0".into(),
        "-i".into(), list.to_string_lossy().to_string(),
        "-c".into(), "copy".into(),
        out.to_string_lossy().to_string(),
    ];
    let ok = run_ffmpeg(args).await && nonempty(out).await;
    let _ = tokio::fs::remove_file(&list).await;
    ok
}

/// POST one WAV chunk to the DeepFilterNet server and write the cleaned WAV back.
/// `atten` caps how much noise the model subtracts; omitted = full enhancement.
async fn enhance(
    client: &reqwest::Client,
    url: &str,
    atten: Option<f64>,
    src: &std::path::Path,
    out: &std::path::Path,
) -> anyhow::Result<()> {
    let bytes = tokio::fs::read(src).await?;
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name("audio.wav")
        .mime_str("audio/wav")?;
    let form = reqwest::multipart::Form::new().part("file", part);
    let endpoint = match atten {
        Some(db) => format!("{url}/denoise?atten_lim_db={db}"),
        None => format!("{url}/denoise"),
    };
    let resp = client.post(endpoint).multipart(form).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("denoise server returned {}", resp.status());
    }
    let body = resp.bytes().await?;
    if body.len() < 1024 {
        anyhow::bail!("denoise server returned {} bytes", body.len());
    }
    tokio::fs::write(out, &body).await?;
    Ok(())
}

async fn nonempty(p: &std::path::Path) -> bool {
    tokio::fs::metadata(p).await.map(|m| m.len() > 0).unwrap_or(false)
}

async fn run_ffmpeg(args: Vec<String>) -> bool {
    let child = Command::new("ffmpeg")
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("denoise ffmpeg spawn: {e}");
            return false;
        }
    };
    match tokio::time::timeout(FFMPEG_TIMEOUT, child.wait()).await {
        Ok(Ok(s)) => s.success(),
        Ok(Err(_)) => false,
        Err(_) => {
            let _ = child.start_kill();
            false
        }
    }
}
