//! /onboard.sh serves a host-templated setup script, publicly (no auth).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use homeconnect::config::Config;
use tower::ServiceExt;

#[tokio::test]
async fn onboard_script_is_templated_and_public() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config::from_env();
    config.data_dir = tmp.path().to_path_buf();
    config.public_url = "http://homeconnect.bam".into();
    let state = homeconnect::build_state(config).await.unwrap();
    let app = homeconnect::router(state);

    let resp = app
        .oneshot(Request::builder().uri("/onboard.sh").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "served without auth");
    let ct = resp.headers().get("content-type").unwrap().to_str().unwrap().to_string();
    assert!(ct.contains("shellscript"), "content-type {ct}");
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let s = String::from_utf8_lossy(&body);
    assert!(s.starts_with("#!/usr/bin/env bash"));
    assert!(s.contains("HOST=\"http://homeconnect.bam\""), "host baked in");
    assert!(s.contains("API_HOST=") && s.contains("ATHENA_HOST=") && s.contains("MAPS_HOST="));
    assert!(s.contains("DongleId"), "clears cached dongle");
    assert!(!s.contains("__HC_HOST__"), "placeholder substituted");
}
