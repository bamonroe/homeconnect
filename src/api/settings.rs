//! Admin settings: view/edit the retention policy and storage stats.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::auth::AuthUser;
use crate::error::{AppError, AppResult};
use crate::retention::{self, Policy};
use crate::state::AppState;

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
