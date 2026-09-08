//! Device software updates over SSH. Owner- or admin-only, device must be
//! online, and every mutating action requires the car to be off — see
//! `crate::device_update` for the gate and the underlying openpilot protocol.

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::access::can_manage_device;
use crate::auth::AuthUser;
use crate::device_update;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// GET /v1/devices/:dongle/update — live update status (read over SSH, so the
/// device has to be online; there's no cache because a stale "car is off" would
/// be exactly the wrong thing to act on).
pub async fn get_update(
    State(state): State<AppState>,
    Path(dongle): Path<String>,
    AuthUser(user): AuthUser,
) -> AppResult<Json<Value>> {
    let device = can_manage_device(&state, &user, &dongle).await?;
    if device.online == 0 || device.last_addr.is_empty() {
        return Ok(Json(json!({ "online": false })));
    }
    let status = device_update::status(&state, &device.last_addr).await?;
    Ok(Json(json!({ "online": true, "update": status })))
}

#[derive(Deserialize)]
pub struct UpdateAction {
    /// `check` | `download` | `install` | `reboot` | `branch` | `updates`
    pub action: String,
    /// Target branch, for `action = "branch"`.
    #[serde(default)]
    pub branch: String,
    /// Whether openpilot's own updater runs, for `action = "updates"`.
    #[serde(default)]
    pub enabled: bool,
}

/// POST /v1/devices/:dongle/update — check / download / install an update,
/// switch the target branch, or toggle openpilot's updater.
pub async fn post_update(
    State(state): State<AppState>,
    Path(dongle): Path<String>,
    AuthUser(user): AuthUser,
    Json(req): Json<UpdateAction>,
) -> AppResult<Json<Value>> {
    let device = can_manage_device(&state, &user, &dongle).await?;
    if device.online == 0 || device.last_addr.is_empty() {
        return Err(AppError::BadRequest("device must be online to update it".into()));
    }
    let addr = &device.last_addr;
    match req.action.as_str() {
        "check" => device_update::check(&state, addr).await?,
        "download" => device_update::download(&state, addr).await?,
        "install" => device_update::install(&state, addr).await?,
        "reboot" => device_update::reboot(&state, addr).await?,
        "branch" => device_update::set_branch(&state, addr, &req.branch).await?,
        "updates" => device_update::set_updates_enabled(&state, addr, req.enabled).await?,
        _ => return Err(AppError::BadRequest("unknown update action".into())),
    }
    tracing::info!(user = %user.username, %dongle, action = %req.action, "device update action");
    Ok(Json(json!({ "ok": true })))
}
