//! Shared authorization helpers for browse/serve endpoints.

use crate::auth::Auth;
use crate::error::AppResult;
use crate::models::Device;
use crate::state::AppState;

/// May this principal view the given device's data? True for the device itself,
/// its owner, or a user it's been shared with (authorized_users).
pub async fn can_view_device(state: &AppState, auth: &Auth, device: &Device) -> AppResult<bool> {
    if let Some(d) = &auth.device {
        return Ok(d.dongle_id == device.dongle_id);
    }
    if let Some(u) = &auth.user {
        if device.owner_id == Some(u.id) {
            return Ok(true);
        }
        let shared: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM authorized_users WHERE user_id = ? AND device_dongle_id = ?",
        )
        .bind(u.id)
        .bind(&device.dongle_id)
        .fetch_optional(&state.pool)
        .await?;
        return Ok(shared.is_some());
    }
    Ok(false)
}

pub async fn load_device(state: &AppState, dongle: &str) -> AppResult<Option<Device>> {
    Ok(sqlx::query_as::<_, Device>("SELECT * FROM devices WHERE dongle_id = ?")
        .bind(dongle)
        .fetch_optional(&state.pool)
        .await?)
}

/// May this principal view this dongle's data (resolves the device first)?
pub async fn can_view_dongle(state: &AppState, auth: &Auth, dongle: &str) -> AppResult<bool> {
    match load_device(state, dongle).await? {
        Some(d) => can_view_device(state, auth, &d).await,
        None => Ok(false),
    }
}

/// May this (possibly anonymous) principal view a route? Public routes are
/// visible to anyone; otherwise the device-view rules apply.
pub async fn can_view_route(state: &AppState, auth: &Auth, dongle: &str) -> AppResult<bool> {
    can_view_dongle(state, auth, dongle).await
}
