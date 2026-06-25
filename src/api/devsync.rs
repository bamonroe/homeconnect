//! Manual device-sync trigger + queue stats. A request enqueues a scan and
//! returns immediately (non-blocking); background workers do the pulling, and the
//! UI polls `queue_stats` for progress. Authorized to the device owner or admin.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::AuthUser;
use crate::devsync::{self, SyncOpts};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct SyncQuery {
    /// Pull full-res cameras + rlog (accepts `1`/`true`/`yes`).
    pub full: Option<String>,
    /// Limit to a single route (the `{ts}` portion of the route name).
    pub route: Option<String>,
    /// Explicit, comma-separated data types to pull (overrides `full`/defaults),
    /// e.g. `fcamera,dcamera`. `qlog` is always pulled regardless.
    pub types: Option<String>,
}

/// POST /v1/devices/:dongle/sync — pull new drives off the device now.
pub async fn sync_now(
    State(state): State<AppState>,
    Path(dongle): Path<String>,
    AuthUser(user): AuthUser,
    Query(q): Query<SyncQuery>,
) -> AppResult<Json<Value>> {
    let device = crate::access::load_device(&state, &dongle)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown device".into()))?;
    if user.is_admin == 0 && device.owner_id != Some(user.id) {
        return Err(AppError::Forbidden("not your device".into()));
    }

    // Precedence: explicit `types` > `full` (all) > per-route resolution (None).
    let fullres = matches!(q.full.as_deref(), Some("1" | "true" | "yes"));
    let types: Option<Vec<String>> = match q.types.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(list) => {
            let req: Vec<&str> = list.split(',').map(str::trim).collect();
            // Keep only known types (qlog is always pulled, so it needn't be listed).
            Some(devsync::all_types().into_iter().filter(|t| req.contains(&t.as_str())).collect())
        }
        None if fullres => Some(devsync::all_types()),
        None => None, // resolve per route (override else global default)
    };
    let opts = SyncOpts { types, route: q.route.clone() };

    // Non-blocking: scan + enqueue in the background, return immediately. The
    // workers pull; the UI watches the queue counter for progress.
    let online = device.online == 1;
    tokio::spawn(async move {
        match devsync::scan(&state, &device, opts).await {
            Ok(n) => tracing::info!(dongle = %device.dongle_id, "manual sync: queued {n} files"),
            Err(e) => tracing::warn!(dongle = %device.dongle_id, "manual sync: {e}"),
        }
    });
    Ok(Json(json!({ "ok": true, "online": online })))
}

/// GET /v1/sync/queue — how much is queued/in-flight, for the header counter.
pub async fn queue_stats(
    State(state): State<AppState>,
    AuthUser(_user): AuthUser,
) -> AppResult<Json<Value>> {
    let (drives, files) = state.sync_queue.stats().await;
    Ok(Json(json!({ "drives": drives, "files": files })))
}

/// Authorize a per-route action: the caller owns the route's device (or is admin).
async fn authorize_route(state: &AppState, user: &crate::models::User, fullname: &str) -> AppResult<()> {
    let dongle = fullname
        .split_once('|')
        .map(|(d, _)| d)
        .ok_or_else(|| AppError::BadRequest("bad route name".into()))?;
    let device = crate::access::load_device(state, dongle)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown device".into()))?;
    if user.is_admin == 0 && device.owner_id != Some(user.id) {
        return Err(AppError::Forbidden("not your device".into()));
    }
    Ok(())
}

/// GET /v1/route/:fullname/sync — this drive's effective synced types, whether
/// it's a per-drive override or the inherited default, and the choices.
pub async fn get_route_sync(
    State(state): State<AppState>,
    Path(fullname): Path<String>,
    AuthUser(user): AuthUser,
) -> AppResult<Json<Value>> {
    authorize_route(&state, &user, &fullname).await?;
    let over = devsync::get_route_override(&state, &fullname).await;
    let default = devsync::get_sync_types(&state).await;
    Ok(Json(json!({
        "types": over.clone().unwrap_or_else(|| default.clone()),
        "overridden": over.is_some(),
        "default": default,
        "all_types": devsync::all_types(),
    })))
}

#[derive(Deserialize)]
pub struct RouteSyncReq {
    /// Set this drive's override to these types.
    pub types: Option<Vec<String>>,
    /// Clear the override so the drive inherits the global default.
    pub reset: Option<bool>,
}

/// POST /v1/route/:fullname/sync — set or clear this drive's type override.
pub async fn set_route_sync(
    State(state): State<AppState>,
    Path(fullname): Path<String>,
    AuthUser(user): AuthUser,
    Json(req): Json<RouteSyncReq>,
) -> AppResult<Json<Value>> {
    authorize_route(&state, &user, &fullname).await?;
    if req.reset.unwrap_or(false) {
        devsync::set_route_override(&state, &fullname, None).await?;
    } else if let Some(types) = &req.types {
        devsync::set_route_override(&state, &fullname, Some(types)).await?;
    }
    let over = devsync::get_route_override(&state, &fullname).await;
    let default = devsync::get_sync_types(&state).await;
    Ok(Json(json!({
        "ok": true,
        "types": over.clone().unwrap_or_else(|| default.clone()),
        "overridden": over.is_some(),
    })))
}
