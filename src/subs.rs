//! Per-drive **subtitles**. A drive's audio lives only in the qcamera segments;
//! `movie.rs` already stitches those into one continuous, audio-muxed MP4. This
//! module takes the same stitched audio, ships it to a resident whisper.cpp
//! server, and stores the transcript as a route-level WebVTT blob
//! (`{dongle}_{ts}--subs--en.vtt`) that the Drive view attaches to the movie as
//! a `<track>`.
//!
//! Two details make the timing line up with what you see:
//!
//! * The same first-segment mic-startup gap the movie compensates for
//!   (`movie::av_lead`) is prepended here as silence, so a cue's `t` matches the
//!   movie timeline (and therefore the route-relative model/telemetry `t`).
//! * The audio is transcribed in fixed-length chunks rather than as one giant
//!   upload — a 40-minute drive is a ~75 MB WAV, which is a lot to push through
//!   one HTTP request, and chunking also lets a long drive make visible progress.
//!   Each chunk's cue times are shifted by its start offset when merged.
//!
//! whisper.cpp's server does voice-activity detection and non-speech-token
//! suppression by default, which matters here: an hour of road noise with no
//! talking would otherwise come back as pages of hallucinated filler.

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// settings-table keys for the runtime toggle + whisper endpoint.
const ENABLED_KEY: &str = "subs_enabled";
const URL_KEY: &str = "whisper_url";

/// How much audio goes in one transcription request (seconds).
const CHUNK_SECS: f64 = 300.0;
/// A chunk of speech is minutes of audio on a GPU model — but the server is
/// shared, so allow for queueing behind another consumer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(900);
/// Extracting a whole drive's audio is quick, but a long drive is a lot of I/O.
const EXTRACT_TIMEOUT: Duration = Duration::from_secs(900);

/// Route-level blob key for a drive's subtitles.
pub fn subs_key(dongle: &str, ts: &str) -> String {
    format!("{dongle}_{ts}--subs--en.vtt")
}

/// Is background subtitle generation enabled? Runtime toggle, seeded from
/// `HC_SUBS_ENABLED` (default off — it needs a whisper server to talk to).
pub async fn is_enabled(state: &AppState) -> bool {
    match crate::settings::get(state, ENABLED_KEY).await {
        Some(s) => s == "1" || s.eq_ignore_ascii_case("true"),
        None => state.config.subs_enabled,
    }
}

pub async fn set_enabled(state: &AppState, on: bool) -> AppResult<()> {
    crate::settings::set(state, ENABLED_KEY, if on { "1" } else { "0" }).await
}

/// The whisper.cpp server base URL (no trailing slash). Runtime setting, seeded
/// from `HC_WHISPER_URL`.
pub async fn whisper_url(state: &AppState) -> String {
    let raw = crate::settings::get(state, URL_KEY)
        .await
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| state.config.whisper_url.clone());
    raw.trim().trim_end_matches('/').to_string()
}

pub async fn set_whisper_url(state: &AppState, url: &str) -> AppResult<()> {
    let url = url.trim();
    if !url.is_empty() && !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(AppError::BadRequest("whisper url must be http(s)".into()));
    }
    crate::settings::set(state, URL_KEY, url).await
}

/// Ask the whisper server whether it's up (used by the Settings page so a
/// misconfigured URL is obvious before a sweep silently does nothing).
pub async fn probe(state: &AppState) -> bool {
    let url = whisper_url(state).await;
    if url.is_empty() {
        return false;
    }
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(5)).build() {
        Ok(c) => c,
        Err(_) => return false,
    };
    client.get(&url).send().await.map(|r| r.status().is_success()).unwrap_or(false)
}

/// Current subtitle settings as JSON (for the Settings UI).
pub async fn settings_json(state: &AppState) -> Value {
    json!({
        "enabled": is_enabled(state).await,
        "whisper_url": whisper_url(state).await,
        "reachable": probe(state).await,
    })
}

/// Transcription is heavy and the whisper server holds one model: serialize.
fn sem() -> &'static Semaphore {
    static S: OnceLock<Semaphore> = OnceLock::new();
    S.get_or_init(|| Semaphore::new(1))
}

