//! qlog parsing: decode a segment's cereal log into route/segment rows and the
//! derived browse artifacts (coords.json, events.json, sprite.jpg). Mirrors the
//! reference (Konik) field-for-field so the SPA and device see equivalent data.

use std::io::Read;

use capnp::message::ReaderOptions;
use capnp::serialize;
use serde::Serialize;
use serde_json::json;

use crate::cereal::car_capnp::car_state::GearShifter;
use crate::cereal::log_capnp::{event, init_data, selfdrive_state};
use crate::db::now_millis;
use crate::error::AppResult;
use crate::state::AppState;
use crate::storage::blob_key;

const METERS_PER_MILE: f64 = 1609.344;

/// Haversine distance in meters (Earth radius 6371 km).
fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6_371_000.0_f64;
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dphi = (lat2 - lat1).to_radians();
    let dlam = (lon2 - lon1).to_radians();
    let a = (dphi / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dlam / 2.0).sin().powi(2);
    2.0 * r * a.sqrt().atan2((1.0 - a).sqrt())
}

#[derive(Serialize)]
struct Coord {
    t: f64,    // seconds since onroad start
    lat: f64,
    lng: f64,
    speed: f32,
    dist: f64, // meters from previous point
}

/// A downsampled vehicle-telemetry sample (≈4 Hz), synced to the video by `t`.
#[derive(Serialize)]
struct Telem {
    t: f64,       // seconds since onroad start
    speed: f32,   // mph
    gear: &'static str, // park/drive/reverse/neutral/... (gear_name is &'static)
    lb: bool,     // left blinker
    rb: bool,     // right blinker
    brake: bool,  // brake pressed
    gas: bool,    // gas pressed
    steer: f32,    // steering angle (deg)
    steer_override: bool, // driver is overriding the wheel (CarState.steeringPressed)
    engaged: bool, // openpilot controlling — lateral OR longitudinal (see lat/long)
    lat: bool,     // lateral (steering) active — sunnypilot MADS active
    long: bool,    // longitudinal (gas/brake) active — SelfdriveState.enabled
    cruise: bool,  // car cruise control on (cruiseState.enabled)
    soc: f32,      // state of charge / fuel level, percent (fuelGauge * 100)
    charging: bool,
    // Driver monitoring (only meaningful while DM is active). `dm_aware` is the
    // awareness level 0..1; -1 means DM wasn't actively monitoring at this sample.
    dm_aware: f32,
    dm_distracted: bool,
    dm_face: bool,
    // Lead car from radarState.leadOne (omitted when none is tracked): distance
    // (m) and speed relative to us (m/s, negative = closing).
    #[serde(skip_serializing_if = "Option::is_none")]
    lead_d: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lead_v: Option<f32>,
}

/// Aggregate stats derived from a segment's telemetry samples.
#[derive(Default)]
struct SegStats {
    engaged_m: f64,  // distance while openpilot engaged
    telem_m: f64,    // distance (speed-integrated)
    engaged_s: f64,  // time engaged
    drive_s: f64,    // time moving/driving
    max_speed: f64,  // mph
    disengage: i64,  // engaged→disengaged transitions
    hard_brake: i64, // distinct hard-braking events
    hard_accel: i64, // distinct hard-acceleration events
    // Following (see FOLLOW_MIN_MS / CLOSE_GAP_S). Gap = time headway (s).
    follow_s: f64,
    follow_gap_sum: f64, // Σ gap·dt, so mean gap = follow_gap_sum / follow_s
    close_s: f64,
    follow_op_s: f64,    // …while openpilot longitudinal was on
    follow_op_gap_sum: f64,
    close_op_s: f64,
    min_gap: f64,        // -1 when never following
}

const MPH_TO_MS: f64 = 0.44704;
const HARD_BRAKE_MS2: f64 = -3.0; // ~0.3 g
const HARD_ACCEL_MS2: f64 = 2.5;
/// Following stats only count at or above this speed (~18 mph): in stop-and-go
/// traffic the time headway (distance / speed) blows up and means little.
pub const FOLLOW_MIN_MS: f64 = 8.0;
/// A lead further back than this (time headway) is just a car up the road, not
/// one being followed, so it's left out of the following stats.
pub const FOLLOW_MAX_GAP_S: f64 = 3.0;
/// Time headway below this counts as following close.
pub const CLOSE_GAP_S: f64 = 1.0;
/// A radarState older than this is stale, so no lead is reported.
const LEAD_STALE_NS: u64 = 1_000_000_000;

/// Compute per-segment driving stats from the (≈4 Hz) telemetry series. Distance
/// is speed-integrated; hard accel/brake are rising-edge counted (a sustained
/// event counts once); accel is the derivative of speed.
fn telemetry_stats(t: &[Telem]) -> SegStats {
    let mut s = SegStats { min_gap: -1.0, ..Default::default() };
    let (mut prev_v, mut in_brake, mut in_accel) = (None::<f64>, false, false);
    for i in 0..t.len() {
        let v_ms = (t[i].speed as f64) * MPH_TO_MS;
        s.max_speed = s.max_speed.max(t[i].speed as f64);
        if i > 0 {
            let dt = t[i].t - t[i - 1].t;
            if dt > 0.0 && dt < 2.0 {
                let dist = v_ms * dt;
                s.telem_m += dist;
                s.drive_s += dt;
                if t[i].engaged {
                    s.engaged_m += dist;
                    s.engaged_s += dt;
                }
                let gap = t[i].lead_d.filter(|_| v_ms >= FOLLOW_MIN_MS).map(|d| d as f64 / v_ms);
                if let Some(gap) = gap.filter(|&g| g <= FOLLOW_MAX_GAP_S) {
                    let close = gap < CLOSE_GAP_S;
                    s.follow_s += dt;
                    s.follow_gap_sum += gap * dt;
                    if close { s.close_s += dt; }
                    if t[i].long {
                        s.follow_op_s += dt;
                        s.follow_op_gap_sum += gap * dt;
                        if close { s.close_op_s += dt; }
                    }
                    if s.min_gap < 0.0 || gap < s.min_gap { s.min_gap = gap; }
                }
                if let Some(pv) = prev_v {
                    let a = (v_ms - pv) / dt;
                    in_brake = if a <= HARD_BRAKE_MS2 {
                        if !in_brake { s.hard_brake += 1; }
                        true
                    } else { false };
                    in_accel = if a >= HARD_ACCEL_MS2 {
                        if !in_accel { s.hard_accel += 1; }
                        true
                    } else { false };
                }
            }
            if t[i - 1].engaged && !t[i].engaged {
                s.disengage += 1;
            }
        }
        prev_v = Some(v_ms);
    }
    s
}

fn gear_name(g: GearShifter) -> &'static str {
    use GearShifter::*;
    match g {
        Unknown => "unknown",
        Park => "park",
        Drive => "drive",
        Neutral => "neutral",
        Reverse => "reverse",
        Sport => "sport",
        Low => "low",
        Brake => "brake",
        Eco => "eco",
        Manumatic => "manumatic",
    }
}

#[derive(Serialize)]
struct DriveEvent {
    #[serde(rename = "type")]
    kind: String,
    time: u64, // log mono time (ns)
    route_offset_millis: i64,
    data: serde_json::Value,
}

