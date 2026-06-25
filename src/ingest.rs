//! Ingest: the device uploads files here (the URL we hand back from
//! `upload_url`). Bodies are written to the blob store; driving-log segments
//! are queued for parsing in M2. Auth is the device JWT (identity == dongle).

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;

use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::storage::blob_key;

/// Guard: the authenticated principal must be *this* device.
fn require_device(auth: &Auth, dongle: &str) -> AppResult<()> {
    match &auth.device {
        Some(d) if d.dongle_id == dongle && auth.claims.identity == dongle => Ok(()),
        Some(_) => Err(AppError::Forbidden("device/dongle mismatch".into())),
        None => Err(AppError::Unauthorized("device token required".into())),
    }
}

async fn store(state: &AppState, dongle: &str, key: &str, body: &[u8]) -> AppResult<StatusCode> {
    // Refuse silent overwrite of an existing upload (matches reference: 403).
    if state.blobs.exists(key).await {
        return Err(AppError::Forbidden("file already uploaded".into()));
    }
    state
        .blobs
        .put(key, body)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("blob write: {e}")))?;

    // Cumulative storage accounting.
    let _ = sqlx::query("UPDATE devices SET server_storage = server_storage + ? WHERE dongle_id = ?")
        .bind(body.len() as i64)
        .bind(dongle)
        .execute(&state.pool)
        .await;

    Ok(StatusCode::CREATED)
}

/// Ingest one already-fetched segment file. Shared by the HTTP upload handler
/// (`upload_driving`) and the SSH puller (`devsync`): store the blob, then parse
/// (qlog) or register the file's URL (camera/rlog) — synchronously, so the caller
/// learns the outcome. Idempotent: re-ingesting an existing blob returns
/// `Forbidden` from `store`, which the puller treats as "already have it".
pub async fn ingest_segment_file(
    state: &AppState,
    dongle: &str,
    timestamp: &str,
    segment: i64,
    file: &str,
    body: &[u8],
) -> AppResult<StatusCode> {
    let key = blob_key(dongle, timestamp, segment, file);
    let status = store(state, dongle, &key, body).await?;
    register_segment_file(state, dongle, timestamp, segment, file, body).await?;
    // The rlog carries modelV2 (the qlog doesn't) — derive the top-down artifact.
    if file.contains("rlog") {
        if let Err(e) = crate::parse::parse_model_and_store(state, dongle, timestamp, segment, file, body).await {
            tracing::warn!(%dongle, ts = %timestamp, segment, "model parse failed: {e}");
        }
    }
    Ok(status)
}

/// Register an already-stored segment file into the DB without (re)storing the
/// blob: parse the qlog into route/segment rows + artifacts, or set the file's
/// `*_url` column. Used by `devsync` to reconcile blobs that are on disk but were
/// never parsed (e.g. a legacy manual import), and by `ingest_segment_file`.
/// For non-qlog files the `body` is unused (only the URL is registered).
pub async fn register_segment_file(
    state: &AppState,
    dongle: &str,
    timestamp: &str,
    segment: i64,
    file: &str,
    body: &[u8],
) -> AppResult<()> {
    if file.contains("qlog") {
        // parse_and_store also sets the segment's `qlog_url` (and any sibling cam
        // URLs already on disk), so devsync can key "parsed?" on `qlog_url` —
        // independent of whether the drive had a GPS fix.
        crate::parse::parse_and_store(state, dongle, timestamp, segment, file, body).await?;
    } else {
        crate::parse::set_segment_file(state, dongle, timestamp, segment, file).await?;
        let _ = crate::parse::recompute_route(state, dongle, timestamp).await;
    }
    Ok(())
}

/// PUT /connectincoming/:dongle/:timestamp/:segment/:file — driving-log segment.
pub async fn upload_driving(
    State(state): State<AppState>,
    Path((dongle, timestamp, segment, file)): Path<(String, String, i64, String)>,
    auth: Auth,
    body: Bytes,
) -> AppResult<StatusCode> {
    require_device(&auth, &dongle)?;
    let key = blob_key(&dongle, &timestamp, segment, &file);
    let status = store(&state, &dongle, &key, &body).await?;

    // Only the qlog is parsed (it has everything we browse). Everything else —
    // cameras AND the raw rlog — is just stored and its segment URL registered.
    if file.contains("qlog") {
        let st = state.clone();
        let (d, t, f) = (dongle.clone(), timestamp.clone(), file.clone());
        let bytes = body.to_vec();
        // Parse in the background so the device's upload returns promptly.
        tokio::spawn(async move {
            if let Err(e) = crate::parse::parse_and_store(&st, &d, &t, segment, &f, &bytes).await {
                tracing::error!(dongle = %d, ts = %t, segment, "parse failed: {e}");
            }
        });
    } else {
        // qcamera/fcamera/dcamera/ecamera/rlog → register the file's URL.
        if let Err(e) = crate::parse::set_segment_file(&state, &dongle, &timestamp, segment, &file).await {
            tracing::error!(%dongle, "set_segment_file failed: {e}");
        } else {
            let _ = crate::parse::recompute_route(&state, &dongle, &timestamp).await;
        }
    }
    Ok(status)
}

/// PUT /connectincoming/:dongle/boot/:file — bootlog.
pub async fn upload_boot(
    State(state): State<AppState>,
    Path((dongle, file)): Path<(String, String)>,
    auth: Auth,
    body: Bytes,
) -> AppResult<StatusCode> {
    require_device(&auth, &dongle)?;
    let key = format!("{dongle}_boot_{file}");
    store(&state, &dongle, &key, &body).await
}

/// PUT /connectincoming/:dongle/crash/:log_id/:commit/:name — crash log.
pub async fn upload_crash(
    State(state): State<AppState>,
    Path((dongle, log_id, commit, name)): Path<(String, String, String, String)>,
    auth: Auth,
    body: Bytes,
) -> AppResult<StatusCode> {
    require_device(&auth, &dongle)?;
    let key = format!("{dongle}_crash_{log_id}_{commit}_{name}");
    store(&state, &dongle, &key, &body).await
}
