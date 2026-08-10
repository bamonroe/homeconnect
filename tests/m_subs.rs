//! Subtitles: the per-drive WebVTT transcript produced from the drive's audio.
//! Covers the status/serve/delete endpoints and their access rules — the whisper
//! call itself is exercised against the real server by hand (no network in tests),
//! so here the transcript blob is written directly, as `subs::build` would.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use homeconnect::config::Config;
use serde_json::Value;
use tower::ServiceExt;

async fn body_json(resp: axum::response::Response) -> Value {
    let b = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&b).unwrap_or(Value::Null)
}
async fn body_text(resp: axum::response::Response) -> String {
    let b = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    String::from_utf8_lossy(&b).to_string()
}

const VTT: &str = "WEBVTT\n\n00:00:01.000 --> 00:00:02.500\nTurning left onto Main Street.\n\n";

#[tokio::test]
async fn subtitles_are_reported_served_and_deletable() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config::from_env();
    config.data_dir = tmp.path().to_path_buf();
    let state = homeconnect::build_state(config).await.unwrap();
    let app = homeconnect::router(state.clone());

    let dongle = "dongle0";
    let ts = "2024-05-06--07-08-09";
    let fullname = format!("{dongle}|{ts}");

    let _ = homeconnect::api::users::create_user_row(&state, "alice", "password1", None, true)
        .await
        .unwrap();
    let (user_id,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE username='alice'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO devices (dongle_id, public_key, owner_id, created_at) VALUES (?, 'x', ?, 0)")
        .bind(dongle)
        .bind(user_id)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO routes (fullname, device_dongle_id, maxqlog, start_time_utc_millis, created_at) \
         VALUES (?, ?, 0, 0, 0)",
    )
    .bind(&fullname)
    .bind(dongle)
    .execute(&state.pool)
    .await
    .unwrap();

    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"username":"alice","password":"password1"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let jwt = body_json(login).await["access_token"].as_str().unwrap().to_string();
    let bearer = format!("JWT {jwt}");

    let subs_uri = format!("/v1/route/{}/subs", urlencoding(&fullname));
    let vtt_uri = format!("/v1/route/{}/subs.vtt", urlencoding(&fullname));

    // Nothing transcribed yet.
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(&subs_uri).header("Authorization", &bearer).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["ready"], false);

    let resp = app
        .clone()
        .oneshot(Request::builder().uri(&vtt_uri).header("Authorization", &bearer).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Write a transcript the way a build would, and record it.
    state
        .blobs
        .put(&homeconnect::subs::subs_key(dongle, ts), VTT.as_bytes())
        .await
        .unwrap();
    sqlx::query("INSERT INTO subs (fullname, seg_count, cues, bytes, built_at) VALUES (?, 1, 1, ?, 0)")
        .bind(&fullname)
        .bind(VTT.len() as i64)
        .execute(&state.pool)
        .await
        .unwrap();

    let resp = app
        .clone()
        .oneshot(Request::builder().uri(&subs_uri).header("Authorization", &bearer).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let st = body_json(resp).await;
    assert_eq!(st["ready"], true);
    assert_eq!(st["cues"], 1);

    // Served as WebVTT to the owner...
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(&vtt_uri).header("Authorization", &bearer).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("text/vtt"));
    assert!(body_text(resp).await.contains("Main Street"));

    // ...but not to an anonymous caller while the route is private.
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(&vtt_uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // A public share link carries its subtitles.
    sqlx::query("UPDATE routes SET is_public = 1 WHERE fullname = ?")
        .bind(&fullname)
        .execute(&state.pool)
        .await
        .unwrap();
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(&vtt_uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Deleting removes the blob and marks it so the sweep won't rebuild it.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&subs_uri)
                .header("Authorization", &bearer)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"action":"delete"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(!state.blobs.exists(&homeconnect::subs::subs_key(dongle, ts)).await);
    let (disabled,): (i64,) = sqlx::query_as("SELECT disabled FROM subs WHERE fullname = ?")
        .bind(&fullname)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(disabled, 1);

    let resp = app
        .clone()
        .oneshot(Request::builder().uri(&subs_uri).header("Authorization", &bearer).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let st = body_json(resp).await;
    assert_eq!(st["ready"], false);
    assert_eq!(st["disabled"], true);
}

/// The route fullname contains a `|`, which must be percent-encoded in a URI.
fn urlencoding(s: &str) -> String {
    s.replace('|', "%7C")
}
