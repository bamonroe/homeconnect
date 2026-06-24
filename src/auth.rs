//! Auth: password hashing, user (HS512) + device (ES256/RS256) JWTs, and the
//! `Auth` request extractor that mirrors comma's token handling.

use axum::extract::{FromRequestParts, OptionalFromRequestParts};
use axum::http::request::Parts;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::db::now_millis;
use crate::error::AppError;
use crate::models::{Device, User};
use crate::state::AppState;

/// Standard claims used by both user and device JWTs. `nbf`/`iat` are optional
/// because device tokens in the wild don't always include them; `exp` is always
/// present and validated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub identity: String, // user uuid OR device dongle_id
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nbf: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iat: Option<usize>,
    pub exp: usize,
}

const USER_TOKEN_TTL_SECS: i64 = 60 * 60 * 24 * 30; // 30 days

/// Issue an HS512 user/server token for `identity` (default 30-day TTL).
pub fn issue_hs512(secret_b64: &str, identity: &str) -> anyhow::Result<String> {
    issue_hs512_ttl(secret_b64, identity, USER_TOKEN_TTL_SECS)
}

/// Issue an HS512 token for `identity` with a custom TTL (seconds).
pub fn issue_hs512_ttl(secret_b64: &str, identity: &str, ttl_secs: i64) -> anyhow::Result<String> {
    let now = now_millis() / 1000;
    let claims = Claims {
        identity: identity.to_string(),
        nbf: Some((now - 5) as usize),
        iat: Some(now as usize),
        exp: (now + ttl_secs) as usize,
    };
    let key = EncodingKey::from_base64_secret(secret_b64)?;
    Ok(jsonwebtoken::encode(&Header::new(Algorithm::HS512), &claims, &key)?)
}

// ---- passwords (argon2) -------------------------------------------------

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hash error: {e}"))?
        .to_string();
    Ok(hash)
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

// ---- token verification + extractor ------------------------------------

/// Authenticated principal extracted from a request. Exactly one of
/// `user`/`device` is set (or neither, if the identity isn't found).
#[derive(Debug, Clone)]
pub struct Auth {
    pub claims: Claims,
    pub user: Option<User>,
    pub device: Option<Device>,
}

