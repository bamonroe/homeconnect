//! Device-params local cache: edits land in the cache as `pending` and survive
//! updates, so the Device page is instant and offline-editable. (Flush/refresh go
//! over SSH and are exercised against a real device, not here.)

use homeconnect::config::Config;
use homeconnect::device_params as dp;

async fn test_state() -> homeconnect::state::AppState {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config::from_env();
    config.data_dir = tmp.path().to_path_buf();
    config.public_url = "http://test.local".into();
    std::mem::forget(tmp);
    homeconnect::build_state(config).await.unwrap()
}

#[tokio::test]
async fn cache_set_marks_pending_and_updates() {
    let state = test_state().await;
    let d = "dongle0";

    assert!(dp::cache_empty(&state, d).await);

    dp::cache_set(&state, d, "RecordAudio", "0").await.unwrap();
    assert!(!dp::cache_empty(&state, d).await);
    assert_eq!(dp::cache_all(&state, d).await, vec![("RecordAudio".into(), "0".into(), true)]);

    // Re-setting updates the value and keeps it pending.
    dp::cache_set(&state, d, "RecordAudio", "1").await.unwrap();
    assert_eq!(dp::cache_all(&state, d).await, vec![("RecordAudio".into(), "1".into(), true)]);

    // Another key is independent.
    dp::cache_set(&state, d, "ExperimentalMode", "0").await.unwrap();
    let mut all = dp::cache_all(&state, d).await;
    all.sort();
    assert_eq!(
        all,
        vec![
            ("ExperimentalMode".into(), "0".into(), true),
            ("RecordAudio".into(), "1".into(), true),
        ]
    );
}
