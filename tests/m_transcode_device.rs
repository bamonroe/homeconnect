//! Admin transcode-device selection: list/get/set persists and validates.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use homeconnect::config::Config;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn jbody(r: axum::response::Response) -> Value {
    let b = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&b).unwrap_or(Value::Null)
}

#[tokio::test]
async fn transcode_device_select() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config::from_env();
    config.data_dir = tmp.path().to_path_buf();
    let state = homeconnect::build_state(config).await.unwrap();
    let app = homeconnect::router(state.clone());

    let _ = homeconnect::api::users::create_user_row(&state, "admin", "password1", None, true).await.unwrap();
    let login = app.clone().oneshot(
        Request::builder().method("POST").uri("/v1/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(json!({"username":"admin","password":"password1"}).to_string())).unwrap(),
    ).await.unwrap();
    let jwt = jbody(login).await["access_token"].as_str().unwrap().to_string();
    let bearer = format!("JWT {jwt}");

    // GET lists devices (at least CPU) and a current selection.
    let r = app.clone().oneshot(
        Request::builder().uri("/v1/admin/transcode").header("Authorization", &bearer).body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v = jbody(r).await;
    let devices = v["devices"].as_array().unwrap();
    assert!(devices.iter().any(|d| d["value"] == "cpu"), "CPU is always an option");

    // Set to cpu (always valid) → persists.
    let r = app.clone().oneshot(
        Request::builder().method("POST").uri("/v1/admin/transcode")
            .header("Authorization", &bearer).header("content-type", "application/json")
            .body(Body::from(json!({"device":"cpu"}).to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(homeconnect::transcode::current_device(&state).await, None, "cpu => None");

    // Unknown device is rejected.
    let r = app.clone().oneshot(
        Request::builder().method("POST").uri("/v1/admin/transcode")
            .header("Authorization", &bearer).header("content-type", "application/json")
            .body(Body::from(json!({"device":"/dev/dri/renderD999"}).to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);

    // Non-admin can't read it.
    let _ = homeconnect::api::users::create_user_row(&state, "bob", "password1", None, false).await.unwrap();
    let login = app.clone().oneshot(
        Request::builder().method("POST").uri("/v1/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(json!({"username":"bob","password":"password1"}).to_string())).unwrap(),
    ).await.unwrap();
    let bob = jbody(login).await["access_token"].as_str().unwrap().to_string();
    let r = app.clone().oneshot(
        Request::builder().uri("/v1/admin/transcode").header("Authorization", format!("JWT {bob}")).body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
}