/// Segment numbers of a route that carry audio (i.e. have a qcamera).
async fn audio_segments(state: &AppState, fullname: &str) -> Vec<i64> {
    sqlx::query_as::<_, (i64,)>(
        "SELECT number FROM segments WHERE canonical_route_name = ? AND qcam_url != '' ORDER BY number",
    )
    .bind(fullname)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|r| r.0)
    .collect()
}

/// Build (or rebuild) one drive's subtitles. Returns the number of cues found
/// (zero is a legitimate result — most drives are silent).
pub async fn build(state: &AppState, dongle: &str, ts: &str) -> AppResult<usize> {
    let fullname = format!("{dongle}|{ts}");
    let segs = audio_segments(state, &fullname).await;
    let seg_count = segs.len() as i64;

    let mut paths: Vec<String> = Vec::new();
    for &n in &segs {
        let p = state.blobs.path_for(&crate::storage::blob_key(dongle, ts, n, "qcamera.ts"));
        if tokio::fs::metadata(&p).await.map(|m| m.len() > 0).unwrap_or(false) {
            paths.push(p.to_string_lossy().to_string());
        }
    }
    if paths.is_empty() {
        record(state, &fullname, seg_count, 0, 0).await;
        return Err(AppError::NotFound(format!("no audio for {fullname}")));
    }

    let _permit = sem().acquire().await.expect("semaphore");

    // Mirror the movie's audio timeline exactly: same concatenation, same
    // first-segment mic-startup delay, so a cue at t lands where the movie plays it.
    let lead = crate::movie::av_lead(&paths[0]).await;
    let dir = tempdir(state, &fullname).await?;
    let wav = dir.join("audio.wav");
    if !extract_wav(&paths, lead, &wav).await {
        cleanup(&dir).await;
        record(state, &fullname, seg_count, 0, 0).await;
        return Err(AppError::Other(anyhow::anyhow!("audio extract failed for {fullname}")));
    }

    let total = crate::transcode::probe_duration(&wav).await.unwrap_or(0.0);
    let url = whisper_url(state).await;
    if url.is_empty() {
        cleanup(&dir).await;
        return Err(AppError::BadRequest("no whisper url configured".into()));
    }
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| AppError::Other(e.into()))?;

    let mut cues: Vec<Cue> = Vec::new();
    let mut offset = 0.0_f64;
    while offset < total.max(0.001) {
        // A toggle flipped off mid-run stops at the next chunk boundary.
        if !is_enabled(state).await {
            cleanup(&dir).await;
            return Err(AppError::Other(anyhow::anyhow!("subtitle build aborted (disabled)")));
        }
        let chunk = dir.join(format!("chunk-{}.wav", offset as i64));
        if !slice_wav(&wav, offset, CHUNK_SECS, &chunk).await {
            break;
        }
        match transcribe(&client, &url, &chunk).await {
            Ok(vtt) => cues.extend(shift(parse_vtt(&vtt), offset)),
            Err(e) => {
                cleanup(&dir).await;
                record(state, &fullname, seg_count, 0, 0).await;
                return Err(AppError::Other(anyhow::anyhow!("whisper: {e}")));
            }
        }
        let _ = tokio::fs::remove_file(&chunk).await;
        offset += CHUNK_SECS;
    }
    cleanup(&dir).await;

    let vtt = render_vtt(&cues);
    let key = subs_key(dongle, ts);
    state
        .blobs
        .put(&key, vtt.as_bytes())
        .await
        .map_err(|e| AppError::Other(e.into()))?;
    record(state, &fullname, seg_count, cues.len() as i64, vtt.len() as i64).await;
    tracing::info!(%fullname, cues = cues.len(), "subs: built");
    Ok(cues.len())
}

/// Scratch directory for one build (under the data dir, not /tmp — a drive's WAV
/// can be a hundred megabytes).
async fn tempdir(state: &AppState, fullname: &str) -> AppResult<std::path::PathBuf> {
    let safe: String = fullname.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let dir = state.config.data_dir.join("subs-work").join(safe);
    tokio::fs::create_dir_all(&dir).await.map_err(|e| AppError::Other(e.into()))?;
    Ok(dir)
}

