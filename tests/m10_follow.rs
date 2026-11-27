//! M10: following distance from the qlog's radarState. A synthetic qlog with a
//! known lead profile goes through `parse_and_store`; the telemetry lead fields
//! and the segment/route following stats must match.

use std::io::Write;

use homeconnect::cereal::log_capnp::event;
use homeconnect::config::Config;
use homeconnect::state::AppState;
use homeconnect::storage::blob_key;

const BASE: u64 = 1_000_000_000;
const MS: u64 = 1_000_000;

fn write_event<F: FnOnce(event::Builder)>(out: &mut Vec<u8>, mono_ns: u64, fill: F) {
    let mut msg = capnp::message::Builder::new_default();
    {
        let mut ev = msg.init_root::<event::Builder>();
        ev.set_log_mono_time(mono_ns);
        fill(ev);
    }
    capnp::serialize::write_message(out, &msg).unwrap();
}

/// 25 s at 20 m/s. 0–10 s: lead at 30 m (1.5 s gap), openpilot longitudinal
/// on. 10–20 s: lead at 15 m (0.75 s, close), driver in control. 20–25 s: the
/// radar goes quiet, so the lead must drop out after it goes stale. Then 5 s
/// at 5 m/s with a lead at 2 m, below the following speed floor.
fn build_qlog() -> Vec<u8> {
    let mut raw = Vec::new();
    write_event(&mut raw, BASE, |ev| {
        let _ = ev.init_init_data();
    });
    for i in 0..=600u64 {
        let ms = i * 50;
        let t = BASE + ms * MS;
        let secs = ms as f64 / 1000.0;
        let slow = secs >= 25.0;
        if ms % 100 == 0 {
            write_event(&mut raw, t, |ev| {
                ev.init_selfdrive_state().set_enabled(secs < 10.0);
            });
        }
        if ms % 250 == 0 && (secs < 20.0 || slow) {
            let d = if slow { 2.0 } else if secs < 10.0 { 30.0 } else { 15.0 };
            write_event(&mut raw, t + 1, |ev| {
                let mut l = ev.init_radar_state().init_lead_one();
                l.set_status(true);
                l.set_d_rel(d);
                l.set_v_rel(-0.5);
            });
        }
        write_event(&mut raw, t + 2, |ev| {
            ev.init_car_state().set_v_ego(if slow { 5.0 } else { 20.0 });
        });
    }
    let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
    enc.write_all(&raw).unwrap();
    enc.finish().unwrap()
}

async fn new_state(tmp: &std::path::Path) -> AppState {
    let mut config = Config::from_env();
    config.data_dir = tmp.to_path_buf();
    let state = homeconnect::build_state(config).await.unwrap();
    sqlx::query("INSERT INTO devices (dongle_id, public_key, created_at) VALUES ('d', 'x', 0)")
        .execute(&state.pool)
        .await
        .unwrap();
    state
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[tokio::test]
async fn lead_car_lands_in_telemetry_and_following_stats() {
    let tmp = tempfile::tempdir().unwrap();
    let state = new_state(tmp.path()).await;
    homeconnect::parse::parse_and_store(&state, "d", "r", 0, "qlog.bz2", &build_qlog()).await.unwrap();

    let bytes = state.blobs.get(&blob_key("d", "r", 0, "telemetry.json")).await.unwrap();
    let telem: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
    let at = |s: f64| telem.iter().find(|x| x["t"].as_f64().unwrap() >= s).unwrap();
    assert_eq!(at(5.0)["lead_d"], 30.0);
    assert_eq!(at(5.0)["lead_v"], -0.5);
    assert_eq!(at(15.0)["lead_d"], 15.0);
    assert!(at(22.0).get("lead_d").is_none(), "stale lead dropped: {}", at(22.0));
    assert_eq!(at(27.0)["lead_d"], 2.0, "slow lead still shown");

    let r: (f64, f64, f64, f64, f64, f64, f64) = sqlx::query_as(
        "SELECT follow_seconds, follow_gap_sum, close_seconds, \
                follow_op_seconds, follow_op_gap_sum, close_op_seconds, min_gap \
         FROM routes WHERE fullname = 'd|r'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let (follow, gap_sum, close_s, op, op_gap_sum, close_op, min_gap) = r;
    // ~21 s: 0–20 s plus the ~1 s the last radar sample stays fresh.
    assert!(close(follow, 21.0, 0.6), "follow {follow}");
    assert!(close(close_s, 11.0, 0.6), "close {close_s}");
    assert!(close(op, 10.0, 0.6), "op {op}");
    assert!(close(op_gap_sum / op, 1.5, 0.01), "op mean gap {}", op_gap_sum / op);
    assert_eq!(close_op, 0.0, "openpilot never followed close");
    assert!(close(min_gap, 0.75, 0.01), "min gap {min_gap}");
    assert!(gap_sum / follow > 0.75 && gap_sum / follow < 1.5);
}

#[tokio::test]
async fn no_lead_means_no_following() {
    let tmp = tempfile::tempdir().unwrap();
    let state = new_state(tmp.path()).await;
    let mut raw = Vec::new();
    write_event(&mut raw, BASE, |ev| {
        let _ = ev.init_init_data();
    });
    for i in 0..200u64 {
        let t = BASE + i * 50 * MS;
        write_event(&mut raw, t, |ev| ev.init_selfdrive_state().set_enabled(true));
        // radarState present but leadOne.status false (empty road).
        write_event(&mut raw, t + 1, |ev| {
            ev.init_radar_state().init_lead_one().set_d_rel(0.0);
        });
        write_event(&mut raw, t + 2, |ev| ev.init_car_state().set_v_ego(30.0));
    }
    homeconnect::parse::parse_and_store(&state, "d", "r", 0, "qlog", &raw).await.unwrap();
    let (follow, min_gap): (f64, f64) =
        sqlx::query_as("SELECT follow_seconds, min_gap FROM routes WHERE fullname = 'd|r'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!((follow, min_gap), (0.0, -1.0));
}