/// Decompress the raw upload by extension. Tolerant of truncation: the final
/// segment of a drive often has an incomplete frame (the device stopped
/// mid-write), so we keep whatever decoded cleanly and let the capnp loop stop
/// at the first partial message.
fn decompress(file: &str, raw: &[u8]) -> AppResult<Vec<u8>> {
    if file.ends_with(".bz2") {
        Ok(read_tolerant(bzip2::read::BzDecoder::new(raw)))
    } else if file.ends_with(".zst") {
        let dec = zstd::stream::read::Decoder::new(raw)
            .map_err(|e| crate::error::AppError::BadRequest(format!("zst init: {e}")))?;
        Ok(read_tolerant(dec))
    } else {
        // Already-decompressed log.
        Ok(raw.to_vec())
    }
}

/// Read a decoder to completion, returning all bytes decoded before any error
/// (truncated/incomplete trailing frame is treated as end-of-stream).
fn read_tolerant<R: Read>(mut r: R) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 65536];
    loop {
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
    }
    out
}

fn device_type_name(dt: init_data::DeviceType) -> &'static str {
    use init_data::DeviceType::*;
    match dt {
        Unknown => "unknown",
        Neo => "neo",
        ChffrAndroid => "chffrAndroid",
        ChffrIos => "chffrIos",
        Tici => "tici",
        Pc => "pc",
        Tizi => "tizi",
        Mici => "mici",
    }
}

#[derive(Default)]
struct Accum {
    coords: Vec<Coord>,
    events: Vec<DriveEvent>,
    telemetry: Vec<Telem>,
    last_telem_mono: u64,
    thumbnails: Vec<Vec<u8>>,
    total_meters: f64,
    first_gps: Option<(f64, f64, i64)>, // lat, lng, unix_ms
    last_gps: Option<(f64, f64, i64)>,
    // Route-start mono (each segment's qlog opens with initData carrying it), so
    // `mono - route_base` is the route-relative time, matching the video grid.
    route_base_mono: Option<u64>,
    last_pt: Option<(f64, f64)>,
    car_fingerprint: String,
    git_remote: String,
    git_branch: String,
    git_commit: String,
    device_type: String,
    has_can: bool,
    has_gps: bool,
    cur_engaged: bool,    // latest longitudinal engagement (SelfdriveState.enabled)
    cur_lat: bool,        // latest lateral engagement (MADS active, selfdriveStateSP)
    cur_alert: i32,       // latest alert status (0 normal, 1 prompt, 2 critical)
    // latest driver-monitoring state (driverMonitoringState)
    cur_dm_active: bool,
    cur_dm_face: bool,
    cur_dm_distracted: bool,
    cur_dm_aware: f32,
    // latest radarState.leadOne: (mono, dRel m, vRel m/s); None = no lead
    cur_lead: Option<(u64, f32, f32)>,
    seen_selfdrive: bool, // have we read engagement state yet this segment?
    // Device health (from DeviceState).
    max_temp: f32,
    min_free: f32,
    max_mem: i32,
    has_health: bool,
    // selfdrive state collapsing
    last_state: Option<(bool, i32)>,
}

// ── modelV2 (top-down "what openpilot saw") — parsed from the rlog ──────────────
// modelV2 isn't in the qlog (decimated out), so this runs over the full rlog.
// All points are in the device frame: x = forward (m), y = left (m).

#[derive(serde::Serialize)]
struct ModelXY {
    x: Vec<f32>,
    y: Vec<f32>,
    z: Vec<f32>,
}
#[derive(serde::Serialize)]
struct ModelLead {
    x: f32,
    y: f32,
    v: f32,
}
#[derive(serde::Serialize)]
struct ModelFrame {
    t: f64,
    path: ModelXY,
    speed: Vec<f32>, // predicted speed (m/s) per path point (from modelV2.velocity)
    lanes: Vec<ModelXY>,
    edges: Vec<ModelXY>,
    lead: Option<ModelLead>,
}
#[derive(serde::Serialize)]
struct ModelArtifact {
    /// liveCalibration rpyCalib (roll, pitch, yaw) for this segment — the camera
    /// mount orientation, applied when projecting onto the road video.
    rpy: [f32; 3],
    frames: Vec<ModelFrame>,
}

fn flist(r: ::capnp::Result<::capnp::primitive_list::Reader<'_, f32>>, step: usize) -> Vec<f32> {
    match r {
        Ok(l) => l.iter().step_by(step.max(1)).collect(),
        Err(_) => Vec::new(),
    }
}
fn xy(d: crate::cereal::log_capnp::x_y_z_t_data::Reader<'_>, step: usize) -> ModelXY {
    ModelXY { x: flist(d.get_x(), step), y: flist(d.get_y(), step), z: flist(d.get_z(), step) }
}
fn speed_norm(d: crate::cereal::log_capnp::x_y_z_t_data::Reader<'_>, step: usize) -> Vec<f32> {
    let (xs, ys, zs) = (flist(d.get_x(), step), flist(d.get_y(), step), flist(d.get_z(), step));
    let n = xs.len().min(ys.len()).min(zs.len());
    (0..n).map(|i| (xs[i] * xs[i] + ys[i] * ys[i] + zs[i] * zs[i]).sqrt()).collect()
}

/// Extract a downsampled model series from an rlog (path, lane lines, road edges,
/// lead — with z, in the device frame) + the segment's calibration. Frames thinned
/// to ~5 Hz, points to ~1/3, to keep it light.
pub fn extract_model(file: &str, raw: &[u8]) -> ModelArtifact {
    match decompress(file, raw) {
        Ok(data) => model_from_data(&data),
        Err(_) => ModelArtifact { rpy: [0.0; 3], frames: Vec::new() },
    }
}

fn model_from_data(data: &[u8]) -> ModelArtifact {
    use crate::cereal::log_capnp::event;
    const FRAME_STEP: usize = 4; // 20 Hz → ~5 Hz
    const PT_STEP: usize = 3;
    let opts = ReaderOptions { traversal_limit_in_words: Some(usize::MAX), nesting_limit: 128 };
    let mut cursor = std::io::Cursor::new(data);
    let mut base: Option<u64> = None;
    let mut frames = Vec::new();
    let mut rpy = [0.0f32; 3];
    let mut idx = 0usize;
    while let Ok(reader) = serialize::read_message(&mut cursor, opts) {
        let Ok(ev) = reader.get_root::<event::Reader>() else { continue };
        let mono = ev.get_log_mono_time();
        let b = *base.get_or_insert(mono);
        if let Ok(event::Which::LiveCalibration(Ok(lc))) = ev.which() {
            if let Ok(r) = lc.get_rpy_calib() {
                if r.len() >= 3 {
                    rpy = [r.get(0), r.get(1), r.get(2)];
                }
            }
            continue;
        }
        let Ok(event::Which::ModelV2(Ok(m))) = ev.which() else { continue };
        idx += 1;
        if idx % FRAME_STEP != 0 {
            continue;
        }
        let t = mono.saturating_sub(b) as f64 / 1_000_000_000.0;
        let path = m.get_position().map(|p| xy(p, PT_STEP)).unwrap_or(ModelXY { x: vec![], y: vec![], z: vec![] });
        let speed = m.get_velocity().map(|v| speed_norm(v, PT_STEP)).unwrap_or_default();
        let mut lanes = Vec::new();
        if let Ok(lls) = m.get_lane_lines() {
            let probs = m.get_lane_line_probs().ok();
            for (j, ll) in lls.iter().enumerate() {
                let prob = match &probs {
                    Some(p) if (j as u32) < p.len() => p.get(j as u32),
                    _ => 1.0,
                };
                if prob > 0.25 {
                    lanes.push(xy(ll, PT_STEP));
                }
            }
        }
        let mut edges = Vec::new();
        if let Ok(res) = m.get_road_edges() {
            for re in res.iter() {
                edges.push(xy(re, PT_STEP));
            }
        }
        let lead = m.get_leads_v3().ok().and_then(|leads| {
            if leads.len() == 0 {
                return None;
            }
            let l = leads.get(0);
            if l.get_prob() < 0.5 {
                return None;
            }
            let first = |r: ::capnp::Result<::capnp::primitive_list::Reader<'_, f32>>| {
                r.ok().and_then(|v| (v.len() > 0).then(|| v.get(0)))
            };
            Some(ModelLead {
                x: first(l.get_x())?,
                y: first(l.get_y()).unwrap_or(0.0),
                v: first(l.get_v()).unwrap_or(0.0),
            })
        });
        frames.push(ModelFrame { t, path, speed, lanes, edges, lead });
    }
    ModelArtifact { rpy, frames }
}