async fn cleanup(dir: &std::path::Path) {
    let _ = tokio::fs::remove_dir_all(dir).await;
}

/// Concatenate the qcamera segments' audio into one 16 kHz mono WAV (what
/// whisper wants), prepending `lead` seconds of silence so the timeline matches
/// the movie's.
async fn extract_wav(paths: &[String], lead: f64, out: &std::path::Path) -> bool {
    let input = format!("concat:{}", paths.join("|"));
    let mut args: Vec<String> = ["-nostdin", "-y", "-fflags", "+genpts", "-i", &input, "-vn"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if lead > 0.01 {
        args.extend(["-af".to_string(), format!("adelay={}:all=1", (lead * 1000.0) as i64)]);
    }
    args.extend(
        ["-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"].iter().map(|s| s.to_string()),
    );
    args.push(out.to_string_lossy().to_string());
    run_ffmpeg(args, EXTRACT_TIMEOUT).await && nonempty(out).await
}

/// Cut `len` seconds starting at `start` out of the extracted WAV (a PCM copy —
/// no re-encode). False when the range is past the end (nothing was written).
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
    run_ffmpeg(args, EXTRACT_TIMEOUT).await
        && tokio::fs::metadata(out).await.map(|m| m.len() > 1024).unwrap_or(false)
}

async fn nonempty(p: &std::path::Path) -> bool {
    tokio::fs::metadata(p).await.map(|m| m.len() > 0).unwrap_or(false)
}

async fn run_ffmpeg(args: Vec<String>, timeout: Duration) -> bool {
    let child = Command::new("ffmpeg")
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("subs ffmpeg spawn: {e}");
            return false;
        }
    };
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(s)) => s.success(),
        Ok(Err(_)) => false,
        Err(_) => {
            let _ = child.start_kill();
            false
        }
    }
}

/// POST one WAV chunk to whisper.cpp's `/inference` and return its WebVTT body.
async fn transcribe(client: &reqwest::Client, url: &str, wav: &std::path::Path) -> anyhow::Result<String> {
    let bytes = tokio::fs::read(wav).await?;
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name("audio.wav")
        .mime_str("audio/wav")?;
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("response_format", "vtt")
        .text("temperature", "0.0");
    let resp = client.post(format!("{url}/inference")).multipart(form).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("whisper returned {}", resp.status());
    }
    Ok(resp.text().await?)
}

/// One subtitle cue on the drive's timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Parse whisper.cpp's WebVTT output into cues. Deliberately lenient: anything
/// that isn't a well-formed timing line plus text is skipped.
pub fn parse_vtt(body: &str) -> Vec<Cue> {
    let mut out = Vec::new();
    let mut lines = body.lines().peekable();
    while let Some(line) = lines.next() {
        let Some((a, b)) = line.split_once("-->") else { continue };
        let (Some(start), Some(end)) = (parse_ts(a), parse_ts(b)) else { continue };
        let mut text = Vec::new();
        while let Some(next) = lines.peek() {
            if next.trim().is_empty() {
                break;
            }
            text.push(lines.next().unwrap().trim().to_string());
        }
        let text = text.join("\n");
        if !text.is_empty() {
            out.push(Cue { start, end, text });
        }
    }
    out
}

/// `HH:MM:SS.mmm` / `MM:SS.mmm` (comma or dot decimal) → seconds.
fn parse_ts(s: &str) -> Option<f64> {
    let s = s.trim().replace(',', ".");
    // Strip any WebVTT cue settings trailing the end timestamp ("… align:start").
    let s = s.split_whitespace().next()?;
    let mut secs = 0.0;
    let parts: Vec<&str> = s.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    for p in &parts {
        secs = secs * 60.0 + p.parse::<f64>().ok()?;
    }
    Some(secs)
}

/// Move a chunk's cues onto the whole-drive timeline.
pub fn shift(cues: Vec<Cue>, offset: f64) -> Vec<Cue> {
    cues.into_iter()
        .map(|c| Cue { start: c.start + offset, end: c.end + offset, text: c.text })
        .collect()
}

