//! Device settings: read/write a curated allowlist of openpilot params over SSH.
//! Owner- or admin-only. See `crate::device_params` for the allowlist + safety.

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::auth::AuthUser;
use crate::device_params;
use crate::error::{AppError, AppResult};
use crate::models::{Device, User};
use crate::state::AppState;

async fn authorize_device(state: &AppState, user: &User, dongle: &str) -> AppResult<Device> {
    let device = crate::access::load_device(state, dongle)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown device".into()))?;
    if user.is_admin == 0 && device.owner_id != Some(user.id) {
        return Err(AppError::Forbidden("not your device".into()));
    }
    Ok(device)
}

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

/// GET /v1/devices/:dongle/params — the editable settings + current values.
pub async fn get_params(
    State(state): State<AppState>,
    Path(dongle): Path<String>,
    AuthUser(user): AuthUser,
) -> AppResult<Json<Value>> {
    let device = authorize_device(&state, &user, &dongle).await?;
    let online = device.online == 1 && !device.last_addr.is_empty();

    let mut values = Map::new();
    if online {
        match device_params::read_all(&state, &device.last_addr).await {
            Ok(pairs) => {
                for (k, v) in pairs {
                    values.insert(k, Value::String(v));
                }
            }
            // Reachability can lapse between the online flag and the SSH call;
            // surface specs anyway so the UI can render a disabled form.
            Err(e) => tracing::warn!(%dongle, "device params read: {e}"),
        }
    }

    Ok(Json(json!({
        "online": online && !values.is_empty(),
        "specs": specs_json(),
        "values": values,
    })))
}

#[derive(Deserialize)]
pub struct SetParam {
    pub key: String,
    pub value: String,
}

/// POST /v1/devices/:dongle/params — set one allowlisted param.
pub async fn set_param(
    State(state): State<AppState>,
    Path(dongle): Path<String>,
    AuthUser(user): AuthUser,
    Json(req): Json<SetParam>,
) -> AppResult<Json<Value>> {
    let device = authorize_device(&state, &user, &dongle).await?;
    if device.online != 1 || device.last_addr.is_empty() {
        return Err(AppError::BadRequest("device is offline".into()));
    }
    device_params::write(&state, &device.last_addr, &req.key, &req.value).await?;
    tracing::info!(user = %user.username, %dongle, key = %req.key, "device setting changed");
    Ok(Json(json!({ "ok": true, "key": req.key, "value": req.value })))
}
