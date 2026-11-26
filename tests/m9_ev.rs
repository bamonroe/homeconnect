//! M9: EV energy from the rlog's bus-1 CAN. A synthetic rlog with a known power
//! profile goes through `parse_rlog_and_store`; the `ev.json` artifact and the
//! segment/route energy totals must match, whichever of rlog/qlog lands first.

use std::io::Write;

use homeconnect::cereal::log_capnp::event;
use homeconnect::config::Config;
use homeconnect::parse::{decode_ev_frame, EvArtifact, EvField};
use homeconnect::state::AppState;
use homeconnect::storage::blob_key;

const BASE: u64 = 1_000_000_000;

fn write_event<F: FnOnce(event::Builder)>(out: &mut Vec<u8>, mono_ns: u64, fill: F) {
    let mut msg = capnp::message::Builder::new_default();
    {
        let mut ev = msg.init_root::<event::Builder>();
        ev.set_log_mono_time(mono_ns);
        fill(ev);
    }
    capnp::serialize::write_message(out, &msg).unwrap();
}

fn frame(addr: u32, len: usize, off: usize, raw: [u8; 2]) -> (u32, Vec<u8>) {
    let mut d = vec![0u8; len];
    d[off..off + 2].copy_from_slice(&raw);
    (addr, d)
}

fn bz2(raw: &[u8]) -> Vec<u8> {
    let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
    enc.write_all(raw).unwrap();
    enc.finish().unwrap()
}

/// 10 s at 10 Hz and 20 m/s: +50 kW for the first 5 s, then −20 kW (regen).
fn build_rlog(fingerprint: &str) -> Vec<u8> {
    let mut raw = Vec::new();
    write_event(&mut raw, BASE, |ev| {
        let _ = ev.init_init_data();
    });
    write_event(&mut raw, BASE + 1, |ev| {
        ev.init_car_params().set_car_fingerprint(fingerprint);
    });
    for i in 0..=100u64 {
        let t = BASE + i * 100_000_000;
        let kw: i16 = if i < 50 { 12_500 } else { -5_000 }; // ×4 W
        let frames = [
            frame(0x250, 24, 20, kw.to_le_bytes()),
            frame(0x235, 16, 13, 7000u16.to_le_bytes()),
            frame(0x25a, 16, 12, (0x8000u16 | 16_384).to_le_bytes()),
        ];
        write_event(&mut raw, t, |ev| {
            let mut can = ev.init_can(frames.len() as u32 + 1);
            for (j, (addr, dat)) in frames.iter().enumerate() {
                let mut f = can.reborrow().get(j as u32);
                f.set_address(*addr);
                f.set_src(1);
                f.set_dat(dat);
            }
            // Same address on another bus: must be ignored.
            let mut f = can.reborrow().get(frames.len() as u32);
            f.set_address(0x250);
            f.set_src(0);
            f.set_dat(&frame(0x250, 24, 20, i16::MAX.to_le_bytes()).1);
        });
        write_event(&mut raw, t + 1, |ev| {
            ev.init_car_state().set_v_ego(20.0);
        });
    }
    bz2(&raw)
}

/// Just enough qlog to create the segment row.
fn build_qlog() -> Vec<u8> {
    let mut raw = Vec::new();
    write_event(&mut raw, BASE, |ev| {
        let _ = ev.init_init_data();
    });
    bz2(&raw)
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

async fn route_energy(state: &AppState) -> (f64, f64, f64) {
    sqlx::query_as("SELECT ev_kwh_out, ev_kwh_regen, ev_meters FROM routes WHERE fullname = 'd|r'")
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn decodes_known_fields() {
    let (a, d) = frame(0x250, 24, 20, (-250i16).to_le_bytes());
    assert_eq!(decode_ev_frame(a, &d), Some(EvField::PowerKw(-1.0)));
    let (a, d) = frame(0x25a, 16, 12, (0x8000u16 | 123).to_le_bytes());
    assert_eq!(decode_ev_frame(a, &d), Some(EvField::SocRaw(123.0)), "top bit masked");
    assert_eq!(decode_ev_frame(0x250, &[0u8; 8]), None, "short frame");
    assert_eq!(decode_ev_frame(0x123, &[0u8; 32]), None, "unknown address");
}

#[tokio::test]
async fn rlog_energy_lands_on_segment_and_route_in_either_order() {
    let rlog = build_rlog("HYUNDAI_IONIQ_5");
    // 50 kW × 5 s out, 20 kW × 5 s back; 200 m.
    let (want_out, want_regen) = (50.0 * 5.0 / 3600.0, 20.0 * 5.0 / 3600.0);

    // qlog first, then rlog.
    let tmp = tempfile::tempdir().unwrap();
    let state = new_state(tmp.path()).await;
    homeconnect::parse::parse_and_store(&state, "d", "r", 0, "qlog.bz2", &build_qlog()).await.unwrap();
    homeconnect::parse::parse_rlog_and_store(&state, "d", "r", 0, "rlog.bz2", &rlog).await.unwrap();

    let bytes = state.blobs.get(&blob_key("d", "r", 0, "ev.json")).await.expect("ev.json written");
    let ev: EvArtifact = serde_json::from_slice(&bytes).unwrap();
    assert!(close(ev.kwh_out, want_out, 0.002), "kwh_out {}", ev.kwh_out);
    assert!(close(ev.kwh_regen, want_regen, 0.002), "kwh_regen {}", ev.kwh_regen);
    assert!(close(ev.meters, 200.0, 1.0), "meters {}", ev.meters);
    assert_eq!(ev.t.len(), ev.kw.len());
    assert_eq!(ev.kw[0], Some(50.0), "bus-0 frame ignored");
    assert_eq!(ev.volts[0], Some(700.0));
    assert_eq!(ev.soc_raw[0], Some(16_384.0));
    assert!(ev.amps[0].is_none(), "no current frames → null");

    let (out, regen, m) = route_energy(&state).await;
    assert!(close(out, ev.kwh_out, 1e-9) && close(regen, ev.kwh_regen, 1e-9) && close(m, ev.meters, 1e-9));

    // rlog first (no segment row yet), then qlog: the qlog parse picks it up.
    let tmp = tempfile::tempdir().unwrap();
    let state = new_state(tmp.path()).await;
    homeconnect::parse::parse_rlog_and_store(&state, "d", "r", 0, "rlog.bz2", &rlog).await.unwrap();
    homeconnect::parse::parse_and_store(&state, "d", "r", 0, "qlog.bz2", &build_qlog()).await.unwrap();
    let (out, regen, _) = route_energy(&state).await;
    assert!(close(out, want_out, 0.002) && close(regen, want_regen, 0.002), "{out} {regen}");
}

#[tokio::test]
async fn other_cars_get_no_ev_data() {
    let tmp = tempfile::tempdir().unwrap();
    let state = new_state(tmp.path()).await;
    homeconnect::parse::parse_and_store(&state, "d", "r", 0, "qlog.bz2", &build_qlog()).await.unwrap();
    homeconnect::parse::parse_rlog_and_store(&state, "d", "r", 0, "rlog.bz2", &build_rlog("TOYOTA_PRIUS"))
        .await
        .unwrap();
    assert!(!state.blobs.exists(&blob_key("d", "r", 0, "ev.json")).await);
    assert_eq!(route_energy(&state).await.0, 0.0);
}