fn extract_token(parts: &Parts) -> Option<String> {
    let query = parts.uri.query().unwrap_or("");
    let qget = |k: &str| {
        query.split('&').find_map(|kv| {
            let (key, val) = kv.split_once('=')?;
            if key == k { Some(val.to_string()) } else { None }
        })
    };
    // 1. ?sig=
    if let Some(t) = qget("sig") {
        return Some(t);
    }
    // 2. cookie jwt=
    if let Some(cookie) = parts.headers.get(axum::http::header::COOKIE) {
        if let Ok(s) = cookie.to_str() {
            for part in s.split(';') {
                if let Some((k, v)) = part.trim().split_once('=') {
                    if k == "jwt" {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    // 3. Authorization: JWT <tok>
    if let Some(h) = parts.headers.get(axum::http::header::AUTHORIZATION) {
        if let Ok(s) = h.to_str() {
            if let Some(rest) = s.strip_prefix("JWT ") {
                return Some(rest.trim().to_string());
            }
            if let Some(rest) = s.strip_prefix("Bearer ") {
                return Some(rest.trim().to_string());
            }
        }
    }
    // 4. ?access_token=
    qget("access_token")
}

/// Decode claims WITHOUT verifying the signature (to read `identity` first).
fn decode_unverified(token: &str) -> Option<Claims> {
    decode_claims_unverified(token)
}

/// Generic unverified claim decode — reads claims to discover the device
/// identity before we know which key to verify against. NEVER trust the result
/// for authorization; always re-verify with the resolved key.
///
/// We decode the JWT payload segment directly rather than going through
/// `jsonwebtoken::decode`, which (in v10) still wants a usable key/provider even
/// with signature validation disabled.
pub fn decode_claims_unverified<T: serde::de::DeserializeOwned>(token: &str) -> Option<T> {
    use base64::Engine;
    let payload_b64 = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Verify a token and resolve the principal. Errors map to 401.
pub async fn verify(state: &AppState, token: &str) -> Result<Auth, AppError> {
    let header = decode_header(token).map_err(|e| AppError::Unauthorized(format!("bad token: {e}")))?;
    let alg = header.alg;

    let mut validation = Validation::new(alg);
    validation.leeway = 300;

    match alg {
        Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512 => {
            let key = DecodingKey::from_base64_secret(&state.config.jwt_secret_b64)
                .map_err(|e| AppError::Other(anyhow::anyhow!("bad secret: {e}")))?;
            let data = decode::<Claims>(token, &key, &validation)
                .map_err(|e| AppError::Unauthorized(format!("invalid token: {e}")))?;
            resolve_principal(state, data.claims).await
        }
        Algorithm::ES256 | Algorithm::ES384 | Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512 => {
            // Device token: read identity unverified, look up the device key, verify.
            let unverified = decode_unverified(token)
                .ok_or_else(|| AppError::Unauthorized("undecodable token".into()))?;
            let device: Device = sqlx::query_as("SELECT * FROM devices WHERE dongle_id = ?")
                .bind(&unverified.identity)
                .fetch_optional(&state.pool)
                .await?
                .ok_or_else(|| AppError::Unauthorized("unknown device".into()))?;
            let key = device_decoding_key(alg, &device.public_key)?;
            let data = decode::<Claims>(token, &key, &validation)
                .map_err(|e| AppError::Unauthorized(format!("device token invalid: {e}")))?;
            Ok(Auth {
                claims: data.claims,
                user: None,
                device: Some(device),
            })
        }
        other => Err(AppError::Unauthorized(format!("unsupported alg {other:?}"))),
    }
}

/// Verify a device-signed JWT against a supplied PEM public key, returning its
/// claims. Used by pilotauth/pilotpair where the device isn't (fully) in the DB
/// yet, so the key comes from the request, not a lookup. The algorithm is read
/// from the token header (ES256/RS256 family).
pub fn verify_device_token<T: serde::de::DeserializeOwned>(
    pem: &str,
    token: &str,
) -> Result<T, AppError> {
    let header = decode_header(token).map_err(|e| AppError::Unauthorized(format!("bad token: {e}")))?;
    let alg = header.alg;
    let key = device_decoding_key(alg, pem)?;
    let mut validation = Validation::new(alg);
    validation.leeway = 300;
    // These tokens carry custom claims (register/pair), not the standard set.
    validation.required_spec_claims.clear();
    validation.validate_aud = false;
    decode::<T>(token, &key, &validation)
        .map(|d| d.claims)
        .map_err(|e| AppError::Unauthorized(format!("device token invalid: {e}")))
}

fn device_decoding_key(alg: Algorithm, pem: &str) -> Result<DecodingKey, AppError> {
    let r = match alg {
        Algorithm::ES256 | Algorithm::ES384 => DecodingKey::from_ec_pem(pem.as_bytes()),
        _ => DecodingKey::from_rsa_pem(pem.as_bytes()),
    };
    r.map_err(|e| AppError::Unauthorized(format!("bad device key: {e}")))
}

async fn resolve_principal(state: &AppState, claims: Claims) -> Result<Auth, AppError> {
    // identity may be a device dongle_id or a user uuid.
    if let Some(device) = sqlx::query_as::<_, Device>("SELECT * FROM devices WHERE dongle_id = ?")
        .bind(&claims.identity)
        .fetch_optional(&state.pool)
        .await?
    {
        return Ok(Auth { claims, user: None, device: Some(device) });
    }
    if let Some(user) = sqlx::query_as::<_, User>("SELECT * FROM users WHERE identity = ?")
        .bind(&claims.identity)
        .fetch_optional(&state.pool)
        .await?
    {
        return Ok(Auth { claims, user: Some(user), device: None });
    }
    Err(AppError::Unauthorized("unknown identity".into()))
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let token = extract_token(parts).ok_or_else(|| AppError::Unauthorized("missing token".into()))?;
        verify(state, &token).await
    }
}

/// Optional auth: yields `None` (instead of rejecting) when no/invalid token is
/// present — used by endpoints that also serve public routes anonymously.
impl OptionalFromRequestParts<AppState> for Auth {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Option<Self>, Self::Rejection> {
        match extract_token(parts) {
            Some(token) => Ok(verify(state, &token).await.ok()),
            None => Ok(None),
        }
    }
}

/// Extractor that requires a logged-in *user* (rejects device tokens).
pub struct AuthUser(pub User);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let auth = <Auth as FromRequestParts<AppState>>::from_request_parts(parts, state).await?;
        match auth.user {
            Some(u) => Ok(AuthUser(u)),
            None => Err(AppError::Unauthorized("user login required".into())),
        }
    }
}
