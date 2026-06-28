//! Public drive sharing: an owner toggles a route public; then the route-info,
//! movies, and playlist endpoints serve it to an anonymous (logged-out) caller,
//! and stop again when unshared. Non-managers can't toggle it.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use homeconnect::config::Config;
use serde_json::{json, Value};
use tower::ServiceExt;

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
async fn get(app: &axum::Router, uri: &str, jwt: Option<&str>) -> axum::response::Response {
    let mut b = Request::builder().method("GET").uri(uri);
    if let Some(j) = jwt { b = b.header("authorization", format!("JWT {j}")); }
    app.clone().oneshot(b.body(Body::empty()).unwrap()).await.unwrap()
}
async fn post(app: &axum::Router, uri: &str, jwt: &str, body: Value) -> axum::response::Response {
    app.clone().oneshot(
        Request::builder().method("POST").uri(uri)
            .header("authorization", format!("JWT {jwt}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string())).unwrap(),
    ).await.unwrap()
}

#[tokio::test]
async fn public_share_flow() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config::from_env();
    config.data_dir = tmp.path().to_path_buf();
    let state = homeconnect::build_state(config).await.unwrap();
    let app = homeconnect::router(state.clone());

    let _ = homeconnect::api::users::create_user_row(&state, "alice", "password1", None, true).await.unwrap();
    let _ = homeconnect::api::users::create_user_row(&state, "bob", "password1", None, false).await.unwrap();
    let alice_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE username='alice'").fetch_one(&state.pool).await.unwrap();

    // A device owned by alice + one of her routes.
    sqlx::query("INSERT INTO devices (dongle_id, public_key, owner_id, device_type, created_at) VALUES ('dev_a','k',?,'tici',0)")
        .bind(alice_id).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO routes (fullname, device_dongle_id, maxqlog, created_at) VALUES ('dev_a|t1','dev_a',0,0)")
        .execute(&state.pool).await.unwrap();

    let alice = login(&app, "alice").await;
    let bob = login(&app, "bob").await;
    let info = "/v1/route/dev_a%7Ct1/info";       // %7C = '|'
    let m3u8 = "/v1/route/dev_a%7Ct1/qcamera.m3u8";
    let pubu = "/v1/route/dev_a%7Ct1/public";

    // Before sharing: owner sees it, anonymous does not.
    assert_eq!(get(&app, info, Some(&alice)).await.status(), StatusCode::OK, "owner reads info");
    assert_eq!(get(&app, info, None).await.status(), StatusCode::UNAUTHORIZED, "anon blocked pre-share");

    // Non-manager can't share it.
    assert_eq!(post(&app, pubu, &bob, json!({"public":true})).await.status(), StatusCode::FORBIDDEN, "bob can't share alice's drive");

    // Owner shares it.
    assert_eq!(post(&app, pubu, &alice, json!({"public":true})).await.status(), StatusCode::OK);

    // Now anonymous can read info (is_public true), movies, and the playlist.
    let r = get(&app, info, None).await;
    assert_eq!(r.status(), StatusCode::OK, "anon reads shared info");
    assert_eq!(jbody(r).await["is_public"], json!(true));
    assert_eq!(get(&app, "/v1/route/dev_a%7Ct1/movies", None).await.status(), StatusCode::OK, "anon reads movies");
    assert_eq!(get(&app, m3u8, None).await.status(), StatusCode::OK, "anon reads playlist");

    // Unshare → anonymous blocked again.
    assert_eq!(post(&app, pubu, &alice, json!({"public":false})).await.status(), StatusCode::OK);
    assert_eq!(get(&app, info, None).await.status(), StatusCode::UNAUTHORIZED, "anon blocked after unshare");
}
