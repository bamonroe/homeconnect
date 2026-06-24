//! Manage Data: per-drive download (streamed stored zip) and local delete of
//! selected file types. User-only; the caller must own/share the device.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio_util::io::ReaderStream;

use crate::auth::AuthUser;
use crate::error::{AppError, AppResult};
use crate::models::User;
use crate::state::AppState;
use crate::storage::blob_key;

/// Map a selectable type to (candidate filenames, segment URL column).
fn type_spec(t: &str) -> Option<(&'static [&'static str], &'static str)> {
    match t {
        "qcamera" => Some((&["qcamera.ts"], "qcam_url")),
        "fcamera" => Some((&["fcamera.hevc"], "fcam_url")),
        "dcamera" => Some((&["dcamera.hevc"], "dcam_url")),
        "ecamera" => Some((&["ecamera.hevc"], "ecam_url")),
        "qlog" => Some((&["qlog.zst", "qlog.bz2"], "qlog_url")),
        "rlog" => Some((&["rlog.zst", "rlog.bz2"], "rlog_url")),
        _ => None,
    }
}

async fn require_owner(state: &AppState, user: &User, dongle: &str) -> AppResult<()> {
    let device = crate::access::load_device(state, dongle)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown device".into()))?;
    if device.owner_id == Some(user.id) {
        return Ok(());
    }
    let shared: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM authorized_users WHERE user_id = ? AND device_dongle_id = ?",
    )
    .bind(user.id)
    .bind(dongle)
    .fetch_optional(&state.pool)
    .await?;
    if shared.is_some() {
        Ok(())
    } else {
        Err(AppError::Forbidden("not authorized for device".into()))
    }
}

fn split_full(fullname: &str) -> AppResult<(String, String)> {
    fullname
        .split_once('|')
        .map(|(d, t)| (d.to_string(), t.to_string()))
        .ok_or_else(|| AppError::BadRequest("bad route name".into()))
}

async fn segment_numbers(state: &AppState, fullname: &str) -> AppResult<Vec<i64>> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT number FROM segments WHERE canonical_route_name = ? ORDER BY number",
    )
    .bind(fullname)
    .fetch_all(&state.pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// Gather (zip-entry-name, on-disk-path) for the selected types that exist.
async fn collect_files(
    state: &AppState,
    dongle: &str,
    ts: &str,
    segs: &[i64],
    types: &[String],
) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();
    for &seg in segs {
        for t in types {
            let Some((names, _)) = type_spec(t) else { continue };
            for name in names {
                let key = blob_key(dongle, ts, seg, name);
                if state.blobs.exists(&key).await {
                    files.push((format!("{ts}--{seg}/{name}"), state.blobs.path_for(&key)));
                    break; // one filename per (segment,type)
                }
            }
        }
    }
    files
}

#[derive(Deserialize)]
pub struct DownloadQuery {
    /// Comma-separated types, e.g. "fcamera,qlog".
    pub types: String,
}

/// GET /v1/route/:fullname/download?types=fcamera,qlog — streamed stored zip.
pub async fn download(
    State(state): State<AppState>,
    Path(fullname): Path<String>,
    AuthUser(user): AuthUser,
    Query(q): Query<DownloadQuery>,
) -> AppResult<Response> {
    let (dongle, ts) = split_full(&fullname)?;
    require_owner(&state, &user, &dongle).await?;
    let types: Vec<String> = q.types.split(',').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
    let segs = segment_numbers(&state, &fullname).await?;
    let files = collect_files(&state, &dongle, &ts, &segs, &types).await;
    if files.is_empty() {
        return Err(AppError::NotFound("no matching files".into()));
    }

    // Build the stored (uncompressed) zip to a temp file (the zip writer needs
    // Seek), then stream it. We unlink the temp immediately after opening — the
    // open fd keeps it readable until the download finishes, so it self-cleans.
    let tmp_dir = state.config.data_dir.join("tmp");
    tokio::fs::create_dir_all(&tmp_dir).await.map_err(|e| AppError::Other(e.into()))?;
    let tmp = tmp_dir.join(format!("dl-{}.zip", uuid::Uuid::new_v4()));
    let tmp_build = tmp.clone();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let to_io = |e: zip::result::ZipError| std::io::Error::new(std::io::ErrorKind::Other, e);
        let f = std::fs::File::create(&tmp_build)?;
        let mut zw = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(true);
        for (name, path) in files {
            zw.start_file(&name, opts).map_err(to_io)?;
            let mut src = std::fs::File::open(&path)?;
            std::io::copy(&mut src, &mut zw)?;
        }
        zw.finish().map_err(to_io)?;
        Ok(())
    })
    .await
    .map_err(|e| AppError::Other(e.into()))?
    .map_err(|e| AppError::Other(e.into()))?;

    let file = tokio::fs::File::open(&tmp).await.map_err(|e| AppError::Other(e.into()))?;
    let _ = tokio::fs::remove_file(&tmp).await; // unlink; fd stays valid until streamed
    let body = Body::from_stream(ReaderStream::new(file));
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{ts}.zip\""),
            ),
        ],
        body,
    )
        .into_response())
}

#[derive(Deserialize)]
pub struct DeleteReq {
    pub types: Vec<String>,
}

/// POST /v1/route/:fullname/delete {types:[...]} — delete selected file types
/// off the server (blobs + transcode cache), then re-aggregate the route.
pub async fn delete(
    State(state): State<AppState>,
    Path(fullname): Path<String>,
    AuthUser(user): AuthUser,
    Json(req): Json<DeleteReq>,
) -> AppResult<Json<Value>> {
    let (dongle, ts) = split_full(&fullname)?;
    require_owner(&state, &user, &dongle).await?;
    let segs = segment_numbers(&state, &fullname).await?;

    let mut removed = 0u64;
    let mut freed = 0u64;
    for &seg in &segs {
        for t in &req.types {
            let Some((names, col)) = type_spec(t) else { continue };
            for name in names {
                let key = blob_key(&dongle, &ts, seg, name);
                if let Some(sz) = state.blobs.size(&key).await {
                    let _ = state.blobs.delete(&key).await;
                    removed += 1;
                    freed += sz;
                }
            }
            clear_segment_col(&state, &dongle, &ts, seg, col).await;
            // Camera transcode caches become stale once the source is gone.
            for cam in ["fcamera", "dcamera", "ecamera"] {
                if t == cam {
                    let p = state.config.transcode_dir().join(format!("{dongle}_{ts}--{seg}--{cam}.ts"));
                    let _ = tokio::fs::remove_file(&p).await;
                }
            }
        }
    }

    crate::parse::recompute_route(&state, &dongle, &ts).await?;
    tracing::info!(%fullname, removed, freed, "manage: deleted files");
    Ok(Json(json!({ "removed": removed, "freed_bytes": freed })))
}

/// Clear a segment's URL column for a deleted type (static SQL per column).
async fn clear_segment_col(state: &AppState, dongle: &str, ts: &str, seg: i64, col: &str) {
    let canonical = format!("{dongle}|{ts}--{seg}");
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
