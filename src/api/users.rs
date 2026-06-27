//! Local user endpoints: login, "me", and admin user creation.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::{self, AuthUser};
use crate::db::now_millis;
use crate::error::{AppError, AppResult};
use crate::models::User;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct LoginReq {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResp {
    pub access_token: String,
    pub identity: String,
    pub username: String,
    pub is_admin: bool,
}

/// POST /v1/auth/login — verify password, return an HS512 user JWT.
pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginReq>,
) -> AppResult<Json<LoginResp>> {
    let user: Option<User> = sqlx::query_as("SELECT * FROM users WHERE username = ?")
        .bind(&req.username)
        .fetch_optional(&state.pool)
        .await?;

    let user = user.ok_or_else(|| AppError::Unauthorized("invalid credentials".into()))?;

    if !auth::verify_password(&req.password, &user.password_hash) {
        return Err(AppError::Unauthorized("invalid credentials".into()));
    }

    let token = auth::issue_hs512(&state.config.jwt_secret_b64, &user.identity)
        .map_err(AppError::Other)?;

    Ok(Json(LoginResp {
        access_token: token,
        identity: user.identity,
        username: user.username,
        is_admin: user.is_admin != 0,
    }))
}

/// GET /v1/me — current user info.
pub async fn me(AuthUser(user): AuthUser) -> AppResult<Json<Value>> {
    Ok(Json(json!({
        "id": user.identity,
        "username": user.username,
        "email": user.email,
        "is_admin": user.is_admin != 0,
        "points": 0,
        "regdate": user.created_at / 1000,
    })))
}

#[derive(Deserialize)]
pub struct CreateUserReq {
    pub username: String,
    pub password: String,
    pub email: Option<String>,
    #[serde(default)]
    pub is_admin: bool,
}

/// POST /v1/admin/users — create a user. Requires an admin caller.
/// (The very first admin is created out-of-band via the `create-user` CLI
/// subcommand, so there's no anonymous bootstrap path over HTTP.)
pub async fn create_user(
    State(state): State<AppState>,
    AuthUser(caller): AuthUser,
    Json(req): Json<CreateUserReq>,
) -> AppResult<Json<Value>> {
    if caller.is_admin == 0 {
        return Err(AppError::Forbidden("admin required".into()));
    }
    create_user_row(
        &state,
        &req.username,
        &req.password,
        req.email.as_deref(),
        req.is_admin,
    )
    .await
}

/// Shared insert used by the HTTP handler and the CLI bootstrap.
pub async fn create_user_row(
    state: &AppState,
    username: &str,
    password: &str,
    email: Option<&str>,
    is_admin: bool,
) -> AppResult<Json<Value>> {
    if username.trim().is_empty() || password.len() < 6 {
        return Err(AppError::BadRequest(
            "username required, password >= 6 chars".into(),
        ));
    }

    let identity = Uuid::new_v4().to_string();
    let hash = auth::hash_password(password).map_err(AppError::Other)?;
    let now = now_millis();

    sqlx::query(
        "INSERT INTO users (identity, username, password_hash, email, is_admin, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&identity)
    .bind(username)
    .bind(&hash)
    .bind(email)
    .bind(is_admin as i64)
    .bind(now)
    .execute(&state.pool)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            AppError::BadRequest("username already exists".into())
        }
        other => AppError::Sqlx(other),
    })?;

    Ok(Json(json!({
        "identity": identity,
        "username": username,
        "is_admin": is_admin,
    })))
}

// ---- User management (admin), self password change ----------------------------

fn require_admin(user: &User) -> AppResult<()> {
    if user.is_admin == 0 {
        return Err(AppError::Forbidden("admin required".into()));
    }
    Ok(())
}

async fn user_by_identity(state: &AppState, identity: &str) -> AppResult<User> {
    sqlx::query_as("SELECT * FROM users WHERE identity = ?")
        .bind(identity)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown user".into()))
}

async fn admin_count(state: &AppState) -> AppResult<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE is_admin = 1")
        .fetch_one(&state.pool)
        .await?)
}

/// Hash + store a new password for a user (shared by self-change and admin-reset).
async fn store_password(state: &AppState, identity: &str, new: &str) -> AppResult<()> {
    if new.len() < 6 {
        return Err(AppError::BadRequest("password must be >= 6 chars".into()));
    }
    let hash = auth::hash_password(new).map_err(AppError::Other)?;
    sqlx::query("UPDATE users SET password_hash = ? WHERE identity = ?")
        .bind(&hash)
        .bind(identity)
        .execute(&state.pool)
        .await?;
    Ok(())
}

