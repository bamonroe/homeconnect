//! Device settings: read/write a curated allowlist of openpilot params over SSH.
//! Owner- or admin-only. See `crate::device_params` for the allowlist + safety.

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::access::can_manage_device;
use crate::auth::AuthUser;
use crate::device_params;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

fn specs_json() -> Vec<Value> {
    device_params::SPECS
        .iter()
        .map(|s| {
            json!({
                "key": s.key,
                "label": s.label,
                "group": s.group,
                "kind": s.kind.as_str(),
                "help": s.help,
                "options": s.options.iter().map(|(v, l)| json!({"value": v, "label": l})).collect::<Vec<_>>(),
                "min": s.min,
                "max": s.max,
                "step": s.step,
                "unit": s.unit,
                "depends_on": if s.dep_key.is_empty() {
                    Value::Null
                } else {
                    json!({ "key": s.dep_key, "values": s.dep_values })
                },
            })
        })
        .collect()
}

/// GET /v1/devices/:dongle/params — editable settings + cached values. Served
/// from homeconnect's cache (instant), so it works even when the device is
/// offline. The cache is refreshed from the device on connect; first-ever load
/// populates it once if the device is reachable.
pub async fn get_params(
    State(state): State<AppState>,
    Path(dongle): Path<String>,
    AuthUser(user): AuthUser,
) -> AppResult<Json<Value>> {
    let device = can_manage_device(&state, &user, &dongle).await?;
    let online = device.online == 1 && !device.last_addr.is_empty();

    // One-time bootstrap: if we've never cached this device, read it now.
    if online && device_params::cache_empty(&state, &dongle).await {
        let _ = device_params::refresh(&state, &dongle, &device.last_addr).await;
    }

    let mut values = Map::new();
    let mut pending: Vec<Value> = Vec::new();
    for (k, v, is_pending) in device_params::cache_all(&state, &dongle).await {
        if is_pending {
            pending.push(Value::String(k.clone()));
        }
        values.insert(k, Value::String(v));
    }

    Ok(Json(json!({
        "online": online,
        "specs": specs_json(),
        "values": values,
        "pending": pending,
    })))
}

#[derive(Deserialize)]
pub struct SetParam {
    pub key: String,
    pub value: String,
}

/// POST /v1/devices/:dongle/params — set one allowlisted param. Writes the cache
/// (instant) and marks it pending; the value is flushed to the device now if it's
/// online, otherwise on its next connect.
pub async fn set_param(
    State(state): State<AppState>,
    Path(dongle): Path<String>,
    AuthUser(user): AuthUser,
    Json(req): Json<SetParam>,
) -> AppResult<Json<Value>> {
    let device = can_manage_device(&state, &user, &dongle).await?;
    if !device_params::is_writable(&req.key, &req.value) {
        return Err(AppError::BadRequest("not an editable setting, or invalid value".into()));
    }
    device_params::cache_set(&state, &dongle, &req.key, &req.value).await?;
    tracing::info!(user = %user.username, %dongle, key = %req.key, "device setting queued");

    // Flush promptly (in the background) if the device is reachable.
    let online = device.online == 1 && !device.last_addr.is_empty();
    if online {
        let (st, dg, addr) = (state.clone(), dongle.clone(), device.last_addr.clone());
        tokio::spawn(async move {
            let _ = device_params::flush(&st, &dg, &addr).await;
        });
    }
    Ok(Json(json!({ "ok": true, "key": req.key, "value": req.value, "online": online })))
}

/// GET /v1/devices/:dongle/model — the active driving model + selectable catalog.
/// Read live over SSH (no cache; the catalog is large and device-specific), so it
/// needs the device online.
pub async fn get_model(
    State(state): State<AppState>,
    Path(dongle): Path<String>,
    AuthUser(user): AuthUser,
) -> AppResult<Json<Value>> {
    let device = can_manage_device(&state, &user, &dongle).await?;
    if device.online == 0 || device.last_addr.is_empty() {
        return Ok(Json(json!({ "online": false })));
    }
    let model = crate::model_select::get_models(&state, &device.last_addr).await?;
    Ok(Json(json!({ "online": true, "model": model })))
}

#[derive(Deserialize)]
pub struct SetModel {
    /// Catalog index to switch to, or a negative value to revert to the default model.
    pub index: i64,
}

/// POST /v1/devices/:dongle/model — switch the driving model (or reset to default).
/// Device must be online; the switch downloads in the background on the device.
pub async fn set_model(
    State(state): State<AppState>,
    Path(dongle): Path<String>,
    AuthUser(user): AuthUser,
    Json(req): Json<SetModel>,
) -> AppResult<Json<Value>> {
    let device = can_manage_device(&state, &user, &dongle).await?;
    if device.online == 0 || device.last_addr.is_empty() {
        return Err(AppError::BadRequest("device must be online to change the model".into()));
    }
    if req.index < 0 {
        crate::model_select::use_default(&state, &device.last_addr).await?;
    } else {
        crate::model_select::select(&state, &device.last_addr, req.index).await?;
    }
    tracing::info!(user = %user.username, %dongle, index = req.index, "device model change requested");
    Ok(Json(json!({ "ok": true })))
}