/// Render cues back out as one WebVTT document.
pub fn render_vtt(cues: &[Cue]) -> String {
    let mut s = String::from("WEBVTT\n\n");
    for c in cues {
        s.push_str(&format!("{} --> {}\n{}\n\n", fmt_ts(c.start), fmt_ts(c.end), c.text));
    }
    s
}

fn fmt_ts(t: f64) -> String {
    let t = t.max(0.0);
    let h = (t / 3600.0).floor() as i64;
    let m = ((t % 3600.0) / 60.0).floor() as i64;
    let s = t % 60.0;
    format!("{h:02}:{m:02}:{s:06.3}")
}

/// Record a build outcome (`bytes == 0` marks a failed attempt so the sweep
/// doesn't retry it until the drive's audio coverage changes).
async fn record(state: &AppState, fullname: &str, seg_count: i64, cues: i64, bytes: i64) {
    let now = crate::db::now_secs();
    let _ = sqlx::query(
        "INSERT INTO subs (fullname, seg_count, cues, bytes, built_at) VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(fullname) DO UPDATE SET \
           seg_count = excluded.seg_count, cues = excluded.cues, \
           bytes = excluded.bytes, built_at = excluded.built_at",
    )
    .bind(fullname)
    .bind(seg_count)
    .bind(cues)
    .bind(bytes)
    .bind(now)
    .execute(&state.pool)
    .await;
}

/// Whether a drive has subtitles ready (+ cue count), for the UI.
pub async fn status(state: &AppState, fullname: &str) -> Value {
    let row: Option<(i64, i64, i64)> =
        sqlx::query_as("SELECT cues, bytes, disabled FROM subs WHERE fullname = ?")
            .bind(fullname)
            .fetch_optional(&state.pool)
            .await
            .unwrap_or_default();
    let (dongle, ts) = match fullname.split_once('|') {
        Some(p) => p,
        None => return json!({ "ready": false, "cues": 0, "disabled": false }),
    };
    let disabled = matches!(row, Some((_, _, 1)));
    let ready = !disabled && row.is_some() && state.blobs.exists(&subs_key(dongle, ts)).await;
    json!({
        "ready": ready,
        "disabled": disabled,
        "cues": row.map(|(c, _, _)| c).unwrap_or(0),
    })
}

/// Delete a drive's subtitles (blob + row) so they rebuild on the next sweep.
pub async fn delete(state: &AppState, dongle: &str, ts: &str) {
    let _ = state.blobs.delete(&subs_key(dongle, ts)).await;
    let _ = sqlx::query("DELETE FROM subs WHERE fullname = ?")
        .bind(format!("{dongle}|{ts}"))
        .execute(&state.pool)
        .await;
}

/// User-delete: remove the blob and mark it so the sweep won't rebuild it.
pub async fn disable(state: &AppState, dongle: &str, ts: &str) {
    let _ = state.blobs.delete(&subs_key(dongle, ts)).await;
    let _ = sqlx::query(
        "INSERT INTO subs (fullname, seg_count, cues, bytes, built_at, disabled) \
         VALUES (?, 0, 0, 0, ?, 1) \
         ON CONFLICT(fullname) DO UPDATE SET disabled = 1, bytes = 0",
    )
    .bind(format!("{dongle}|{ts}"))
    .bind(crate::db::now_secs())
    .execute(&state.pool)
    .await;
}

