//! M2: build a synthetic qlog from real cereal Events (engagement + a GPS track
//! + a thumbnail), run it through the parser, and assert the derived artifacts
//! (coords/events/sprite) and the route/segment rows.

use std::io::Write;

use homeconnect::cereal::log_capnp::{event, init_data, selfdrive_state};
use homeconnect::config::Config;

fn write_event<F: FnOnce(event::Builder)>(out: &mut Vec<u8>, mono_ns: u64, fill: F) {
    let mut msg = capnp::message::Builder::new_default();
    {
        let mut ev = msg.init_root::<event::Builder>();
        ev.set_log_mono_time(mono_ns);
        fill(ev);
    }
    capnp::serialize::write_message(out, &msg).unwrap();
}

/// A tiny valid JPEG (solid colour) for thumbnail/sprite testing.
fn tiny_jpeg() -> Vec<u8> {
    use image::{ImageEncoder, RgbImage};
    let img = RgbImage::from_pixel(160, 120, image::Rgb([20, 120, 200]));
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 80)
        .write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgb8)
        .unwrap();
    buf
}

fn build_qlog() -> Vec<u8> {
    let mut raw = Vec::new();
    let onroad = 1_000_000_000u64; // 1s in ns

    // InitData: git + device type.
    write_event(&mut raw, 0, |ev| {
        let mut id = ev.init_init_data();
        id.set_git_branch("master");
        id.set_git_commit("deadbeef");
        id.set_git_remote("git@example:op.git");
        id.set_device_type(init_data::DeviceType::Tici);
    });
    // CarParams: platform fingerprint.
    write_event(&mut raw, 10, |ev| {
        let mut cp = ev.init_car_params();
        cp.set_car_fingerprint("TOYOTA_COROLLA_TSS2");
    });
    // DeviceState started → sets onroad baseline.
    write_event(&mut raw, onroad, |ev| {
        let mut ds = ev.init_device_state();
        ds.set_started(true);
        ds.set_started_mono_time(onroad);
    });
    // SelfdriveState disabled at start.
    write_event(&mut raw, onroad, |ev| {
        let mut ss = ev.init_selfdrive_state();
        ss.set_enabled(false);
        ss.set_engageable(true);
        ss.set_alert_status(selfdrive_state::AlertStatus::Normal);
        ss.set_alert_size(selfdrive_state::AlertSize::None);
    });

    // A short GPS track: 5 points moving north/east, each ~1s apart.
    let base_lat = 37.7749;
    let base_lng = -122.4194;
    for i in 0..5u64 {
        let mono = onroad + (i + 1) * 1_000_000_000;
        let lat = base_lat + (i as f64) * 0.001; // ~111 m per 0.001 deg lat
        let lng = base_lng + (i as f64) * 0.001;
        let ts = 1_700_000_000_000i64 + (i as i64) * 1000;
        write_event(&mut raw, mono, |ev| {
            let mut gps = ev.init_gps_location();
            gps.set_latitude(lat);
            gps.set_longitude(lng);
            gps.set_unix_timestamp_millis(ts);
            gps.set_speed(15.0);
            gps.set_has_fix(true);
        });
    }

    // SelfdriveState becomes enabled → a state-transition event.
    write_event(&mut raw, onroad + 3_000_000_000, |ev| {
        let mut ss = ev.init_selfdrive_state();
        ss.set_enabled(true);
        ss.set_engageable(true);
        ss.set_alert_status(selfdrive_state::AlertStatus::Normal);
        ss.set_alert_size(selfdrive_state::AlertSize::None);
    });

    // One CAN frame (presence flag) and a thumbnail.
    write_event(&mut raw, onroad + 100, |ev| {
        let _ = ev.init_can(1);
    });
    let jpeg = tiny_jpeg();
    write_event(&mut raw, onroad + 200, |ev| {
        let mut t = ev.init_thumbnail();
        t.set_thumbnail(&jpeg);
    });

    // Compress as bz2 (the parser chooses by extension).
    let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
    enc.write_all(&raw).unwrap();
    enc.finish().unwrap()
}

