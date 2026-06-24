//! M3 browse API: a paired user lists devices, fetches routes_segments, pulls
//! the qcamera HLS playlist, and downloads blobs (with Range). Anonymous access
//! to a non-public route is rejected.

use std::io::Write;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use homeconnect::cereal::log_capnp::event;
use homeconnect::config::Config;
use serde_json::Value;
use tower::ServiceExt;

fn write_event<F: FnOnce(event::Builder)>(out: &mut Vec<u8>, mono_ns: u64, fill: F) {
    let mut msg = capnp::message::Builder::new_default();
    {
        let mut ev = msg.init_root::<event::Builder>();
        ev.set_log_mono_time(mono_ns);
        fill(ev);
    }
    capnp::serialize::write_message(out, &msg).unwrap();
}

/// Minimal qlog: onroad start + two GPS points + git/platform metadata.
fn minimal_qlog() -> Vec<u8> {
    use homeconnect::cereal::log_capnp::init_data;
    let mut raw = Vec::new();
    let onroad = 1_000_000_000u64;
    write_event(&mut raw, 0, |ev| {
        let mut id = ev.init_init_data();
        id.set_git_branch("master");
        id.set_device_type(init_data::DeviceType::Tici);
    });
    write_event(&mut raw, 5, |ev| {
        let mut cp = ev.init_car_params();
        cp.set_car_fingerprint("HONDA_CIVIC");
    });
    write_event(&mut raw, onroad, |ev| {
        let mut ds = ev.init_device_state();
        ds.set_started(true);
        ds.set_started_mono_time(onroad);
    });
    for i in 0..3u64 {
        write_event(&mut raw, onroad + (i + 1) * 1_000_000_000, |ev| {
            let mut gps = ev.init_gps_location();
            gps.set_latitude(37.0 + i as f64 * 0.001);
            gps.set_longitude(-122.0 + i as f64 * 0.001);
            gps.set_unix_timestamp_millis(1_700_000_000_000 + i as i64 * 1000);
            gps.set_speed(10.0);
            gps.set_has_fix(true);
        });
    }
    let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
    enc.write_all(&raw).unwrap();
    enc.finish().unwrap()
}

async fn body_json(resp: axum::response::Response) -> Value {
    let b = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&b).unwrap_or(Value::Null)
}
async fn body_text(resp: axum::response::Response) -> String {
    let b = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    String::from_utf8_lossy(&b).to_string()
}

#[tokio::test]
async fn user_browses_routes_playlist_and_blobs() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config::from_env();
    config.data_dir = tmp.path().to_path_buf();
    config.public_url = "http://test.local".into();
    let state = homeconnect::build_state(config).await.unwrap();
    let app = homeconnect::router(state.clone());

    let dongle = "dongle0";
    let ts = "2024-05-06--07-08-09";

    // Create user + device, pair them.
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

    // Parse a qlog and register a qcamera.ts blob for the segment.
    homeconnect::parse::parse_and_store(&state, dongle, ts, 0, "qlog.bz2", &minimal_qlog())
        .await
        .unwrap();
    let ts_bytes = b"0123456789".to_vec();
    let qcam_key = homeconnect::storage::blob_key(dongle, ts, 0, "qcamera.ts");
    state.blobs.put(&qcam_key, &ts_bytes).await.unwrap();
    homeconnect::parse::set_segment_file(&state, dongle, ts, 0, "qcamera.ts")
        .await
        .unwrap();
    homeconnect::parse::recompute_route(&state, dongle, ts).await.unwrap();

    // Log in.
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
    let user_jwt = body_json(login).await["access_token"].as_str().unwrap().to_string();
    let bearer = format!("JWT {user_jwt}");

    // /v1/me/devices
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/v1/me/devices").header("Authorization", &bearer).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let devs = body_json(resp).await;
    assert_eq!(devs[0]["dongle_id"], dongle);
    assert_eq!(devs[0]["is_owner"], true);

    // /v1/devices/{dongle}/routes_segments
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/devices/{dongle}/routes_segments"))
                .header("Authorization", &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let routes = body_json(resp).await;
    let r0 = &routes[0];
    assert_eq!(r0["fullname"], format!("{dongle}|{ts}"));
    assert_eq!(r0["platform"], "HONDA_CIVIC");
    assert_eq!(r0["maxqlog"], 0);
    assert_eq!(r0["maxqcamera"], 0);
    assert_eq!(r0["segment_numbers"], serde_json::json!([0]));
    assert!(r0["length"].as_f64().unwrap() > 0.0);
    assert!(r0["share_sig"].as_str().unwrap().len() > 10);

    // /v1/route/{fullname}/qcamera.m3u8
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/route/{dongle}|{ts}/qcamera.m3u8"))
                .header("Authorization", &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let m3u8 = body_text(resp).await;
    assert!(m3u8.starts_with("#EXTM3U"));
    assert!(m3u8.contains("#EXT-X-ENDLIST"));
    assert!(m3u8.contains("/connectdata/qcam/dongle0/"));
    assert!(m3u8.contains("qcamera.ts?sig="));

    // Blob download with Range → 206 partial.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/connectdata/qcam/{dongle}/{ts}/0/qcamera.ts"))
                .header("Authorization", &bearer)
                .header("Range", "bytes=2-5")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(resp.headers().get("content-range").unwrap(), "bytes 2-5/10");
    let part = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&part[..], b"2345");

    // Full download → 200.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/connectdata/qcam/{dongle}/{ts}/0/qcamera.ts"))
                .header("Authorization", &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let full = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&full[..], &ts_bytes[..]);

    // coords.json via the 4-part (no-type) path.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/connectdata/{dongle}/{ts}/0/coords.json"))
                .header("Authorization", &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let coords = body_json(resp).await;
    assert!(coords.as_array().unwrap().len() >= 2);

    // Anonymous access to a non-public route is rejected.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/connectdata/qcam/{dongle}/{ts}/0/qcamera.ts"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