/// GET /v1/admin/users — list all users (admin only), with owned-device counts.
pub async fn list_users(
    State(state): State<AppState>,
    AuthUser(caller): AuthUser,
) -> AppResult<Json<Value>> {
    require_admin(&caller)?;
    #[derive(sqlx::FromRow)]
    struct Row {
        identity: String,
        username: String,
        email: Option<String>,
        is_admin: i64,
        created_at: i64,
        devices: i64,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT u.identity, u.username, u.email, u.is_admin, u.created_at, \
                (SELECT COUNT(*) FROM devices d WHERE d.owner_id = u.id) AS devices \
         FROM users u ORDER BY u.is_admin DESC, u.username ASC",
    )
    .fetch_all(&state.pool)
    .await?;
    let users: Vec<Value> = rows
        .into_iter()
        .map(|u| {
            json!({
                "id": u.identity,
                "username": u.username,
                "email": u.email,
                "is_admin": u.is_admin != 0,
                "regdate": u.created_at / 1000,
                "devices": u.devices,
                "self": u.identity == caller.identity,
            })
        })
        .collect();
    Ok(Json(json!({ "users": users })))
}

#[derive(Deserialize)]
pub struct UpdateUserReq {
    pub is_admin: Option<bool>,
    pub email: Option<String>,
}

/// POST /v1/admin/users/{identity} — change a user's admin flag and/or email.
pub async fn update_user(
    State(state): State<AppState>,
    Path(identity): Path<String>,
    AuthUser(caller): AuthUser,
    Json(req): Json<UpdateUserReq>,
) -> AppResult<Json<Value>> {
    require_admin(&caller)?;
    let target = user_by_identity(&state, &identity).await?;
    let new_admin = req.is_admin.unwrap_or(target.is_admin != 0);
    // Never leave the system with zero admins.
    if target.is_admin != 0 && !new_admin && admin_count(&state).await? <= 1 {
        return Err(AppError::BadRequest("cannot remove the last admin".into()));
    }
    let email = req.email.or(target.email);
    sqlx::query("UPDATE users SET is_admin = ?, email = ? WHERE identity = ?")
        .bind(new_admin as i64)
        .bind(&email)
        .bind(&identity)
        .execute(&state.pool)
        .await?;
    Ok(Json(json!({ "ok": true, "is_admin": new_admin })))
}

#[derive(Deserialize)]
pub struct AdminPasswordReq {
    pub password: String,
}

/// POST /v1/admin/users/{identity}/password — admin resets someone's password
/// (no current-password check; that's the admin-override path).
pub async fn admin_set_password(
    State(state): State<AppState>,
    Path(identity): Path<String>,
    AuthUser(caller): AuthUser,
    Json(req): Json<AdminPasswordReq>,
) -> AppResult<Json<Value>> {
    require_admin(&caller)?;
    user_by_identity(&state, &identity).await?; // 404 if unknown
    store_password(&state, &identity, &req.password).await?;
    Ok(Json(json!({ "ok": true })))
}

/// DELETE /v1/admin/users/{identity} — remove a user. Unclaims their devices
/// (owner → NULL) and drops their device shares first to satisfy FKs.
pub async fn delete_user(
    State(state): State<AppState>,
    Path(identity): Path<String>,
    AuthUser(caller): AuthUser,
) -> AppResult<Json<Value>> {
    require_admin(&caller)?;
    let target = user_by_identity(&state, &identity).await?;
    if target.identity == caller.identity {
        return Err(AppError::BadRequest("you can't delete your own account".into()));
    }
    if target.is_admin != 0 && admin_count(&state).await? <= 1 {
        return Err(AppError::BadRequest("cannot delete the last admin".into()));
    }
    let mut tx = state.pool.begin().await?;
    sqlx::query("DELETE FROM authorized_users WHERE user_id = ?")
        .bind(target.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE devices SET owner_id = NULL WHERE owner_id = ?")
        .bind(target.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(target.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct ChangePasswordReq {
    pub current_password: String,
    pub new_password: String,
}

/// POST /v1/me/password — any logged-in user changes their own password
/// (verifies the current one first).
pub async fn change_my_password(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(req): Json<ChangePasswordReq>,
) -> AppResult<Json<Value>> {
    if !auth::verify_password(&req.current_password, &user.password_hash) {
        return Err(AppError::Unauthorized("current password is incorrect".into()));
    }
    store_password(&state, &user.identity, &req.new_password).await?;
    Ok(Json(json!({ "ok": true })))
}
