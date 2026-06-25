//! Devsync: the shared ingest core (`ingest::ingest_segment_file`) that the SSH
//! puller feeds pulled files through. The SSH-specific listing/tier logic is unit
//! tested inline in `src/devsync.rs`; here we assert that a pulled file lands in
//! the blob store and registers its segment/route exactly like an HTTP upload,
//! and that a re-pull is idempotent.

use homeconnect::config::Config;

async fn test_state() -> homeconnect::state::AppState {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config::from_env();
    config.data_dir = tmp.path().to_path_buf();
    config.public_url = "http://test.local".into();
    // Leak the tempdir so it outlives the test (state holds paths into it).
    std::mem::forget(tmp);
    homeconnect::build_state(config).await.unwrap()
}

#[tokio::test]
async fn ingest_segment_file_registers_camera_and_is_idempotent() {
    let state = test_state().await;
    sqlx::query("INSERT INTO devices (dongle_id, public_key, created_at) VALUES ('dongle0','x',0)")
        .execute(&state.pool)
        .await
        .unwrap();

    let dongle = "dongle0";
    // The device's on-disk route name (counter--hash) is used verbatim as the ts.
    let ts = "00000009--f3d1ef15b7";
    let body = b"\x47fake-qcamera-ts-bytes".to_vec();

    let st = homeconnect::ingest::ingest_segment_file(&state, dongle, ts, 5, "qcamera.ts", &body)
        .await
        .expect("ingest");
    assert_eq!(st, axum::http::StatusCode::CREATED);

    // Segment row exists with the qcamera URL registered.
    let (qcam,): (String,) =
        sqlx::query_as("SELECT qcam_url FROM segments WHERE canonical_name = ?")
            .bind(format!("{dongle}|{ts}--5"))
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(
        qcam,
        format!("http://test.local/connectdata/qcam/{dongle}/{ts}/5/qcamera.ts")
    );

    // Route row was created lazily.
    let (cnt,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM routes WHERE fullname = ?")
        .bind(format!("{dongle}|{ts}"))
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(cnt, 1);

    // Blob is on disk.
    let key = homeconnect::storage::blob_key(dongle, ts, 5, "qcamera.ts");
    assert!(state.blobs.exists(&key).await);

    // Re-pulling the same file is refused — the puller treats this as "already
    // have it" (so its diff/exists check and this guard agree).
    let again =
        homeconnect::ingest::ingest_segment_file(&state, dongle, ts, 5, "qcamera.ts", &body).await;
    assert!(again.is_err(), "second ingest of an existing blob should error");
}

#[tokio::test]
async fn sync_enabled_toggle_defaults_on_and_persists() {
    let state = test_state().await;
    // Unset → falls back to config default (HC_SYNC_ENABLED, default on).
    assert!(homeconnect::devsync::is_enabled(&state).await);

    homeconnect::devsync::set_enabled(&state, false).await.unwrap();
    assert!(!homeconnect::devsync::is_enabled(&state).await);

    homeconnect::devsync::set_enabled(&state, true).await.unwrap();
    assert!(homeconnect::devsync::is_enabled(&state).await);

    // Interval: unset → config default; settable at runtime.
    let default = homeconnect::devsync::get_interval(&state).await;
    assert_eq!(default, 60, "config default interval");
    homeconnect::devsync::set_interval(&state, 300).await.unwrap();
    assert_eq!(homeconnect::devsync::get_interval(&state).await, 300);
    homeconnect::devsync::set_interval(&state, 0).await.unwrap();
    assert_eq!(homeconnect::devsync::get_interval(&state).await, 0);

    // Default sync types: unset → just qcamera; settable; unknown tokens dropped.
    assert_eq!(homeconnect::devsync::get_sync_types(&state).await, vec!["qcamera".to_string()]);
    homeconnect::devsync::set_sync_types(&state, &["fcamera".into(), "bogus".into(), "rlog".into()])
        .await
        .unwrap();
    assert_eq!(
        homeconnect::devsync::get_sync_types(&state).await,
        vec!["fcamera".to_string(), "rlog".to_string()]
    );
}
