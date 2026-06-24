//! Pairing: a user claims an unpaired (registered) device via the home-friendly
//! claim endpoint, and a device-signed pair token works via pilotpair.

use std::process::Command;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use homeconnect::config::Config;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{json, Value};
use tower::ServiceExt;

fn gen_ec_keypair(dir: &std::path::Path) -> (String, String) {
    let p = dir.join("k.pem");
    let pubp = dir.join("k.pub");
    assert!(Command::new("openssl")
        .args(["genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256", "-out"])
        .arg(&p).status().unwrap().success());
    assert!(Command::new("openssl")
        .args(["pkey", "-pubout", "-in"]).arg(&p).arg("-out").arg(&pubp)
        .status().unwrap().success());
    (std::fs::read_to_string(&p).unwrap(), std::fs::read_to_string(&pubp).unwrap())
}
fn sign(priv_pem: &str, claims: &Value) -> String {
    let k = EncodingKey::from_ec_pem(priv_pem.as_bytes()).unwrap();
    jsonwebtoken::encode(&Header::new(Algorithm::ES256), claims, &k).unwrap()
}
fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}
async fn jbody(r: axum::response::Response) -> Value {
    let b = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&b).unwrap_or(Value::Null)
}

async fn login(app: &axum::Router, user: &str) -> String {
    let r = app.clone().oneshot(
        Request::builder().method("POST").uri("/v1/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(json!({"username":user,"password":"password1"}).to_string())).unwrap(),
    ).await.unwrap();
    jbody(r).await["access_token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn claim_unpaired_then_pair_token() {
    let tmp = tempfile::tempdir().unwrap();
    let (priv1, pub1) = gen_ec_keypair(tmp.path());
    let mut config = Config::from_env();
    config.data_dir = tmp.path().to_path_buf();
    let state = homeconnect::build_state(config).await.unwrap();
    let app = homeconnect::router(state.clone());

    let _ = homeconnect::api::users::create_user_row(&state, "alice", "password1", None, true).await.unwrap();

    // An unpaired registered device (e.g. just ran pilotauth).
    sqlx::query("INSERT INTO devices (dongle_id, public_key, device_type, created_at) VALUES ('dev_a', ?, 'tici', 0)")
        .bind(&pub1).execute(&state.pool).await.unwrap();

    let jwt = login(&app, "alice").await;
    let bearer = format!("JWT {jwt}");

    // It shows up as unpaired.
    let r = app.clone().oneshot(
        Request::builder().uri("/v1/me/unpaired_devices").header("Authorization", &bearer).body(Body::empty()).unwrap()
    ).await.unwrap();
    let list = jbody(r).await;
    assert_eq!(list[0]["dongle_id"], "dev_a");

    // Claim it.
    let r = app.clone().oneshot(
        Request::builder().method("POST").uri("/v1/devices/dev_a/claim")
            .header("Authorization", &bearer).body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(jbody(r).await["first_pair"], true);

    // Now it's owned → no longer unpaired, and appears in my devices.
    let r = app.clone().oneshot(
        Request::builder().uri("/v1/me/unpaired_devices").header("Authorization", &bearer).body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(jbody(r).await.as_array().unwrap().len(), 0);

    let r = app.clone().oneshot(
        Request::builder().uri("/v1/me/devices").header("Authorization", &bearer).body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(jbody(r).await[0]["dongle_id"], "dev_a");

    // Claiming an already-paired device by a different user is rejected.
    let _ = homeconnect::api::users::create_user_row(&state, "bob", "password1", None, false).await.unwrap();
    let bob = login(&app, "bob").await;
    let r = app.clone().oneshot(
        Request::builder().method("POST").uri("/v1/devices/dev_a/claim")
            .header("Authorization", format!("JWT {bob}")).body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);

    // --- pilotpair (device-signed token) on a second device ---
    let d2 = tempfile::tempdir().unwrap();
    let (priv2, pub2) = gen_ec_keypair(d2.path());
    sqlx::query("INSERT INTO devices (dongle_id, public_key, device_type, created_at) VALUES ('dev_b', ?, 'tici', 0)")
        .bind(&pub2).execute(&state.pool).await.unwrap();
    let pair_token = sign(&priv2, &json!({"identity":"dev_b","pair":true,"exp":now()+3600}));
    let r = app.clone().oneshot(
        Request::builder().method("POST").uri("/v2/pilotpair")
            .header("Authorization", &bearer)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(format!("pair_token={pair_token}"))).unwrap()
    ).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v = jbody(r).await;
    assert_eq!(v["first_pair"], true);
    assert_eq!(v["dongle_id"], "dev_b");

    let _ = (priv1, pub1);
}