#[tokio::test]
async fn parses_synthetic_qlog_into_route_and_artifacts() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config::from_env();
    config.data_dir = tmp.path().to_path_buf();
    config.public_url = "http://test.local".into();
    let state = homeconnect::build_state(config).await.unwrap();

    // A device row is needed for the GPS-update FK-ish write (and realism).
    sqlx::query("INSERT INTO devices (dongle_id, public_key, created_at) VALUES ('dongle0', 'x', 0)")
        .execute(&state.pool)
        .await
        .unwrap();

    let dongle = "dongle0";
    let ts = "2024-01-02--03-04-05";
    let qlog = build_qlog();

    homeconnect::parse::parse_and_store(&state, dongle, ts, 0, "qlog.bz2", &qlog)
        .await
        .expect("parse");

    // --- segment row ---
    #[derive(sqlx::FromRow)]
    struct Seg {
        miles: f64,
        start_lat: f64,
        end_lat: f64,
        qlog_url: String,
        start_time_utc_millis: i64,
        end_time_utc_millis: i64,
    }
    let seg: Seg = sqlx::query_as("SELECT miles, start_lat, end_lat, qlog_url, start_time_utc_millis, end_time_utc_millis FROM segments WHERE canonical_name = ?")
        .bind(format!("{dongle}|{ts}--0"))
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert!(seg.miles > 0.0, "segment should have positive mileage");
    assert!((seg.start_lat - 37.7749).abs() < 1e-6, "start lat from first GPS");
    assert!(seg.end_lat > seg.start_lat, "track moves north");
    assert_eq!(
        seg.qlog_url,
        format!("http://test.local/connectdata/qlog/{dongle}/{ts}/0/qlog.bz2")
    );
    assert_eq!(seg.start_time_utc_millis, 1_700_000_000_000);
    assert_eq!(seg.end_time_utc_millis, 1_700_000_004_000);

    // --- route aggregation ---
    #[derive(sqlx::FromRow)]
    struct Route {
        length: f64,
        maxqlog: i64,
        platform: String,
        git_branch: String,
        segment_numbers: String,
    }
    let route: Route = sqlx::query_as("SELECT length, maxqlog, platform, git_branch, segment_numbers FROM routes WHERE fullname = ?")
        .bind(format!("{dongle}|{ts}"))
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert!(route.length > 0.0);
    assert_eq!(route.maxqlog, 0, "segment 0 has a qlog");
    assert_eq!(route.platform, "TOYOTA_COROLLA_TSS2");
    assert_eq!(route.git_branch, "master");
    assert_eq!(route.segment_numbers, "[0]");

    // --- artifacts in the blob store ---
    let coords_key = homeconnect::storage::blob_key(dongle, ts, 0, "coords.json");
    let coords_bytes = state.blobs.get(&coords_key).await.expect("coords.json");
    let coords: serde_json::Value = serde_json::from_slice(&coords_bytes).unwrap();
    assert!(coords.as_array().unwrap().len() >= 4, "GPS points logged as coords");
    assert!(coords[0]["lat"].is_number() && coords[0]["t"].is_number());

    let events_key = homeconnect::storage::blob_key(dongle, ts, 0, "events.json");
    let events_bytes = state.blobs.get(&events_key).await.expect("events.json");
    let events: serde_json::Value = serde_json::from_slice(&events_bytes).unwrap();
    let arr = events.as_array().unwrap();
    assert!(arr.iter().any(|e| e["data"]["enabled"] == true), "an engagement event");

    let sprite_key = homeconnect::storage::blob_key(dongle, ts, 0, "sprite.jpg");
    let sprite = state.blobs.get(&sprite_key).await.expect("sprite.jpg");
    assert!(image::load_from_memory(&sprite).is_ok(), "sprite is valid JPEG");

    // device last-GPS updated
    let (lat,): (f64,) = sqlx::query_as("SELECT last_gps_lat FROM devices WHERE dongle_id = ?")
        .bind(dongle)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert!(lat > 37.0);
}
