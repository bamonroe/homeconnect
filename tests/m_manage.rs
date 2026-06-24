//! Manage Data: download selected types as a stored zip, and delete selected
//! types off the server.

use std::io::{Cursor, Read};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use homeconnect::config::Config;
use homeconnect::storage::blob_key;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn jbody(r: axum::response::Response) -> Value {
    let b = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&b).unwrap_or(Value::Null)
}

#[tokio::test]
async fn download_zip_and_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config::from_env();
    config.data_dir = tmp.path().to_path_buf();
    let state = homeconnect::build_state(config).await.unwrap();
    let app = homeconnect::router(state.clone());

    // user + owned device + a 2-segment route with qcamera + qlog blobs.
    let _ = homeconnect::api::users::create_user_row(&state, "alice", "password1", None, true).await.unwrap();
    let (uid,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE username='alice'").fetch_one(&state.pool).await.unwrap();
    let dongle = "dev1";
    let ts = "2024-01-01--00-00-00";
    let full = format!("{dongle}|{ts}");
    sqlx::query("INSERT INTO devices (dongle_id, public_key, owner_id, created_at) VALUES (?, 'x', ?, 0)")
        .bind(dongle).bind(uid).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO routes (fullname, device_dongle_id, start_time_utc_millis, maxqlog, created_at) VALUES (?, ?, 1, 0, 0)")
        .bind(&full).bind(dongle).execute(&state.pool).await.unwrap();
    for n in 0..2i64 {
        sqlx::query("INSERT INTO segments (canonical_name, canonical_route_name, number, qcam_url, qlog_url, created_at) VALUES (?, ?, ?, 'u', 'u', 0)")
            .bind(format!("{full}--{n}")).bind(&full).bind(n).execute(&state.pool).await.unwrap();
        state.blobs.put(&blob_key(dongle, ts, n, "qcamera.ts"), &vec![1u8; 1000]).await.unwrap();
        state.blobs.put(&blob_key(dongle, ts, n, "qlog.zst"), &vec![2u8; 500]).await.unwrap();
    }

    // login
    let login = app.clone().oneshot(
        Request::builder().method("POST").uri("/v1/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(json!({"username":"alice","password":"password1"}).to_string())).unwrap()
    ).await.unwrap();
    let jwt = jbody(login).await["access_token"].as_str().unwrap().to_string();

    // Download qcamera + qlog as a zip (auth via ?sig, as the browser does).
    let resp = app.clone().oneshot(
        Request::builder()
            .uri(format!("/v1/route/{dongle}|{ts}/download?types=qcamera,qlog&sig={jwt}"))
            .body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get("content-type").unwrap(), "application/zip");
    let zbytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&zbytes[..2], b"PK", "is a zip");
    let mut zip = zip::ZipArchive::new(Cursor::new(zbytes.to_vec())).unwrap();
    assert_eq!(zip.len(), 4, "2 segments x (qcamera+qlog) = 4 entries");
    // entries are stored uncompressed and byte-identical
    let mut found = 0;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).unwrap();
        if f.name().ends_with("qcamera.ts") {
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).unwrap();
            assert_eq!(buf.len(), 1000);
            found += 1;
        }
    }
    assert_eq!(found, 2);

    // Delete just qcamera off the server.
    let resp = app.clone().oneshot(
        Request::builder().method("POST").uri(format!("/v1/route/{dongle}|{ts}/delete"))
            .header("Authorization", format!("JWT {jwt}")).header("content-type", "application/json")
            .body(Body::from(json!({"types":["qcamera"]}).to_string())).unwrap()
    ).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = jbody(resp).await;
    assert_eq!(v["removed"], 2, "2 qcamera blobs removed");
    assert_eq!(v["freed_bytes"], 2000);

    // qcamera gone, qlog intact.
    assert!(!state.blobs.exists(&blob_key(dongle, ts, 0, "qcamera.ts")).await);
    assert!(state.blobs.exists(&blob_key(dongle, ts, 0, "qlog.zst")).await);
    let (qcam,): (String,) = sqlx::query_as("SELECT qcam_url FROM segments WHERE canonical_name=?")
        .bind(format!("{full}--0")).fetch_one(&state.pool).await.unwrap();
    assert_eq!(qcam, "", "qcam_url cleared");
}
