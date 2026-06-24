//! v2 device endpoints: registration (pilotauth) and pairing (pilotpair).

use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::auth::{self, AuthUser};
use crate::db::now_millis;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct PilotAuthParams {
    pub imei: String,
    pub imei2: String,
    pub serial: String,
    pub public_key: String,
    pub register_token: String,
}

/// Claims inside the device-signed register token.
#[derive(Deserialize)]
struct RegisterClaims {
    #[serde(default)]
    register: bool,
}

/// dongle_id = first 16 hex chars of sha256(imei ++ imei2 ++ serial ++ public_key).
fn derive_dongle_id(p: &PilotAuthParams) -> String {
    let mut hasher = Sha256::new();
    hasher.update(p.imei.as_bytes());
    hasher.update(p.imei2.as_bytes());
    hasher.update(p.serial.as_bytes());
    hasher.update(p.public_key.as_bytes());
    let hex = hex::encode(hasher.finalize());
    hex[0..16].to_string()
}

/// POST /v2/pilotauth — params in the QUERY STRING. Verifies the register
/// token against the supplied public key, then upserts the device row.
pub async fn pilotauth(
    State(state): State<AppState>,
    Query(params): Query<PilotAuthParams>,
) -> AppResult<Json<Value>> {
    // The register token must be signed by the device's own key and assert
    // `register: true`.
    let claims: RegisterClaims =
        auth::verify_device_token(&params.public_key, &params.register_token)?;
    if !claims.register {
        return Err(AppError::Unauthorized("register claim missing".into()));
    }

    let dongle_id = derive_dongle_id(&params);
    let now = now_millis();

    // Upsert: a re-registering device (same identity) keeps its owner/aliases.
    sqlx::query(
        "INSERT INTO devices \
           (dongle_id, serial, imei, imei2, public_key, uploads_allowed, alias, device_type, created_at, last_athena_ping) \
         VALUES (?, ?, ?, ?, ?, 1, '', 'unknown', ?, 0) \
         ON CONFLICT(dongle_id) DO UPDATE SET \
           serial = excluded.serial, imei = excluded.imei, imei2 = excluded.imei2, \
           public_key = excluded.public_key",
    )
    .bind(&dongle_id)
    .bind(&params.serial)
    .bind(&params.imei)
    .bind(&params.imei2)
    .bind(&params.public_key)
    .bind(now)
    .execute(&state.pool)
    .await?;

    tracing::info!(%dongle_id, "device registered");
    Ok(Json(json!({ "dongle_id": dongle_id, "access_token": "" })))
}

#[derive(Deserialize)]
pub struct PilotPairForm {
    pub pair_token: String,
}

/// Claims inside the device-signed pair token.
#[derive(Deserialize)]
struct PairClaims {
    identity: String,
    #[serde(default)]
    pair: bool,
}

/// POST /v2/pilotpair — a logged-in user claims a device. The form carries a
/// device-signed `pair_token` proving possession of the device; we verify it
/// against the device's stored public key and set ownership on first pair.
pub async fn pilotpair(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    axum::extract::Form(form): axum::extract::Form<PilotPairForm>,
) -> AppResult<Json<Value>> {
    // We need the dongle to fetch its key, but the dongle is *inside* the
    // token. Read it unverified first, then verify against the stored key.
    let unverified: PairClaims = auth::decode_claims_unverified(&form.pair_token)
        .ok_or_else(|| AppError::BadRequest("undecodable pair token".into()))?;

    let device = sqlx::query_as::<_, crate::models::Device>(
        "SELECT * FROM devices WHERE dongle_id = ?",
    )
    .bind(&unverified.identity)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("unknown device".into()))?;

    let claims: PairClaims = auth::verify_device_token(&device.public_key, &form.pair_token)?;
    if !claims.pair {
        return Err(AppError::Unauthorized("pair claim missing".into()));
    }

    let first_pair = device.owner_id.is_none();
    if first_pair {
        sqlx::query("UPDATE devices SET owner_id = ? WHERE dongle_id = ?")
            .bind(user.id)
            .bind(&device.dongle_id)
            .execute(&state.pool)
            .await?;
        // Owner is implicitly authorized.
        let _ = sqlx::query(
            "INSERT OR IGNORE INTO authorized_users (user_id, device_dongle_id) VALUES (?, ?)",
        )
        .bind(user.id)
        .bind(&device.dongle_id)
        .execute(&state.pool)
        .await;
        tracing::info!(dongle = %device.dongle_id, user = %user.username, "device paired");
    }

    Ok(Json(json!({
        "first_pair": first_pair,
        "dongle_id": device.dongle_id,
    })))
}