/// Transcribe any drive whose audio fully covers it but has no (fresh) subtitles.
/// Sequential — the semaphore serializes anyway, and the whisper server is shared
/// with other services on this host.
pub async fn sweep(state: &AppState) {
    let routes: Vec<(String, String)> =
        match sqlx::query_as("SELECT fullname, device_dongle_id FROM routes").fetch_all(&state.pool).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("subs sweep: {e}");
                return;
            }
        };
    let built: std::collections::HashMap<String, (i64, i64, i64)> =
        sqlx::query_as::<_, (String, i64, i64, i64)>("SELECT fullname, seg_count, bytes, disabled FROM subs")
            .fetch_all(&state.pool)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(f, n, b, d)| (f, (n, b, d)))
            .collect();
    let coverage: std::collections::HashMap<String, (i64, i64)> =
        sqlx::query_as::<_, (String, i64, i64)>(
            "SELECT canonical_route_name, COUNT(*), SUM(qcam_url != '') FROM segments \
             GROUP BY canonical_route_name",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(f, total, q)| (f, (total, q)))
        .collect();

    for (fullname, dongle) in routes {
        if !is_enabled(state).await {
            return;
        }
        let Some((_, ts)) = fullname.split_once('|') else { continue };
        let Some(&(total, with_audio)) = coverage.get(&fullname) else { continue };
        // Only transcribe a drive whose audio covers it end to end, so cue times
        // line up with the movie (same rule movies are built under).
        if total == 0 || with_audio != total {
            continue;
        }
        let skip = match built.get(&fullname) {
            Some((_, _, 1)) => true,
            Some((sc, bytes, _)) if *sc == total => {
                *bytes == 0 || state.blobs.exists(&subs_key(&dongle, ts)).await
            }
            _ => false,
        };
        if skip {
            continue;
        }
        if let Err(e) = build(state, &dongle, ts).await {
            tracing::warn!(%fullname, "subs build: {e}");
        }
    }
}

/// Clear every non-disabled transcript so the sweep regenerates them (e.g. after
/// pointing at a different whisper model). Returns how many were cleared.
pub async fn rebuild_all(state: &AppState) -> u64 {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT fullname FROM subs WHERE disabled = 0")
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();
    let n = rows.len() as u64;
    for (fullname,) in &rows {
        if let Some((dongle, ts)) = fullname.split_once('|') {
            let _ = state.blobs.delete(&subs_key(dongle, ts)).await;
        }
    }
    let _ = sqlx::query("DELETE FROM subs WHERE disabled = 0").execute(&state.pool).await;
    tracing::info!(cleared = n, "subs: rebuild all requested");
    n
}

/// Background transcription loop. Runs on the movie sweep interval (subtitles
/// follow the same "drive is fully synced" trigger) and re-reads its toggle every
/// cycle, so Settings changes take effect without a restart.
pub fn spawn(state: AppState) {
    tracing::info!("subs: background transcriber (runtime toggle)");
    tokio::spawn(async move {
        // Give failed attempts one fresh try per restart (a genuinely silent or
        // broken drive just re-marks itself and stops being retried).
        let _ = sqlx::query("DELETE FROM subs WHERE bytes = 0 AND disabled = 0")
            .execute(&state.pool)
            .await;
        loop {
            if is_enabled(&state).await {
                sweep(&state).await;
            }
            let secs = crate::movie::get_interval(&state).await.max(60);
            tokio::time::sleep(Duration::from_secs(secs)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_whisper_vtt() {
        let body = "WEBVTT\n\n00:00:00.100 --> 00:00:03.150\n Turning left onto Main Street.\n\n\
                    00:00:04.000 --> 00:00:05.500\n Traffic is light.\n";
        let cues = parse_vtt(body);
        assert_eq!(cues.len(), 2);
        assert!((cues[0].start - 0.1).abs() < 1e-6);
        assert_eq!(cues[0].text, "Turning left onto Main Street.");
        assert!((cues[1].end - 5.5).abs() < 1e-6);
    }

    #[test]
    fn empty_transcript_yields_no_cues() {
        assert!(parse_vtt("WEBVTT\n").is_empty());
    }

    #[test]
    fn shifts_chunk_cues_onto_the_drive_timeline() {
        let cues = shift(parse_vtt("WEBVTT\n\n00:00:02.000 --> 00:00:03.000\nhi\n"), 600.0);
        assert!((cues[0].start - 602.0).abs() < 1e-6);
        assert!((cues[0].end - 603.0).abs() < 1e-6);
    }

    #[test]
    fn round_trips_through_render() {
        let cues = vec![Cue { start: 3661.5, end: 3662.0, text: "hello".into() }];
        let out = render_vtt(&cues);
        assert!(out.starts_with("WEBVTT"));
        assert!(out.contains("01:01:01.500 --> 01:01:02.000"));
        assert_eq!(parse_vtt(&out), cues);
    }
}
