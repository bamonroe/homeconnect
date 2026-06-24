//! Local user endpoints: login, "me", and admin user creation.

use axum::extract::State;
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
