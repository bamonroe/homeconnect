//! M6: retention prunes data **per type** by age, per-device count, and size,
//! plus a whole-store cap that only ever takes videos. Drives themselves (rows,
//! logs, derived artifacts) are never deleted by retention.

use std::collections::BTreeMap;

use homeconnect::config::Config;
use homeconnect::retention::{self, Limits, Policy};
use homeconnect::state::AppState;
use homeconnect::storage::blob_key;

/// A one-segment drive holding a `qcamera.ts` of `video_len` bytes, a
/// `qlog.zst` of `log_len` bytes, and a derived `coords.json`.
async fn mk_route(state: &AppState, dongle: &str, ts: &str, start_ms: i64, video_len: usize, log_len: usize) {
    let fullname = format!("{dongle}|{ts}");
    sqlx::query("INSERT INTO routes (fullname, device_dongle_id, start_time_utc_millis, maxqlog, created_at) VALUES (?, ?, ?, 0, ?)")
        .bind(&fullname).bind(dongle).bind(start_ms).bind(start_ms)
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO segments (canonical_name, canonical_route_name, number, qcam_url, qlog_url, created_at) VALUES (?, ?, 0, 'u', 'u', ?)")
        .bind(format!("{fullname}--0")).bind(&fullname).bind(start_ms)
        .execute(&state.pool).await.unwrap();
    state.blobs.put(&blob_key(dongle, ts, 0, "qcamera.ts"), &vec![7u8; video_len]).await.unwrap();
    state.blobs.put(&blob_key(dongle, ts, 0, "qlog.zst"), &vec![1u8; log_len]).await.unwrap();
    state.blobs.put(&blob_key(dongle, ts, 0, "coords.json"), b"[]").await.unwrap();
}

/// Give a drive full-res road video and a built fcamera movie.
async fn add_fcamera(state: &AppState, dongle: &str, ts: &str) {
    let fullname = format!("{dongle}|{ts}");
    sqlx::query("UPDATE segments SET fcam_url = 'u' WHERE canonical_route_name = ?")
        .bind(&fullname).execute(&state.pool).await.unwrap();
    state.blobs.put(&blob_key(dongle, ts, 0, "fcamera.hevc"), &vec![3u8; 100]).await.unwrap();
    state.blobs.put(&homeconnect::movie::movie_key(dongle, ts, "fcamera"), &vec![4u8; 100]).await.unwrap();
    sqlx::query("INSERT INTO movies (fullname, cam, seg_count, bytes) VALUES (?, 'fcamera', 1, 100)")
        .bind(&fullname).execute(&state.pool).await.unwrap();
}

async fn route_exists(state: &AppState, fullname: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM routes WHERE fullname = ?")
        .bind(fullname).fetch_one(&state.pool).await.unwrap() > 0
}

async fn has(state: &AppState, dongle: &str, ts: &str, file: &str) -> bool {
    state.blobs.exists(&blob_key(dongle, ts, 0, file)).await
}

async fn new_state(tmp: &std::path::Path) -> AppState {
    let mut config = Config::from_env();
    config.data_dir = tmp.to_path_buf();
    // disable env-driven limits; tests set policy explicitly
    config.retain_days = 0; config.retain_max_drives = 0; config.retain_gb = 0.0;
    let state = homeconnect::build_state(config).await.unwrap();
    sqlx::query("INSERT INTO devices (dongle_id, public_key, created_at) VALUES ('d', 'x', 0)")
        .execute(&state.pool).await.unwrap();
    state
}

fn policy(max_gb: f64, types: &[(&str, Limits)]) -> Policy {
    let types: BTreeMap<String, Limits> = types.iter().map(|(t, l)| (t.to_string(), *l)).collect();
    Policy { max_gb, types }
}

const DAY: i64 = 86_400_000;

#[tokio::test]
async fn age_prunes_only_that_type_and_keeps_the_drive() {
    let tmp = tempfile::tempdir().unwrap();
    let state = new_state(tmp.path()).await;
    let now = homeconnect::db::now_millis();
    mk_route(&state, "d", "old", now - 40 * DAY, 100, 10).await;
    mk_route(&state, "d", "recent", now - 2 * DAY, 100, 10).await;

    let days30 = Limits { days: 30, ..Default::default() };
    retention::save_policy(&state, &policy(0.0, &[("qcamera", days30)])).await.unwrap();
    let r = retention::run_once(&state).await.unwrap();
    assert_eq!((r.pruned, r.drives), (1, 1));
    assert_eq!(r.freed_bytes, 100);

    // The old drive lost its video but nothing else.
    assert!(route_exists(&state, "d|old").await, "drive row kept");
    assert!(!has(&state, "d", "old", "qcamera.ts").await, "old video pruned");
    assert!(has(&state, "d", "old", "qlog.zst").await, "log kept");
    assert!(has(&state, "d", "old", "coords.json").await, "derived data kept");
    let url: String = sqlx::query_scalar("SELECT qcam_url FROM segments WHERE canonical_route_name = 'd|old'")
        .fetch_one(&state.pool).await.unwrap();
    assert_eq!(url, "", "segment no longer advertises the video");
    // And sync won't pull it back off the device.
    let types = homeconnect::devsync::effective_route_types(&state, "d|old").await;
    assert!(!types.contains(&"qcamera".to_string()), "qcamera pinned off: {types:?}");

    assert!(has(&state, "d", "recent", "qcamera.ts").await, "recent video kept");

    // A second pass has nothing left to do.
    assert_eq!(retention::run_once(&state).await.unwrap().pruned, 0);
}

