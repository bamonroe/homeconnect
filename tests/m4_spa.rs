//! M4: the backend serves the built SPA at `/`, falls back to index.html for
//! client-side routes, and still lets the API take precedence. Requires the SPA
//! to have been built (web/dist) — skips gracefully if it hasn't.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use homeconnect::config::Config;
use tower::ServiceExt;

#[tokio::test]
async fn serves_spa_and_keeps_api_precedence() {
    let config = Config::from_env(); // default web_dir = ./web/dist (crate root)
    if !config.web_dir.join("index.html").exists() {
        eprintln!("skipping: web/dist not built (run `npm run build` in web/)");
        return;
    }
    // Use a temp data dir so we don't touch real data.
    let tmp = tempfile::tempdir().unwrap();
    let mut config = config;
    config.data_dir = tmp.path().to_path_buf();
    let state = homeconnect::build_state(config).await.unwrap();
    let app = homeconnect::router(state);

    // GET / → the SPA shell.
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp.headers().get("content-type").unwrap().to_str().unwrap().to_string();
    assert!(ct.contains("text/html"), "index served as html, got {ct}");
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("homeconnect"), "index references the app");

    // A client-side route falls back to index.html (SPA routing).
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/drive/abc").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("text/html"));

    // API routes are NOT shadowed by the SPA fallback.
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/v1/me").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "API still requires auth, not SPA");
}
