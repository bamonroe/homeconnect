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

/// GET /v1/admin/sync — automatic-sync on/off + loop interval.
pub async fn get_sync(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> AppResult<Json<Value>> {
    require_admin(&user)?;
    Ok(Json(json!({
        "enabled": crate::devsync::is_enabled(&state).await,
        "interval_secs": crate::devsync::get_interval(&state).await,
        "types": crate::devsync::get_sync_types(&state).await,
        "all_types": crate::devsync::all_types(),
        "autoprune": crate::devsync::is_autoprune_enabled(&state).await,
    })))
}

#[derive(Deserialize)]
pub struct SyncSettings {
    pub enabled: Option<bool>,
    pub interval_secs: Option<u64>,
    pub types: Option<Vec<String>>,
    pub autoprune: Option<bool>,
}

/// POST /v1/admin/sync — update the on/off toggle and/or loop interval (runtime;
/// persists). Either field may be omitted.
pub async fn set_sync(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(req): Json<SyncSettings>,
) -> AppResult<Json<Value>> {
    require_admin(&user)?;
    if let Some(on) = req.enabled {
        crate::devsync::set_enabled(&state, on).await?;
        tracing::info!(user = %user.username, "sync {}", if on { "enabled" } else { "disabled" });
    }
    if let Some(secs) = req.interval_secs {
        crate::devsync::set_interval(&state, secs).await?;
        tracing::info!(user = %user.username, "sync interval set to {secs}s");
    }
    if let Some(types) = &req.types {
        crate::devsync::set_sync_types(&state, types).await?;
        tracing::info!(user = %user.username, "sync types set to {:?}", types);
    }
    if let Some(on) = req.autoprune {
        crate::devsync::set_autoprune(&state, on).await?;
        tracing::info!(user = %user.username, "device autoprune {}", if on { "on" } else { "off" });
    }
    Ok(Json(json!({
        "ok": true,
        "enabled": crate::devsync::is_enabled(&state).await,
        "interval_secs": crate::devsync::get_interval(&state).await,
        "types": crate::devsync::get_sync_types(&state).await,
        "all_types": crate::devsync::all_types(),
        "autoprune": crate::devsync::is_autoprune_enabled(&state).await,
    })))
}

/// GET /v1/admin/cam-calib — saved road-camera calibration for the model overlay
/// (effective qcamera intrinsics + small rpy offsets). Defaults if unset.
pub async fn get_cam_calib(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> AppResult<Json<Value>> {
    require_admin(&user)?;
    let v = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = 'cam_calib'")
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten();
    // Per-camera calibration. `h` = camera height above the road (m), added to the
    // path/lead z so they sit on the road. qcamera/fcamera pinhole; ecamera fisheye
    // (equidistant). qcamera is a uniform downscale of the 1928x1208 sensor (f 2648).
    let mut out = json!({
        "qcamera": {"fisheye": false, "fx": 722.4, "fy": 722.4, "cx": 263.0, "cy": 165.0, "pitch": 0.0, "yaw": 0.0, "roll": 0.0, "h": 1.2},
        "fcamera": {"fisheye": false, "fx": 1846.0, "fy": 1846.0, "cx": 672.0, "cy": 380.0, "pitch": 0.0, "yaw": 0.0, "roll": 0.0, "h": 1.2},
        "ecamera": {"fisheye": true, "fx": 395.0, "fy": 395.0, "cx": 672.0, "cy": 380.0, "pitch": 0.0, "yaw": 0.0, "roll": 0.0, "h": 1.2}
    });
    if let Some(mut saved) = v.and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
        // Legacy flat format (pre per-camera) → treat as the qcamera profile.
        if saved.get("qcamera").is_none() && saved.get("fx").is_some() {
            saved = json!({ "qcamera": saved });
        }
        if let (Some(o), Some(s)) = (out.as_object_mut(), saved.as_object()) {
            for (cam, cv) in s {
                let entry = o.entry(cam.clone()).or_insert_with(|| json!({}));
                if let (Some(eo), Some(so)) = (entry.as_object_mut(), cv.as_object()) {
                    for (k, val) in so {
                        eo.insert(k.clone(), val.clone());
                    }
                }
            }
        }
    }
    Ok(Json(out))
}

/// POST /v1/admin/cam-calib — save the calibration (stored verbatim as JSON).
pub async fn set_cam_calib(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(body): Json<Value>,
) -> AppResult<Json<Value>> {
    require_admin(&user)?;
    let s = serde_json::to_string(&body).unwrap_or_else(|_| "{}".into());
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES ('cam_calib', ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(s)
    .execute(&state.pool)
    .await?;
    Ok(Json(json!({ "ok": true })))
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