/// Parse an rlog's modelV2 into a `model.json` artifact (top-down + overlay).
pub async fn parse_model_and_store(
    state: &AppState,
    dongle: &str,
    ts: &str,
    seg: i64,
    file: &str,
    raw: &[u8],
) -> AppResult<()> {
    store_model(state, dongle, ts, seg, extract_model(file, raw)).await
}

async fn store_model(state: &AppState, dongle: &str, ts: &str, seg: i64, artifact: ModelArtifact) -> AppResult<()> {
    if artifact.frames.is_empty() {
        return Ok(());
    }
    let json = serde_json::to_vec(&artifact).unwrap_or_default();
    let key = blob_key(dongle, ts, seg, "model.json");
    state
        .blobs
        .put(&key, &json)
        .await
        .map_err(|e| crate::error::AppError::Other(anyhow::anyhow!("model.json write: {e}")))?;
    Ok(())
}

// ── EV battery / motor telemetry — parsed from the rlog's raw CAN ──────────────
// The IONIQ 5 logs pack power, current, voltage, state of charge and motor torque
// on bus 1 in messages the openpilot DBC doesn't define. Field positions and
// scales are the *provisional* calibration from the comma4 research
// (`/side_hustle/comma4/docs/regen-telemetry.md`). The qlog keeps only a few CAN
// frames a minute, so this needs the full rlog.

/// Fingerprints whose CAN layout the fields below were calibrated on.
const EV_FINGERPRINTS: [&str; 1] = ["HYUNDAI_IONIQ_5"];
const EV_BUS: u8 = 1;
const EV_BIN_S: f64 = 0.5;
/// Longest gap integrated across; a longer CAN dropout counts as no data.
const EV_MAX_DT: f64 = 1.0;

fn s16le(d: &[u8], off: usize) -> Option<f64> {
    d.get(off..off + 2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f64)
}
fn u16le(d: &[u8], off: usize) -> Option<f64> {
    d.get(off..off + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as f64)
}

/// One decoded EV reading.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EvField {
    /// Pack power, kW; positive = discharging.
    PowerKw(f64),
    /// Pack current, A; positive = discharging.
    CurrentA(f64),
    /// Pack voltage, V.
    Volts(f64),
    /// State of charge, raw 15-bit count (mapping to the dash % is unresolved).
    SocRaw(f64),
    /// Motor force at the wheels, kN; negative = regen braking.
    WheelKn(f64),
}

/// Decode one bus-1 CAN frame into an EV reading, if it's one we know.
pub fn decode_ev_frame(address: u32, dat: &[u8]) -> Option<EvField> {
    match address {
        0x250 => s16le(dat, 20).map(|r| EvField::PowerKw(r * 4.0 / 1000.0)),
        0x2fa => s16le(dat, 8).map(|r| EvField::CurrentA(r * 0.1)),
        0x235 => u16le(dat, 13).map(|r| EvField::Volts(r * 0.1)),
        0x25a => u16le(dat, 12).map(|r| EvField::SocRaw((r as u32 & 0x7fff) as f64)),
        0xda => s16le(dat, 12).map(|r| EvField::WheelKn(r * 3.85 / 1000.0)),
        _ => None,
    }
}

#[derive(Default)]
struct EvBin {
    n: [u32; 5],
    sum: [f64; 5],
}

/// Per-segment EV artifact (`ev.json`): 0.5 s averages, columnar, plus totals.
/// A series value is `null` where that field had no frame in the bin.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct EvArtifact {
    pub t: Vec<f64>,
    pub kw: Vec<Option<f32>>,
    pub amps: Vec<Option<f32>>,
    pub volts: Vec<Option<f32>>,
    pub soc_raw: Vec<Option<f32>>,
    pub wheel_kn: Vec<Option<f32>>,
    /// Energy drawn from the pack (kWh).
    pub kwh_out: f64,
    /// Energy put back by regen (kWh, positive).
    pub kwh_regen: f64,
    /// Seconds of power data integrated.
    pub seconds: f64,
    /// Distance from `carState.vEgo` over the segment (m).
    pub meters: f64,
}

