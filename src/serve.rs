//! Serve stored blobs (camera/log segments + coords/events/sprite) with HTTP
//! Range support so the browser's video player can seek. Auth: public routes are
//! open; otherwise the device-view rules apply.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::access;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::storage::blob_key;

fn content_type(file: &str) -> &'static str {
    let lower = file.to_ascii_lowercase();
    if lower.ends_with(".ts") {
        "video/mp2t"
    } else if lower.ends_with(".m3u8") {
        "application/vnd.apple.mpegurl"
    } else if lower.ends_with(".mp4") || lower.ends_with(".m4s") {
        "video/mp4"
    } else if lower.ends_with(".hevc") {
        "video/h265"
    } else if lower.ends_with(".json") {
        "application/json"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".bz2") {
        "application/x-bzip2"
    } else if lower.ends_with(".zst") {
        "application/zstd"
    } else {
        "application/octet-stream"
    }
}

/// Parse a single-range `Range: bytes=start-end` header against a known length.
/// Returns (start, end_inclusive). Multi-range is not supported (returns None).
fn parse_range(headers: &HeaderMap, len: u64) -> Option<(u64, u64)> {
    let raw = headers.get(header::RANGE)?.to_str().ok()?;
    let spec = raw.strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (s, e) = spec.split_once('-')?;
    let (start, end) = match (s.trim(), e.trim()) {
        ("", "") => return None,
        ("", suffix) => {
            // last N bytes
            let n: u64 = suffix.parse().ok()?;
            let n = n.min(len);
            (len - n, len - 1)
        }
        (start, "") => (start.parse().ok()?, len - 1),
        (start, end) => (start.parse().ok()?, end.parse::<u64>().ok()?.min(len - 1)),
    };
    if start > end || start >= len {
        return None;
    }
    Some((start, end))
}

/// GET /connectdata/:type/:dongle/:timestamp/:segment/:file (type is cosmetic).
pub async fn connectdata(
    State(state): State<AppState>,
    Path((_ty, dongle, timestamp, segment, file)): Path<(String, String, String, i64, String)>,
    auth: Option<Auth>,
    headers: HeaderMap,
) -> AppResult<Response> {
    serve_blob(state, dongle, timestamp, segment, file, auth, headers).await
}

/// GET /connectdata/:dongle/:timestamp/:segment/:file (no type prefix; used for
/// coords.json / events.json / sprite.jpg).
pub async fn connectdata_notype(
    State(state): State<AppState>,
    Path((dongle, timestamp, segment, file)): Path<(String, String, i64, String)>,
    auth: Option<Auth>,
    headers: HeaderMap,
) -> AppResult<Response> {
    serve_blob(state, dongle, timestamp, segment, file, auth, headers).await
}

async fn serve_blob(
    state: AppState,
    dongle: String,
    timestamp: String,
    segment: i64,
    file: String,
    auth: Option<Auth>,
    headers: HeaderMap,
) -> AppResult<Response> {
    // Authorization: public route → open; else device-view rules.
    let fullname = format!("{dongle}|{timestamp}");
    let is_public: Option<(i64,)> =
        sqlx::query_as("SELECT is_public FROM routes WHERE fullname = ?")
            .bind(&fullname)
            .fetch_optional(&state.pool)
            .await?;
    let public = matches!(is_public, Some((1,)));
    if !public {
        let auth = auth.ok_or_else(|| AppError::Unauthorized("login required".into()))?;
        if !access::can_view_route(&state, &auth, &dongle).await? {
            return Err(AppError::Forbidden("not authorized for this route".into()));
        }
    }

    let key = blob_key(&dongle, &timestamp, segment, &file);
    let path = state.blobs.path_for(&key);
    serve_file(&path, content_type(&file), &headers).await
}

/// GET /v1/transcode/:dongle/:timestamp/:segment/:file — serve an on-demand
/// H.264 transcode of a full-res/driver camera segment (cached on disk).
pub async fn transcode(
    State(state): State<AppState>,
    Path((dongle, timestamp, segment, file)): Path<(String, String, i64, String)>,
    auth: Option<Auth>,
    headers: HeaderMap,
) -> AppResult<Response> {
    // Same auth as blob serving.
    let fullname = format!("{dongle}|{timestamp}");
    let is_public: Option<(i64,)> =
        sqlx::query_as("SELECT is_public FROM routes WHERE fullname = ?")
            .bind(&fullname)
            .fetch_optional(&state.pool)
            .await?;
    if !matches!(is_public, Some((1,))) {
        let auth = auth.ok_or_else(|| AppError::Unauthorized("login required".into()))?;
        if !crate::access::can_view_route(&state, &auth, &dongle).await? {
            return Err(AppError::Forbidden("not authorized for this route".into()));
        }
    }

    let cam = file.strip_suffix(".ts").unwrap_or(&file);
    let path = crate::transcode::ensure_transcode(&state, &dongle, &timestamp, segment, cam).await?;
    serve_file(&path, "video/mp2t", &headers).await
}

/// Stream a file from disk with HTTP Range support (206 when requested).
async fn serve_file(path: &std::path::Path, ct: &str, headers: &HeaderMap) -> AppResult<Response> {
    let mut f = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(_) => return Err(AppError::NotFound("file not found".into())),
    };
    let len = f.metadata().await.map_err(|e| AppError::Other(e.into()))?.len();

    if let Some((start, end)) = parse_range(headers, len) {
        let chunk = end - start + 1;
        f.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|e| AppError::Other(e.into()))?;
        let body = Body::from_stream(ReaderStream::new(f.take(chunk)));
        return Ok((
            StatusCode::PARTIAL_CONTENT,
            [
                (header::CONTENT_TYPE, ct.to_string()),
                (header::ACCEPT_RANGES, "bytes".to_string()),
                (header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}")),
                (header::CONTENT_LENGTH, chunk.to_string()),
            ],
            body,
        )
            .into_response());
    }

    let body = Body::from_stream(ReaderStream::new(f));
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, ct.to_string()),
            (header::ACCEPT_RANGES, "bytes".to_string()),
            (header::CONTENT_LENGTH, len.to_string()),
        ],
        body,
    )
        .into_response())
}
