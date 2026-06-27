//! Shared authorization helpers for browse/serve endpoints.

use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::{Device, User};
use crate::state::AppState;

/// May this user *manage* (view + control) the device? Owner, admin, or a
/// shared user (`authorized_users`). The single authorization check for the
/// owner-facing device endpoints (params, model, sync, manage data, routes).
pub async fn can_manage_loaded(state: &AppState, user: &User, device: &Device) -> AppResult<bool> {
    if user.is_admin != 0 || device.owner_id == Some(user.id) {
        return Ok(true);
    }
    let shared: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM authorized_users WHERE user_id = ? AND device_dongle_id = ?",
    )
    .bind(user.id)
    .bind(&device.dongle_id)
    .fetch_optional(&state.pool)
    .await?;
    Ok(shared.is_some())
}

/// Load the device and require the user can manage it; returns the device.
pub async fn can_manage_device(state: &AppState, user: &User, dongle: &str) -> AppResult<Device> {
    let device = load_device(state, dongle)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown device".into()))?;
    if can_manage_loaded(state, user, &device).await? {
        Ok(device)
    } else {
        Err(AppError::Forbidden("not authorized for device".into()))
    }
}

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