/// Extract EV telemetry from a decompressed rlog. `None` when the car isn't one
/// the fields are calibrated for, or no power frames were logged.
pub fn extract_ev(data: &[u8]) -> Option<EvArtifact> {
    use crate::cereal::log_capnp::event;
    let opts = ReaderOptions { traversal_limit_in_words: Some(usize::MAX), nesting_limit: 128 };
    let mut cursor = std::io::Cursor::new(data);
    let mut base: Option<u64> = None;
    let mut fingerprint: Option<String> = None;
    let mut bins: std::collections::BTreeMap<i64, EvBin> = Default::default();
    let mut out = EvArtifact::default();
    let mut last_p: Option<(f64, f64)> = None; // (t, kW)
    let mut last_v: Option<(f64, f64)> = None; // (t, m/s)

    while let Ok(reader) = serialize::read_message(&mut cursor, opts) {
        let Ok(ev) = reader.get_root::<event::Reader>() else { continue };
        let mono = ev.get_log_mono_time();
        let b = *base.get_or_insert(mono);
        let t = mono.saturating_sub(b) as f64 / 1e9;
        match ev.which() {
            Ok(event::Which::CarParams(Ok(cp))) if fingerprint.is_none() => {
                if let Ok(f) = cp.get_car_fingerprint() {
                    fingerprint = f.to_string().ok();
                }
            }
            Ok(event::Which::CarState(Ok(cs))) => {
                let v = cs.get_v_ego() as f64;
                if let Some((t0, v0)) = last_v {
                    let dt = t - t0;
                    if dt > 0.0 && dt <= EV_MAX_DT {
                        out.meters += v0.max(0.0) * dt;
                    }
                }
                last_v = Some((t, v));
            }
            Ok(event::Which::Can(Ok(frames))) => {
                for fr in frames.iter() {
                    if fr.get_src() != EV_BUS {
                        continue;
                    }
                    let Ok(dat) = fr.get_dat() else { continue };
                    let Some(field) = decode_ev_frame(fr.get_address(), dat) else { continue };
                    let (i, val) = match field {
                        EvField::PowerKw(p) => {
                            if let Some((t0, p0)) = last_p {
                                let dt = t - t0;
                                if dt > 0.0 && dt <= EV_MAX_DT {
                                    let kwh = p0 * dt / 3600.0;
                                    if kwh >= 0.0 { out.kwh_out += kwh } else { out.kwh_regen -= kwh }
                                    out.seconds += dt;
                                }
                            }
                            last_p = Some((t, p));
                            (0, p)
                        }
                        EvField::CurrentA(a) => (1, a),
                        EvField::Volts(v) => (2, v),
                        EvField::SocRaw(s) => (3, s),
                        EvField::WheelKn(f) => (4, f),
                    };
                    let bin = bins.entry((t / EV_BIN_S).floor() as i64).or_default();
                    bin.n[i] += 1;
                    bin.sum[i] += val;
                }
            }
            _ => {}
        }
    }

    match fingerprint.as_deref() {
        Some(f) if EV_FINGERPRINTS.iter().any(|p| f.starts_with(p)) => {}
        _ => return None,
    }
    if bins.values().all(|b| b.n[0] == 0) {
        return None;
    }
    for (k, b) in &bins {
        let avg = |i: usize| (b.n[i] > 0).then(|| (b.sum[i] / b.n[i] as f64) as f32);
        out.t.push(*k as f64 * EV_BIN_S);
        out.kw.push(avg(0));
        out.amps.push(avg(1));
        out.volts.push(avg(2));
        out.soc_raw.push(avg(3));
        out.wheel_kn.push(avg(4));
    }
    Some(out)
}

/// One-line JSON summary of an rlog's EV totals (debug/CLI helper).
pub fn ev_summary(file: &str, raw: &[u8]) -> Option<String> {
    let data = decompress(file, raw).ok()?;
    let ev = extract_ev(&data)?;
    let soc: Vec<f32> = ev.soc_raw.iter().flatten().copied().collect();
    Some(serde_json::json!({
        "kwh_out": ev.kwh_out,
        "kwh_regen": ev.kwh_regen,
        "seconds": ev.seconds,
        "miles": ev.meters / 1609.344,
        "bins": ev.t.len(),
        "kw_max": ev.kw.iter().flatten().fold(f32::MIN, |a, b| a.max(*b)),
        "kw_min": ev.kw.iter().flatten().fold(f32::MAX, |a, b| a.min(*b)),
        "volts": ev.volts.iter().flatten().next(),
        "soc_raw": [soc.first(), soc.last()],
    }).to_string())
}

/// Parse a stored rlog once into its derived artifacts: `model.json` (top-down
/// model view) and, for calibrated EVs, `ev.json` + the segment's energy stats.
pub async fn parse_rlog_and_store(
    state: &AppState,
    dongle: &str,
    ts: &str,
    seg: i64,
    file: &str,
    raw: &[u8],
) -> AppResult<()> {
    let data = decompress(file, raw)?;
    store_model(state, dongle, ts, seg, model_from_data(&data)).await?;
    if let Some(ev) = extract_ev(&data) {
        let json = serde_json::to_vec(&ev).unwrap_or_default();
        state
            .blobs
            .put(&blob_key(dongle, ts, seg, "ev.json"), &json)
            .await
            .map_err(|e| crate::error::AppError::Other(anyhow::anyhow!("ev.json write: {e}")))?;
        if apply_ev_stats(state, dongle, ts, seg, &ev).await? {
            recompute_route(state, dongle, ts).await?;
        }
    }
    Ok(())
}

