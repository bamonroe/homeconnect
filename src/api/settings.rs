//! Admin settings: view/edit the retention policy and storage stats.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::AuthUser;
use crate::error::{AppError, AppResult};
use crate::retention::{self, Policy};
use crate::state::AppState;
use crate::transcode;

fn require_admin(user: &crate::models::User) -> AppResult<()> {
    if user.is_admin == 0 {
        return Err(AppError::Forbidden("admin required".into()));
    }
    Ok(())
}

/// GET /v1/admin/retention — current policy + storage usage.
pub async fn get_retention(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> AppResult<Json<Value>> {
    require_admin(&user)?;
    let p = retention::load_policy(&state).await;
    let bytes = retention::storage_bytes(&state).await;
    let (routes,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM routes")
        .fetch_one(&state.pool)
        .await
        .unwrap_or((0,));
    Ok(Json(json!({
        "days": p.days,
        "max_drives": p.max_drives,
        "max_gb": p.max_gb,
        "storage_bytes": bytes,
        "storage_gb": (bytes as f64) / 1_000_000_000.0,
        "route_count": routes,
    })))
}

/// POST /v1/admin/retention — update policy (any field; 0 = unlimited).
pub async fn set_retention(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(p): Json<Policy>,
) -> AppResult<Json<Value>> {
    require_admin(&user)?;
    if p.days < 0 || p.max_drives < 0 || p.max_gb < 0.0 {
        return Err(AppError::BadRequest("values must be >= 0".into()));
    }
    retention::save_policy(&state, &p).await?;
    Ok(Json(json!({ "ok": true, "days": p.days, "max_drives": p.max_drives, "max_gb": p.max_gb })))
}

/// POST /v1/admin/retention/run — run a retention pass now.
pub async fn run_retention(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> AppResult<Json<Value>> {
    require_admin(&user)?;
    let deleted = retention::run_once(&state).await?;
    Ok(Json(json!({ "deleted": deleted })))
}

/// GET /v1/admin/transcode — current device + the selectable list.
pub async fn get_transcode(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> AppResult<Json<Value>> {
    require_admin(&user)?;
    let current = match transcode::current_device(&state).await {
        Some(d) => d,
        None => "cpu".to_string(),
    };
    let devices = transcode::list_devices().await;
    Ok(Json(json!({ "current": current, "devices": devices })))
}

#[derive(Deserialize)]
pub struct TranscodeReq {
    pub device: String,
}

/// POST /v1/admin/transcode — set the encode/decode device.
pub async fn set_transcode(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(req): Json<TranscodeReq>,
) -> AppResult<Json<Value>> {
    require_admin(&user)?;
    // Only accept "cpu" or a device the server actually enumerated.
    let valid = req.device == "cpu"
        || transcode::list_devices()
            .await
            .iter()
            .any(|d| d.value == req.device);
    if !valid {
        return Err(AppError::BadRequest("unknown transcode device".into()));
    }
    transcode::set_device(&state, &req.device).await?;
    Ok(Json(json!({ "ok": true, "device": req.device })))
}