#[tokio::test]
async fn count_cap_keeps_newest_drives_and_drops_their_movies() {
    let tmp = tempfile::tempdir().unwrap();
    let state = new_state(tmp.path()).await;
    let now = homeconnect::db::now_millis();
    for i in 0..5 {
        let ts = format!("r{i}");
        mk_route(&state, "d", &ts, now - (i as i64) * 1000, 10, 10).await;
        add_fcamera(&state, "d", &ts).await;
    }
    let keep2 = Limits { max_drives: 2, ..Default::default() };
    retention::save_policy(&state, &policy(0.0, &[("fcamera", keep2)])).await.unwrap();
    let r = retention::run_once(&state).await.unwrap();
    assert_eq!(r.pruned, 3, "5 drives, keep fcamera on 2");

    for (ts, kept) in [("r0", true), ("r1", true), ("r2", false), ("r4", false)] {
        assert_eq!(has(&state, "d", ts, "fcamera.hevc").await, kept, "{ts} fcamera");
        let movie = state.blobs.exists(&homeconnect::movie::movie_key("d", ts, "fcamera")).await;
        assert_eq!(movie, kept, "{ts} movie");
        assert!(has(&state, "d", ts, "qcamera.ts").await, "{ts} other video untouched");
        assert!(route_exists(&state, &format!("d|{ts}")).await);
    }
}

#[tokio::test]
async fn type_size_cap_prunes_oldest_first() {
    let tmp = tempfile::tempdir().unwrap();
    let state = new_state(tmp.path()).await;
    let now = homeconnect::db::now_millis();
    for i in 0..3 {
        mk_route(&state, "d", &format!("s{i}"), now - (i as i64) * 1000, 1_000_000, 10).await;
    }
    let bytes = retention::type_bytes(&state).await;
    assert_eq!(bytes["qcamera"], 3_000_000);
    assert_eq!(bytes["qlog"], 30);

    let cap = Limits { max_gb: 0.0015, ..Default::default() };
    retention::save_policy(&state, &policy(0.0, &[("qcamera", cap)])).await.unwrap();
    retention::run_once(&state).await.unwrap();
    assert!(has(&state, "d", "s0", "qcamera.ts").await, "newest kept");
    assert!(!has(&state, "d", "s1", "qcamera.ts").await);
    assert!(!has(&state, "d", "s2", "qcamera.ts").await);
    assert_eq!(retention::type_bytes(&state).await["qcamera"], 1_000_000);
}

#[tokio::test]
async fn whole_store_cap_never_takes_logs() {
    let tmp = tempfile::tempdir().unwrap();
    let state = new_state(tmp.path()).await;
    let now = homeconnect::db::now_millis();
    // 3 drives × (1 MB video + 300 KB log); cap 1.5 MB.
    for i in 0..3 {
        mk_route(&state, "d", &format!("s{i}"), now - (i as i64) * 1000, 1_000_000, 300_000).await;
    }
    retention::save_policy(&state, &policy(0.0015, &[])).await.unwrap();
    retention::run_once(&state).await.unwrap();

    for i in 0..3 {
        let ts = format!("s{i}");
        assert!(has(&state, "d", &ts, "qlog.zst").await, "{ts} log kept");
        assert!(route_exists(&state, &format!("d|{ts}")).await);
    }
    // 900 KB of logs is under the cap with no video at all, so the newest
    // drive's video can't fit (1.9 MB) and everything video goes.
    assert!(!has(&state, "d", "s0", "qcamera.ts").await);
    assert!(retention::storage_bytes(&state).await <= 1_500_000);

    // If logs alone exceed the cap, the pass stops rather than deleting them.
    retention::save_policy(&state, &policy(0.0001, &[])).await.unwrap();
    retention::run_once(&state).await.unwrap();
    assert!(has(&state, "d", "s2", "qlog.zst").await);
}

#[tokio::test]
async fn legacy_policy_seeds_videos_only() {
    let tmp = tempfile::tempdir().unwrap();
    let state = new_state(tmp.path()).await;
    homeconnect::settings::set(&state, "retain_days", "30").await.unwrap();
    homeconnect::settings::set(&state, "retain_max_drives", "5").await.unwrap();
    homeconnect::settings::set(&state, "retain_gb", "500").await.unwrap();

    let p = retention::load_policy(&state).await;
    assert_eq!(p.max_gb, 500.0);
    let legacy = Limits { days: 30, max_drives: 5, max_gb: 0.0 };
    for t in ["qcamera", "fcamera", "dcamera", "ecamera"] {
        assert_eq!(p.types[t], legacy, "{t}");
    }
    for t in ["qlog", "rlog"] {
        assert_eq!(p.types[t], Limits::default(), "{t} kept forever");
    }

    // Once saved, the per-type policy is what's used.
    retention::save_policy(&state, &policy(500.0, &[("rlog", Limits { days: 365, ..Default::default() })]))
        .await
        .unwrap();
    let p = retention::load_policy(&state).await;
    assert_eq!(p.types["qcamera"], Limits::default());
    assert_eq!(p.types["rlog"].days, 365);
}