/// Write a segment's energy totals. Returns false if the segment row doesn't
/// exist yet (rlog before qlog); the qlog parse picks them up from `ev.json`.
async fn apply_ev_stats(state: &AppState, dongle: &str, ts: &str, seg: i64, ev: &EvArtifact) -> AppResult<bool> {
    let r = sqlx::query(
        "UPDATE segments SET ev_kwh_out = ?, ev_kwh_regen = ?, ev_seconds = ?, ev_meters = ? \
         WHERE canonical_name = ?",
    )
    .bind(ev.kwh_out)
    .bind(ev.kwh_regen)
    .bind(ev.seconds)
    .bind(ev.meters)
    .bind(format!("{dongle}|{ts}--{seg}"))
    .execute(&state.pool)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Re-apply a segment's energy totals from an existing `ev.json` (if any).
async fn apply_ev_stats_from_blob(state: &AppState, dongle: &str, ts: &str, seg: i64) -> AppResult<()> {
    let Ok(bytes) = state.blobs.get(&blob_key(dongle, ts, seg, "ev.json")).await else { return Ok(()) };
    if let Ok(ev) = serde_json::from_slice::<EvArtifact>(&bytes) {
        apply_ev_stats(state, dongle, ts, seg, &ev).await?;
    }
    Ok(())
}

/// Count key event types in a qlog (debug helper for deciding what's available).
pub fn inspect_qlog(file: &str, raw: &[u8]) -> Vec<(String, usize)> {
    use std::collections::BTreeMap;
    let Ok(data) = decompress(file, raw) else { return vec![] };
    let opts = ReaderOptions { traversal_limit_in_words: Some(usize::MAX), nesting_limit: 128 };
    let mut cursor = std::io::Cursor::new(&data[..]);
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    while let Ok(reader) = serialize::read_message(&mut cursor, opts) {
        let Ok(event) = reader.get_root::<event::Reader>() else { continue };
        let key = match event.which() {
            Ok(event::Which::ModelV2(_)) => "modelV2",
            Ok(event::Which::LiveCalibration(_)) => "liveCalibration",
            Ok(event::Which::CarState(_)) => "carState",
            Ok(event::Which::SelfdriveState(_)) => "selfdriveState",
            Ok(event::Which::SelfdriveStateSP(_)) => "selfdriveStateSP",
            Ok(event::Which::DriverMonitoringState(_)) => "driverMonitoringState",
            Ok(event::Which::GpsLocationExternal(_)) | Ok(event::Which::GpsLocation(_)) => "gpsLocation",
            Ok(event::Which::Thumbnail(_)) => "thumbnail",
            Ok(event::Which::RadarState(_)) => "radarState",
            Ok(_) => "other",
            Err(_) => "unknown",
        };
        *counts.entry(key).or_default() += 1;
    }
    counts.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// Re-parse every stored segment's qlog (regenerates routes/segments + the
/// coords/events/telemetry/sprite artifacts). Used after parser changes.
pub async fn reparse_all(state: &AppState) -> AppResult<usize> {
    let segs: Vec<(String, i64)> =
        sqlx::query_as("SELECT canonical_route_name, number FROM segments ORDER BY canonical_route_name, number")
            .fetch_all(&state.pool)
            .await?;
    let mut n = 0;
    for (route, seg) in segs {
        let Some((dongle, ts)) = route.split_once('|') else { continue };
        for f in ["qlog.zst", "qlog.bz2"] {
            let key = blob_key(dongle, ts, seg, f);
            if let Ok(bytes) = state.blobs.get(&key).await {
                if parse_and_store(state, dongle, ts, seg, f, &bytes).await.is_ok() {
                    n += 1;
                }
                break;
            }
        }
    }
    tracing::info!(segments = n, "reparsed");
    Ok(n)
}

/// Parse a qlog/rlog segment and persist its derived data + artifacts.
pub async fn parse_and_store(
    state: &AppState,
    dongle: &str,
    timestamp: &str,
    segment: i64,
    file: &str,
    raw: &[u8],
) -> AppResult<()> {
    let data = decompress(file, raw)?;
    let mut acc = Accum::default();
    let _ = segment; // (kept in the signature; time is derived from mono below)

    let opts = ReaderOptions {
        traversal_limit_in_words: Some(usize::MAX),
        nesting_limit: 128,
    };
    let mut cursor = std::io::Cursor::new(&data[..]);

    while let Ok(reader) = serialize::read_message(&mut cursor, opts) {
        let event = match reader.get_root::<event::Reader>() {
            Ok(e) => e,
            Err(_) => continue,
        };
        let mono = event.get_log_mono_time();
        let which = match event.which() {
            Ok(w) => w,
            Err(_) => continue,
        };
        accumulate(&mut acc, which, mono);
    }

    // Write artifacts to the blob store.
    if !acc.coords.is_empty() {
        let bytes = serde_json::to_vec(&acc.coords).unwrap_or_default();
        let key = blob_key(dongle, timestamp, segment, "coords.json");
        let _ = state.blobs.put(&key, &bytes).await;
    }
    if !acc.events.is_empty() {
        let bytes = serde_json::to_vec(&acc.events).unwrap_or_default();
        let key = blob_key(dongle, timestamp, segment, "events.json");
        let _ = state.blobs.put(&key, &bytes).await;
    }
    if !acc.telemetry.is_empty() {
        let bytes = serde_json::to_vec(&acc.telemetry).unwrap_or_default();
        let key = blob_key(dongle, timestamp, segment, "telemetry.json");
        let _ = state.blobs.put(&key, &bytes).await;
    }
    if !acc.thumbnails.is_empty() {
        if let Some(sprite) = build_sprite(&acc.thumbnails) {
            let key = blob_key(dongle, timestamp, segment, "sprite.jpg");
            let _ = state.blobs.put(&key, &sprite).await;
        }
    }

    upsert_segment(state, dongle, timestamp, segment, file, &acc).await?;
    apply_ev_stats_from_blob(state, dongle, timestamp, segment).await?;
    recompute_route(state, dongle, timestamp).await?;
    write_route_meta(state, dongle, timestamp, &acc).await?;

    // Keep the device's last-known GPS fresh.
    if let Some((lat, lng, ts)) = acc.last_gps {
        let _ = sqlx::query(
            "UPDATE devices SET last_gps_lat = ?, last_gps_lng = ?, last_gps_time = ? WHERE dongle_id = ?",
        )
        .bind(lat)
        .bind(lng)
        .bind(ts)
        .bind(dongle)
        .execute(&state.pool)
        .await;
    }

    tracing::info!(%dongle, %timestamp, segment, coords = acc.coords.len(),
        events = acc.events.len(), thumbs = acc.thumbnails.len(), miles = acc.total_meters / METERS_PER_MILE,
        "parsed segment");
    Ok(())
}

/// Emit a "state" timeline event when the combined engagement (lateral OR
/// longitudinal) or the alert level changes, closing the prior event's interval.
/// Called from both the SelfdriveState (longitudinal) and SelfdriveStateSP (MADS
/// lateral) handlers so either transition is captured.
fn emit_state_change(acc: &mut Accum, t: f64, mono: u64) {
    let engaged = acc.cur_engaged || acc.cur_lat;
    let cur = (engaged, acc.cur_alert);
    if acc.last_state == Some(cur) {
        return;
    }
    let route_offset_millis = (t * 1000.0) as i64;
    // Close the previous event's interval.
    if let Some(prev) = acc.events.last_mut() {
        if let Some(obj) = prev.data.as_object_mut() {
            obj.insert("end_route_offset_millis".into(), json!(route_offset_millis));
        }
    }
    acc.events.push(DriveEvent {
        kind: "state".into(),
        time: mono,
        route_offset_millis,
        data: json!({
            "state": if engaged { "enabled" } else { "disabled" },
            "enabled": engaged,
            "lat": acc.cur_lat,
            "long": acc.cur_engaged,
            "alertStatus": acc.cur_alert,
        }),
    });
    acc.last_state = Some(cur);
}

fn accumulate(acc: &mut Accum, which: event::WhichReader, mono: u64) {
    use event::Which::*;
    // Route-relative time: the first event (initData) carries the route-start
    // mono, so elapsed-since-route-start = the video timeline position.
    let base = *acc.route_base_mono.get_or_insert(mono);
    let t = mono.saturating_sub(base) as f64 / 1_000_000_000.0;
    match which {
        GpsLocationExternal(Ok(gps)) | GpsLocation(Ok(gps)) => {
            let has_fix = (gps.get_flags() % 2 == 1) || gps.get_has_fix();
            if !has_fix {
                return;
            }
            let lat = gps.get_latitude();
            let lng = gps.get_longitude();
            let ts = gps.get_unix_timestamp_millis();
            let speed = gps.get_speed();
            acc.has_gps = true;
            if acc.first_gps.is_none() {
                acc.first_gps = Some((lat, lng, ts));
            }
            acc.last_gps = Some((lat, lng, ts));

            let dist = match acc.last_pt {
                Some((plat, plng)) => haversine_m(plat, plng, lat, lng),
                None => 0.0,
            };
            acc.total_meters += dist;
            acc.last_pt = Some((lat, lng));

            acc.coords.push(Coord { t, lat, lng, speed, dist });
        }
        DeviceState(Ok(ds)) => {
            let temp = ds.get_max_temp_c();
            if temp > acc.max_temp {
                acc.max_temp = temp;
            }
            let fsp = ds.get_free_space_percent();
            if !acc.has_health || fsp < acc.min_free {
                acc.min_free = fsp;
            }
            let mem = ds.get_memory_usage_percent() as i32;
            if mem > acc.max_mem {
                acc.max_mem = mem;
            }
            acc.has_health = true;
        }
        CarParams(Ok(cp)) => {
            if let Ok(fp) = cp.get_car_fingerprint() {
                if let Ok(s) = fp.to_str() {
                    acc.car_fingerprint = s.to_string();
                }
            }
        }
        InitData(Ok(id)) => {
            if let Ok(v) = id.get_git_branch() {
                acc.git_branch = v.to_str().unwrap_or_default().to_string();
            }
            if let Ok(v) = id.get_git_commit() {
                acc.git_commit = v.to_str().unwrap_or_default().to_string();
            }
            if let Ok(v) = id.get_git_remote() {
                acc.git_remote = v.to_str().unwrap_or_default().to_string();
            }
            if let Ok(dt) = id.get_device_type() {
                acc.device_type = device_type_name(dt).to_string();
            }
        }
        SelfdriveState(Ok(ss)) => {
            acc.cur_engaged = ss.get_enabled(); // longitudinal/main engagement
            acc.seen_selfdrive = true;
            let engageable = ss.get_engageable();
            let mut alert_status = match ss.get_alert_status() {
                Ok(selfdrive_state::AlertStatus::Normal) => 0,
                Ok(selfdrive_state::AlertStatus::UserPrompt) => 1,
                Ok(selfdrive_state::AlertStatus::Critical) => 2,
                _ => 0,
            };
            if let Ok(size) = ss.get_alert_size() {
                if size != selfdrive_state::AlertSize::None && !engageable {
                    alert_status = alert_status.max(1);
                }
            }
            acc.cur_alert = alert_status;
            emit_state_change(acc, t, mono);
        }
        // sunnypilot MADS: lateral (steering) assist, independent of longitudinal.
        // This is why a steering-only assist must not be read off SelfdriveState
        // alone — `mads.active` is the lateral truth.
        SelfdriveStateSP(Ok(sp)) => {
            if let Ok(mads) = sp.get_mads() {
                acc.cur_lat = mads.get_active();
                acc.seen_selfdrive = true;
                emit_state_change(acc, t, mono);
            }
        }
        // Driver monitoring: face detection, distraction, and the awareness level.
        DriverMonitoringState(Ok(dm)) => {
            acc.cur_dm_active = dm.get_is_active_mode();
            acc.cur_dm_face = dm.get_face_detected();
            acc.cur_dm_distracted = dm.get_is_distracted();
            acc.cur_dm_aware = dm.get_awareness_status();
        }
        RadarState(Ok(rs)) => {
            acc.cur_lead = rs
                .get_lead_one()
                .ok()
                .filter(|l| l.get_status())
                .map(|l| (mono, l.get_d_rel(), l.get_v_rel()));
        }
        CarState(Ok(cs)) => {
            // Downsample to ~4 Hz. Wait until we've seen the engagement state in
            // this segment so the `engaged` field is never a stale default
            // (avoids a 1-sample disengage flicker at each segment boundary).
            if acc.seen_selfdrive && mono >= acc.last_telem_mono.saturating_add(250_000_000) {
                acc.last_telem_mono = mono;
                let gear = cs.get_gear_shifter().map(gear_name).unwrap_or("unknown");
                let cruise = cs.get_cruise_state().map(|c| c.get_enabled()).unwrap_or(false);
                let lead = acc.cur_lead.filter(|(m, _, _)| mono.saturating_sub(*m) < LEAD_STALE_NS);
                let r1 = |x: f32| (x * 10.0).round() / 10.0;
                acc.telemetry.push(Telem {
                    t,
                    speed: cs.get_v_ego() * 2.236_936, // m/s → mph
                    gear,
                    lb: cs.get_left_blinker(),
                    rb: cs.get_right_blinker(),
                    brake: cs.get_brake_pressed(),
                    gas: cs.get_gas_pressed(),
                    steer: cs.get_steering_angle_deg(),
                    steer_override: cs.get_steering_pressed(),
                    engaged: acc.cur_engaged || acc.cur_lat,
                    lat: acc.cur_lat,
                    long: acc.cur_engaged,
                    cruise,
                    soc: cs.get_fuel_gauge() * 100.0,
                    charging: cs.get_charging(),
                    dm_aware: if acc.cur_dm_active { acc.cur_dm_aware } else { -1.0 },
                    dm_distracted: acc.cur_dm_active && acc.cur_dm_distracted,
                    dm_face: acc.cur_dm_active && acc.cur_dm_face,
                    lead_d: lead.map(|(_, d, _)| r1(d)),
                    lead_v: lead.map(|(_, _, v)| r1(v)),
                });
            }
        }
        Can(_) => {
            acc.has_can = true;
        }
        Thumbnail(Ok(t)) => {
            if let Ok(bytes) = t.get_thumbnail() {
                acc.thumbnails.push(bytes.to_vec());
            }
        }
        _ => {}
    }
}

/// Stitch thumbnails into a horizontal sprite strip (128x80 each, JPEG q80).
fn build_sprite(thumbs: &[Vec<u8>]) -> Option<Vec<u8>> {
    use image::{imageops::FilterType, GenericImage, ImageEncoder, RgbImage};
    const TW: u32 = 128;
    const TH: u32 = 80;
    let n = thumbs.len() as u32;
    if n == 0 {
        return None;
    }
    let mut canvas = RgbImage::new(TW * n, TH);
    for (i, bytes) in thumbs.iter().enumerate() {
        let img = match image::load_from_memory(bytes) {
            Ok(i) => i,
            Err(_) => continue,
        };
        let scaled = img.resize_exact(TW, TH, FilterType::Lanczos3).to_rgb8();
        let _ = canvas.copy_from(&scaled, i as u32 * TW, 0);
    }
    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80);
    encoder
        .write_image(canvas.as_raw(), canvas.width(), canvas.height(), image::ExtendedColorType::Rgb8)
        .ok()?;
    Some(out)
}

/// File-type → (connectdata type, segment url column).
fn file_kind(file: &str) -> Option<(&'static str, &'static str)> {
    if file.contains("qcamera") {
        Some(("qcam", "qcam_url"))
    } else if file.contains("fcamera") {
        Some(("fcam", "fcam_url"))
    } else if file.contains("dcamera") {
        Some(("dcam", "dcam_url"))
    } else if file.contains("ecamera") {
        Some(("ecam", "ecam_url"))
    } else if file.contains("qlog") {
        Some(("qlog", "qlog_url"))
    } else if file.contains("rlog") {
        Some(("rlog", "rlog_url"))
    } else {
        None
    }
}

fn connectdata_url(public: &str, ty: &str, dongle: &str, ts: &str, seg: i64, file: &str) -> String {
    format!("{public}/connectdata/{ty}/{dongle}/{ts}/{seg}/{file}")
}

/// Ensure the route row exists (created lazily on first segment).
async fn ensure_route(state: &AppState, dongle: &str, timestamp: &str) -> AppResult<()> {
    let fullname = format!("{dongle}|{timestamp}");
    sqlx::query(
        "INSERT OR IGNORE INTO routes (fullname, device_dongle_id, created_at) VALUES (?, ?, ?)",
    )
    .bind(&fullname)
    .bind(dongle)
    .bind(now_millis())
    .execute(&state.pool)
    .await?;
    Ok(())
}

/// Upsert the segment row, setting GPS-derived fields and the qlog/rlog url.
async fn upsert_segment(
    state: &AppState,
    dongle: &str,
    timestamp: &str,
    segment: i64,
    file: &str,
    acc: &Accum,
) -> AppResult<()> {
    ensure_route(state, dongle, timestamp).await?;
    let canonical = format!("{dongle}|{timestamp}--{segment}");
    let route = format!("{dongle}|{timestamp}");
    let (slat, slng, sts) = acc.first_gps.unwrap_or((0.0, 0.0, 0));
    let (elat, elng, ets) = acc.last_gps.unwrap_or((0.0, 0.0, 0));
    let miles = (acc.total_meters / METERS_PER_MILE) as f64;
    let st = telemetry_stats(&acc.telemetry);

    sqlx::query(
        "INSERT INTO segments \
           (canonical_name, canonical_route_name, number, start_lat, start_lng, end_lat, end_lng, \
            miles, start_time_utc_millis, end_time_utc_millis, created_at, \
            engaged_meters, telem_meters, engaged_seconds, drive_seconds, max_speed, \
            disengage_count, hard_brake_count, hard_accel_count, max_temp, min_free, max_mem, \
            follow_seconds, follow_gap_sum, close_seconds, \
            follow_op_seconds, follow_op_gap_sum, close_op_seconds, min_gap) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, \
                 ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(canonical_name) DO UPDATE SET \
           start_lat = excluded.start_lat, start_lng = excluded.start_lng, \
           end_lat = excluded.end_lat, end_lng = excluded.end_lng, miles = excluded.miles, \
           start_time_utc_millis = excluded.start_time_utc_millis, \
           end_time_utc_millis = excluded.end_time_utc_millis, \
           engaged_meters = excluded.engaged_meters, telem_meters = excluded.telem_meters, \
           engaged_seconds = excluded.engaged_seconds, drive_seconds = excluded.drive_seconds, \
           max_speed = excluded.max_speed, disengage_count = excluded.disengage_count, \
           hard_brake_count = excluded.hard_brake_count, hard_accel_count = excluded.hard_accel_count, \
           max_temp = excluded.max_temp, min_free = excluded.min_free, max_mem = excluded.max_mem, \
           follow_seconds = excluded.follow_seconds, follow_gap_sum = excluded.follow_gap_sum, \
           close_seconds = excluded.close_seconds, follow_op_seconds = excluded.follow_op_seconds, \
           follow_op_gap_sum = excluded.follow_op_gap_sum, close_op_seconds = excluded.close_op_seconds, \
           min_gap = excluded.min_gap",
    )
    .bind(&canonical)
    .bind(&route)
    .bind(segment)
    .bind(slat)
    .bind(slng)
    .bind(elat)
    .bind(elng)
    .bind(miles)
    .bind(sts)
    .bind(ets)
    .bind(now_millis())
    .bind(st.engaged_m)
    .bind(st.telem_m)
    .bind(st.engaged_s)
    .bind(st.drive_s)
    .bind(st.max_speed)
    .bind(st.disengage)
    .bind(st.hard_brake)
    .bind(st.hard_accel)
    .bind(acc.max_temp)
    .bind(if acc.has_health { acc.min_free } else { -1.0 })
    .bind(acc.max_mem)
    .bind(st.follow_s)
    .bind(st.follow_gap_sum)
    .bind(st.close_s)
    .bind(st.follow_op_s)
    .bind(st.follow_op_gap_sum)
    .bind(st.close_op_s)
    .bind(st.min_gap)
    .execute(&state.pool)
    .await?;

    // Set this file's url, plus any sibling cam blobs already uploaded.
    set_segment_file(state, dongle, timestamp, segment, file).await?;
    for cam in ["qcamera.ts", "fcamera.hevc", "dcamera.hevc", "ecamera.hevc"] {
        let key = blob_key(dongle, timestamp, segment, cam);
        if state.blobs.exists(&key).await {
            set_segment_file(state, dongle, timestamp, segment, cam).await?;
        }
    }
    Ok(())
}

/// Set the appropriate *_url column for one uploaded file on a segment row,
/// creating the segment/route if they don't exist yet (e.g. a camera uploaded
/// before its qlog is parsed).
pub async fn set_segment_file(
    state: &AppState,
    dongle: &str,
    timestamp: &str,
    segment: i64,
    file: &str,
) -> AppResult<()> {
    let Some((ty, col)) = file_kind(file) else {
        return Ok(());
    };
    ensure_route(state, dongle, timestamp).await?;
    let canonical = format!("{dongle}|{timestamp}--{segment}");
    let route = format!("{dongle}|{timestamp}");
    // Create a bare segment row if absent.
    sqlx::query(
        "INSERT OR IGNORE INTO segments (canonical_name, canonical_route_name, number, created_at) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(&canonical)
    .bind(&route)
    .bind(segment)
    .bind(now_millis())
    .execute(&state.pool)
    .await?;

    let url = connectdata_url(&state.config.public_url, ty, dongle, timestamp, segment, file);
    // Static SQL per column (sqlx 0.9 only accepts &'static str for `query`).
    let sql = match col {
        "qcam_url" => "UPDATE segments SET qcam_url = ? WHERE canonical_name = ?",
        "fcam_url" => "UPDATE segments SET fcam_url = ? WHERE canonical_name = ?",
        "dcam_url" => "UPDATE segments SET dcam_url = ? WHERE canonical_name = ?",
        "ecam_url" => "UPDATE segments SET ecam_url = ? WHERE canonical_name = ?",
        "qlog_url" => "UPDATE segments SET qlog_url = ? WHERE canonical_name = ?",
        "rlog_url" => "UPDATE segments SET rlog_url = ? WHERE canonical_name = ?",
        _ => return Ok(()),
    };
    sqlx::query(sql).bind(&url).bind(&canonical).execute(&state.pool).await?;
    Ok(())
}

/// Write platform/git metadata onto the route from a parsed qlog (only fields
/// that are non-empty overwrite existing values).
async fn write_route_meta(
    state: &AppState,
    dongle: &str,
    timestamp: &str,
    acc: &Accum,
) -> AppResult<()> {
    let fullname = format!("{dongle}|{timestamp}");
    sqlx::query(
        "UPDATE routes SET \
           platform   = CASE WHEN ? != '' THEN ? ELSE platform   END, \
           git_remote = CASE WHEN ? != '' THEN ? ELSE git_remote END, \
           git_branch = CASE WHEN ? != '' THEN ? ELSE git_branch END, \
           git_commit = CASE WHEN ? != '' THEN ? ELSE git_commit END \
         WHERE fullname = ?",
    )
    .bind(&acc.car_fingerprint).bind(&acc.car_fingerprint)
    .bind(&acc.git_remote).bind(&acc.git_remote)
    .bind(&acc.git_branch).bind(&acc.git_branch)
    .bind(&acc.git_commit).bind(&acc.git_commit)
    .bind(&fullname)
    .execute(&state.pool)
    .await?;
    Ok(())
}

/// Recompute the route aggregate (times, bbox, length, max* columns, segment
/// time arrays) from its segment rows.
pub async fn recompute_route(state: &AppState, dongle: &str, timestamp: &str) -> AppResult<()> {
    let fullname = format!("{dongle}|{timestamp}");

    #[derive(sqlx::FromRow)]
    struct SegRow {
        number: i64,
        qcam_url: String,
        fcam_url: String,
        dcam_url: String,
        ecam_url: String,
        qlog_url: String,
        rlog_url: String,
        start_lat: f64,
        start_lng: f64,
        end_lat: f64,
        end_lng: f64,
        miles: f64,
        start_time_utc_millis: i64,
        end_time_utc_millis: i64,
        engaged_meters: f64,
        telem_meters: f64,
        engaged_seconds: f64,
        drive_seconds: f64,
        max_speed: f64,
        disengage_count: i64,
        hard_brake_count: i64,
        hard_accel_count: i64,
        max_temp: f64,
        min_free: f64,
        max_mem: i64,
        ev_kwh_out: f64,
        ev_kwh_regen: f64,
        ev_seconds: f64,
        ev_meters: f64,
        follow_seconds: f64,
        follow_gap_sum: f64,
        close_seconds: f64,
        follow_op_seconds: f64,
        follow_op_gap_sum: f64,
        close_op_seconds: f64,
        min_gap: f64,
    }

    let segs: Vec<SegRow> = sqlx::query_as(
        "SELECT number, qcam_url, fcam_url, dcam_url, ecam_url, qlog_url, rlog_url, \
                start_lat, start_lng, end_lat, end_lng, miles, \
                start_time_utc_millis, end_time_utc_millis, \
                engaged_meters, telem_meters, engaged_seconds, drive_seconds, max_speed, \
                disengage_count, hard_brake_count, hard_accel_count, max_temp, min_free, max_mem, \
                ev_kwh_out, ev_kwh_regen, ev_seconds, ev_meters, \
                follow_seconds, follow_gap_sum, close_seconds, \
                follow_op_seconds, follow_op_gap_sum, close_op_seconds, min_gap \
         FROM segments WHERE canonical_route_name = ? ORDER BY number ASC",
    )
    .bind(&fullname)
    .fetch_all(&state.pool)
    .await?;

    if segs.is_empty() {
        return Ok(());
    }

    let numbers: Vec<i64> = segs.iter().map(|s| s.number).collect();
    let starts: Vec<i64> = segs.iter().map(|s| s.start_time_utc_millis).collect();
    let ends: Vec<i64> = segs.iter().map(|s| s.end_time_utc_millis).collect();
    let length: f64 = segs.iter().map(|s| s.miles).sum();

    let with_gps_first = segs.iter().find(|s| s.start_time_utc_millis != 0);
    let with_gps_last = segs.iter().rev().find(|s| s.end_time_utc_millis != 0);
    let (start_lat, start_lng, start_t) = with_gps_first
        .map(|s| (s.start_lat, s.start_lng, s.start_time_utc_millis))
        .unwrap_or((0.0, 0.0, 0));
    let (end_lat, end_lng, end_t) = with_gps_last
        .map(|s| (s.end_lat, s.end_lng, s.end_time_utc_millis))
        .unwrap_or((0.0, 0.0, 0));

    let max_of = |pred: &dyn Fn(&SegRow) -> bool| -> i64 {
        segs.iter().filter(|s| pred(s)).map(|s| s.number).max().unwrap_or(-1)
    };
    let maxqcamera = max_of(&|s| !s.qcam_url.is_empty());
    let maxcamera = max_of(&|s| !s.fcam_url.is_empty());
    let maxdcamera = max_of(&|s| !s.dcam_url.is_empty());
    let maxecamera = max_of(&|s| !s.ecam_url.is_empty());
    let maxqlog = max_of(&|s| !s.qlog_url.is_empty());
    let maxlog = max_of(&|s| !s.rlog_url.is_empty());

    let engaged_m: f64 = segs.iter().map(|s| s.engaged_meters).sum();
    let telem_m: f64 = segs.iter().map(|s| s.telem_meters).sum();
    let engaged_s: f64 = segs.iter().map(|s| s.engaged_seconds).sum();
    let drive_s: f64 = segs.iter().map(|s| s.drive_seconds).sum();
    let max_speed: f64 = segs.iter().map(|s| s.max_speed).fold(0.0, f64::max);
    let disengage: i64 = segs.iter().map(|s| s.disengage_count).sum();
    let hard_brake: i64 = segs.iter().map(|s| s.hard_brake_count).sum();
    let hard_accel: i64 = segs.iter().map(|s| s.hard_accel_count).sum();
    let max_temp: f64 = segs.iter().map(|s| s.max_temp).fold(0.0, f64::max);
    let min_free: f64 = segs
        .iter()
        .map(|s| s.min_free)
        .filter(|&f| f >= 0.0)
        .fold(f64::MAX, f64::min);
    let min_free = if min_free == f64::MAX { -1.0 } else { min_free };
    let max_mem: i64 = segs.iter().map(|s| s.max_mem).max().unwrap_or(0);
    let ev_out: f64 = segs.iter().map(|s| s.ev_kwh_out).sum();
    let ev_regen: f64 = segs.iter().map(|s| s.ev_kwh_regen).sum();
    let ev_secs: f64 = segs.iter().map(|s| s.ev_seconds).sum();
    let ev_m: f64 = segs.iter().map(|s| s.ev_meters).sum();
    let follow: [f64; 6] = segs.iter().fold([0.0; 6], |mut a, s| {
        for (x, v) in a.iter_mut().zip([
            s.follow_seconds, s.follow_gap_sum, s.close_seconds,
            s.follow_op_seconds, s.follow_op_gap_sum, s.close_op_seconds,
        ]) {
            *x += v;
        }
        a
    });
    let min_gap = segs.iter().map(|s| s.min_gap).filter(|&g| g >= 0.0).fold(f64::INFINITY, f64::min);
    let min_gap = if min_gap.is_finite() { min_gap } else { -1.0 };

    sqlx::query(
        "UPDATE routes SET \
           start_time_utc_millis = ?, end_time_utc_millis = ?, \
           start_lat = ?, start_lng = ?, end_lat = ?, end_lng = ?, length = ?, \
           segment_numbers = ?, segment_start_times = ?, segment_end_times = ?, \
           maxqcamera = ?, maxcamera = ?, maxdcamera = ?, maxecamera = ?, maxqlog = ?, maxlog = ?, \
           engaged_meters = ?, telem_meters = ?, engaged_seconds = ?, drive_seconds = ?, \
           max_speed = ?, disengage_count = ?, hard_brake_count = ?, hard_accel_count = ?, \
           max_temp = ?, min_free = ?, max_mem = ?, \
           ev_kwh_out = ?, ev_kwh_regen = ?, ev_seconds = ?, ev_meters = ?, \
           follow_seconds = ?, follow_gap_sum = ?, close_seconds = ?, \
           follow_op_seconds = ?, follow_op_gap_sum = ?, close_op_seconds = ?, min_gap = ? \
         WHERE fullname = ?",
    )
    .bind(start_t)
    .bind(end_t)
    .bind(start_lat)
    .bind(start_lng)
    .bind(end_lat)
    .bind(end_lng)
    .bind(length)
    .bind(serde_json::to_string(&numbers).unwrap_or_else(|_| "[]".into()))
    .bind(serde_json::to_string(&starts).unwrap_or_else(|_| "[]".into()))
    .bind(serde_json::to_string(&ends).unwrap_or_else(|_| "[]".into()))
    .bind(maxqcamera)
    .bind(maxcamera)
    .bind(maxdcamera)
    .bind(maxecamera)
    .bind(maxqlog)
    .bind(maxlog)
    .bind(engaged_m)
    .bind(telem_m)
    .bind(engaged_s)
    .bind(drive_s)
    .bind(max_speed)
    .bind(disengage)
    .bind(hard_brake)
    .bind(hard_accel)
    .bind(max_temp)
    .bind(min_free)
    .bind(max_mem)
    .bind(ev_out)
    .bind(ev_regen)
    .bind(ev_secs)
    .bind(ev_m)
    .bind(follow[0])
    .bind(follow[1])
    .bind(follow[2])
    .bind(follow[3])
    .bind(follow[4])
    .bind(follow[5])
    .bind(min_gap)
    .bind(&fullname)
    .execute(&state.pool)
    .await?;

    Ok(())
}
