//! M6: retention prunes drives by age, per-device count, and total size, and
//! deletes the associated blobs + rows.

use homeconnect::config::Config;
use homeconnect::retention::{self, Policy};
use homeconnect::storage::blob_key;

async fn mk_route(state: &homeconnect::state::AppState, dongle: &str, ts: &str, start_ms: i64, blob_len: usize) {
    let fullname = format!("{dongle}|{ts}");
    sqlx::query("INSERT INTO routes (fullname, device_dongle_id, start_time_utc_millis, maxqlog, created_at) VALUES (?, ?, ?, 0, ?)")
        .bind(&fullname).bind(dongle).bind(start_ms).bind(start_ms)
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO segments (canonical_name, canonical_route_name, number, qcam_url, created_at) VALUES (?, ?, 0, 'u', ?)")
        .bind(format!("{fullname}--0")).bind(&fullname).bind(start_ms)
        .execute(&state.pool).await.unwrap();
    // a blob so storage accounting has something to measure/delete
    state.blobs.put(&blob_key(dongle, ts, 0, "qcamera.ts"), &vec![7u8; blob_len]).await.unwrap();
}

async fn route_exists(state: &homeconnect::state::AppState, fullname: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM routes WHERE fullname = ?")
        .bind(fullname).fetch_one(&state.pool).await.unwrap() > 0
}

async fn new_state(tmp: &std::path::Path) -> homeconnect::state::AppState {
    let mut config = Config::from_env();
    config.data_dir = tmp.to_path_buf();
    // disable env-driven limits; tests set policy explicitly
    config.retain_days = 0; config.retain_max_drives = 0; config.retain_gb = 0.0;
    homeconnect::build_state(config).await.unwrap()
}

const DAY: i64 = 86_400_000;

#[tokio::test]
async fn prunes_by_age() {
    let tmp = tempfile::tempdir().unwrap();
    let state = new_state(tmp.path()).await;
    sqlx::query("INSERT INTO devices (dongle_id, public_key, created_at) VALUES ('d', 'x', 0)").execute(&state.pool).await.unwrap();
    let now = homeconnect::db::now_millis();
    mk_route(&state, "d", "old", now - 40 * DAY, 100).await;
    mk_route(&state, "d", "recent", now - 2 * DAY, 100).await;

    retention::save_policy(&state, &Policy { days: 30, max_drives: 0, max_gb: 0.0 }).await.unwrap();
    let n = retention::run_once(&state).await.unwrap();
    assert_eq!(n, 1);
    assert!(!route_exists(&state, "d|old").await, "old route pruned");
    assert!(route_exists(&state, "d|recent").await, "recent route kept");
    // its blob is gone
    assert!(!state.blobs.exists(&blob_key("d", "old", 0, "qcamera.ts")).await);
}

#[tokio::test]
async fn prunes_by_count_per_device() {
    let tmp = tempfile::tempdir().unwrap();
    let state = new_state(tmp.path()).await;
    sqlx::query("INSERT INTO devices (dongle_id, public_key, created_at) VALUES ('d', 'x', 0)").execute(&state.pool).await.unwrap();
    let now = homeconnect::db::now_millis();
    for i in 0..5 {
        mk_route(&state, "d", &format!("r{i}"), now - (i as i64) * 1000, 100).await;
    }
    // keep newest 2
    retention::save_policy(&state, &Policy { days: 0, max_drives: 2, max_gb: 0.0 }).await.unwrap();
    let n = retention::run_once(&state).await.unwrap();
    assert_eq!(n, 3, "5 drives, keep 2 → delete 3");
    assert!(route_exists(&state, "d|r0").await); // newest
    assert!(route_exists(&state, "d|r1").await);
    assert!(!route_exists(&state, "d|r4").await); // oldest gone
}

#[tokio::test]
async fn prunes_by_storage() {
    let tmp = tempfile::tempdir().unwrap();
    let state = new_state(tmp.path()).await;
    sqlx::query("INSERT INTO devices (dongle_id, public_key, created_at) VALUES ('d', 'x', 0)").execute(&state.pool).await.unwrap();
    let now = homeconnect::db::now_millis();
    // 3 routes × 1 MB blobs = ~3 MB; cap at ~0.0015 GB (1.5 MB) → keep ~1 newest.
    for i in 0..3 {
        mk_route(&state, "d", &format!("s{i}"), now - (i as i64) * 1000, 1_000_000).await;
    }
    retention::save_policy(&state, &Policy { days: 0, max_drives: 0, max_gb: 0.0015 }).await.unwrap();
    retention::run_once(&state).await.unwrap();
    let remaining = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM routes").fetch_one(&state.pool).await.unwrap();
    assert!(remaining <= 1, "storage cap leaves at most 1 route, got {remaining}");
    assert!(retention::storage_bytes(&state).await <= 1_500_000, "under cap");
    // the newest (s0) should be the survivor if any
    if remaining == 1 {
        assert!(route_exists(&state, "d|s0").await, "newest kept");
    }
}
