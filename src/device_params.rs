//! Curated read/write of device (openpilot/sunnypilot) params over SSH — the
//! basis for replacing sunnylink for device settings. Athena has no `setParam`,
//! so SSH is the lever.
//!
//! Params are 0600 files under `/data/params/d`; we write them the same atomic
//! way openpilot does (a temp file in `/data/params` + an flock'd rename), so a
//! concurrent openpilot write can't tear. **Only allowlisted keys are writable**
//! and values are validated per kind, so the UI can't brick the device. Reads are
//! likewise limited to the allowlist (no dumping arbitrary params / secrets).
//!
//! The spec values mirror sunnypilot's settings layouts
//! (`selfdrive/ui/sunnypilot/layouts/settings/`). Note: some sliders there store
//! the *mapped* value (via `value_map`), not the slider index — those are modeled
//! here as `Enum`s of the exact valid values (e.g. MaxTimeOffroad, the screen-off
//! timer). Raw sliders without a value_map are `Int`s.

use crate::db::now_millis;
use crate::device_ssh;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Bool,
    Int,  // integer in [min, max]
    Enum, // one of `options`
    Info, // read-only (informational)
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Bool => "bool",
            Kind::Int => "int",
            Kind::Enum => "enum",
            Kind::Info => "info",
        }
    }
}

pub struct Spec {
    pub key: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub kind: Kind,
    pub help: &'static str,
    /// (value, label) choices for `Enum`.
    pub options: &'static [(&'static str, &'static str)],
    /// Inclusive bounds + step for `Int` (ignored otherwise).
    pub min: i64,
    pub max: i64,
    pub step: i64,
    /// Optional unit suffix shown in the UI for `Int` (e.g. "%", "s").
    pub unit: &'static str,
    /// Conditional: this setting is only active when param `dep_key`'s current
    /// value is one of `dep_values`. Empty `dep_key` = always active.
    pub dep_key: &'static str,
    pub dep_values: &'static [&'static str],
}

// Concise constructors keep the (large) allowlist readable.
const fn b(key: &'static str, group: &'static str, label: &'static str, help: &'static str) -> Spec {
    Spec { key, group, label, kind: Kind::Bool, help, options: &[], min: 0, max: 0, step: 0, unit: "",
        dep_key: "", dep_values: &[] }
}
const fn e(key: &'static str, group: &'static str, label: &'static str, help: &'static str,
    options: &'static [(&'static str, &'static str)]) -> Spec {
    Spec { key, group, label, kind: Kind::Enum, help, options, min: 0, max: 0, step: 0, unit: "",
        dep_key: "", dep_values: &[] }
}
const fn int_(key: &'static str, group: &'static str, label: &'static str, help: &'static str,
    min: i64, max: i64, step: i64, unit: &'static str) -> Spec {
    Spec { key, group, label, kind: Kind::Int, help, options: &[], min, max, step, unit,
        dep_key: "", dep_values: &[] }
}
const fn info(key: &'static str, group: &'static str, label: &'static str) -> Spec {
    Spec { key, group, label, kind: Kind::Info, help: "", options: &[], min: 0, max: 0, step: 0, unit: "",
        dep_key: "", dep_values: &[] }
}
/// Wrap a spec with a conditional dependency: only active when `key`'s value is
/// in `values`.
const fn dep(mut s: Spec, key: &'static str, values: &'static [&'static str]) -> Spec {
    s.dep_key = key;
    s.dep_values = values;
    s
}

/// The allowlist. Reversible, user-facing sunnypilot/openpilot settings only —
/// nothing touching identity, calibration, credentials, tuning blobs, or safety
/// internals. Grouped for display in source order.
pub const SPECS: &[Spec] = &[
    // ── Driving ──────────────────────────────────────────────────────────────
    b("OpenpilotEnabledToggle", "Driving", "openpilot enabled",
        "The master switch for openpilot. When off, all openpilot driving assistance is disabled and the car drives stock."),
    b("ExperimentalMode", "Driving", "Experimental mode",
        "Lets the driving model control gas and brake end-to-end — it can slow for turns and stop for lights and stop signs — instead of the simpler adaptive-cruise longitudinal. More capable, but less predictable. Requires openpilot longitudinal."),
    b("AlphaLongitudinalEnabled", "Driving", "openpilot longitudinal",
        "Let openpilot control acceleration and braking. When off, the car's stock adaptive cruise handles speed and openpilot only steers. This must be on before Experimental mode does anything."),
    b("DynamicExperimentalControl", "Driving", "Dynamic experimental control",
        "Automatically switches between Experimental and standard longitudinal based on the driving situation, so you don't have to pick one manually."),
    b("DisengageOnAccelerator", "Driving", "Disengage on gas",
        "When on, pressing the accelerator pedal disengages openpilot and hands control back to you. When off, openpilot stays engaged and resumes when you lift off the pedal."),
    e("LongitudinalPersonality", "Driving", "Following distance",
        "How much gap openpilot keeps behind the car ahead. Relaxed leaves more room; Aggressive follows closer.",
        &[("0", "Relaxed"), ("1", "Standard"), ("2", "Aggressive")]),
    e("HyundaiLongitudinalTuning", "Driving", "Hyundai longitudinal tuning",
        "Alternative gas/brake tuning for Hyundai/Kia/Genesis. Dynamic and Predictive change how smoothly and proactively openpilot accelerates and brakes; Off uses the default tune.",
        &[("0", "Off"), ("1", "Dynamic"), ("2", "Predictive")]),

    // ── Cruise ───────────────────────────────────────────────────────────────
    b("IntelligentCruiseButtonManagement", "Cruise", "Intelligent cruise buttons (alpha)",
        "Experimental. Lets openpilot press the cruise set-speed buttons for you (for example to apply map or vision speeds) instead of you doing it manually."),
    b("SmartCruiseControlVision", "Cruise", "Vision curve slowing",
        "Uses the road-facing camera to spot upcoming curves and slow down for them automatically."),
    b("SmartCruiseControlMap", "Cruise", "Map curve/speed slowing",
        "Uses offline map data (road curvature and posted limits) to slow for curves and limits ahead, even before the camera can see them."),
    b("CustomAccIncrementsEnabled", "Cruise", "Custom ACC speed steps",
        "Use your own set-speed step sizes for the cruise buttons instead of the car's defaults."),
    dep(int_("CustomAccShortPressIncrement", "Cruise", "ACC short-press step",
        "How much the set speed changes for a single short press of a cruise +/- button.", 1, 10, 1, ""),
        "CustomAccIncrementsEnabled", &["1"]),

    // ── Speed limits ─────────────────────────────────────────────────────────
    e("SpeedLimitMode", "Speed limits", "Speed limit control",
        "What openpilot does with posted speed limits. Off ignores them; Info just shows the limit; Warning alerts you when you exceed it; Assist actively sets your cruise speed to the limit.",
        &[("0", "Off"), ("1", "Info"), ("2", "Warning"), ("3", "Assist")]),
    e("SpeedLimitPolicy", "Speed limits", "Speed limit source",
        "Where the posted limit comes from: the car's own sign recognition, offline maps (OpenStreetMap), or a mix — and which to prefer when both are available.",
        &[("0", "Car only"), ("1", "Map only"), ("2", "Car first"), ("3", "Map first"), ("4", "Combined")]),
    e("SpeedLimitOffsetType", "Speed limits", "Speed limit offset type",
        "Add a margin to the posted limit so you travel slightly above (or below) it. None applies no margin; Fixed adds a set amount; Percent scales the margin with the limit.",
        &[("0", "None"), ("1", "Fixed"), ("2", "Percent")]),
    dep(int_("SpeedLimitValueOffset", "Speed limits", "Speed limit offset",
        "The amount added to the posted limit — in speed units for Fixed, or in percent for Percent.", -30, 30, 1, ""),
        "SpeedLimitOffsetType", &["1", "2"]),

    // ── Steering (MADS) ──────────────────────────────────────────────────────
    b("Mads", "Steering (MADS)", "Enable MADS",
        "Modular Assistive Driving System. Lets lane-keeping steering engage and stay on independently of cruise/longitudinal — so you can have steering without adaptive cruise, and keep steering through braking."),
    dep(b("MadsMainCruiseAllowed", "Steering (MADS)", "Engage with main cruise",
        "Allow MADS steering to turn on when you press the car's MAIN cruise button, not just the dedicated LKAS/steering button."),
        "Mads", &["1"]),
    dep(b("MadsUnifiedEngagementMode", "Steering (MADS)", "Unified engagement",
        "Engage steering and longitudinal together with a single action, instead of controlling them separately."),
        "Mads", &["1"]),
    dep(e("MadsSteeringMode", "Steering (MADS)", "On brake pedal",
        "What MADS steering does when you press the brake: keep steering, pause until you release the brake, or fully disengage.",
        &[("0", "Remain active"), ("1", "Pause"), ("2", "Disengage")]),
        "Mads", &["1"]),
    dep(b("NeuralNetworkLateralControl", "Steering (MADS)", "Neural-net lateral control",
        "Use sunnypilot's neural-network steering model (NNLC) for smoother, car-specific steering, when a model exists for your vehicle. Mutually exclusive with Enforce torque control."),
        "EnforceTorqueControl", &["0"]),
    dep(b("EnforceTorqueControl", "Steering (MADS)", "Enforce torque control",
        "Force the classic torque-based steering controller instead of the neural-network one. Mutually exclusive with Neural-net lateral control."),
        "NeuralNetworkLateralControl", &["0"]),
    b("BlinkerPauseLateralControl", "Steering (MADS)", "Pause steering on blinker",
        "While your turn signal is on, hand steering back to you so you can change lanes manually, then resume afterward."),
    dep(int_("BlinkerMinLateralControlSpeed", "Steering (MADS)", "Min speed to pause on blinker",
        "Only pause steering for the blinker below this speed. Above it, steering stays engaged through the signal.", 0, 255, 5, ""),
        "BlinkerPauseLateralControl", &["1"]),
    dep(int_("BlinkerLateralReengageDelay", "Steering (MADS)", "Post-blinker delay",
        "After the turn signal turns off, wait this many seconds before openpilot resumes steering.", 0, 10, 1, "s"),
        "BlinkerPauseLateralControl", &["1"]),
    e("AutoLaneChangeTimer", "Steering (MADS)", "Auto lane change",
        "Complete a lane change after the blinker is on without nudging the wheel. Nudge needs a wheel nudge; Nudgeless changes immediately; the timed options wait that long first. Use with caution and only where traffic permits.",
        &[("-1", "Off"), ("0", "Nudge"), ("1", "Nudgeless"), ("2", "0.5 s"), ("3", "1 s"), ("4", "2 s"), ("5", "3 s")]),
    dep(b("AutoLaneChangeBsmDelay", "Steering (MADS)", "Blind-spot lane-change delay",
        "If blind-spot monitoring sees a vehicle, delay the auto lane change until that lane is clear."),
        "AutoLaneChangeTimer", &["1", "2", "3", "4", "5"]),

    // ── Display & alerts ─────────────────────────────────────────────────────
    b("IsLdwEnabled", "Display & alerts", "Lane-departure warnings",
        "When openpilot is NOT engaged, warn you if the car drifts out of its lane."),
    b("BlindSpot", "Display & alerts", "Blind-spot warnings",
        "Show a warning on screen when the car's blind-spot monitor detects a vehicle beside you."),
    b("GreenLightAlert", "Display & alerts", "Green-light alert",
        "Chime when a traffic light you're stopped at turns green."),
    b("LeadDepartAlert", "Display & alerts", "Lead-departure alert",
        "Chime when the car ahead of you starts pulling away while you're stopped."),
    b("RoadNameToggle", "Display & alerts", "Show road name",
        "Show the name of the current road on the driving screen (from map data)."),
    b("ShowTurnSignals", "Display & alerts", "Turn-signal icons",
        "Show blinker indicator arrows on the driving screen."),
    b("TorqueBar", "Display & alerts", "Steering arc",
        "Show an arc indicating how much steering torque openpilot is applying."),
    b("RocketFuel", "Display & alerts", "Acceleration bar",
        "Show a real-time acceleration/deceleration bar on the screen."),
    b("StandstillTimer", "Display & alerts", "Standstill timer",
        "While stopped, show how long you've been stationary."),
    b("TrueVEgoUI", "Display & alerts", "Always show true speed",
        "Show your actual measured speed on the speedometer instead of the dash's slightly-optimistic reading."),
    b("HideVEgoUI", "Display & alerts", "Hide speedometer",
        "Hide the speed readout from the driving screen entirely."),
    b("RainbowMode", "Display & alerts", "Rainbow path",
        "Draw the predicted driving path in rainbow colors. Purely cosmetic."),
    e("ChevronInfo", "Display & alerts", "Metrics below chevron",
        "Choose what's shown beneath the lead-car marker (the chevron): distance to it, its relative speed, the time gap, or all of them.",
        &[("0", "Off"), ("1", "Distance"), ("2", "Speed"), ("3", "Time"), ("4", "All")]),
    e("DevUIInfo", "Display & alerts", "Developer UI",
        "Show extra developer telemetry on the driving screen, and where to place it.",
        &[("0", "Off"), ("1", "Bottom"), ("2", "Right"), ("3", "Right & bottom")]),
    e("OnroadScreenOffBrightness", "Display & alerts", "Onroad brightness",
        "Screen brightness while driving. Auto follows the light sensor; Screen off turns the display off; or pick a fixed level.",
        &[("0", "Auto"), ("1", "Auto (dark)"), ("2", "Screen off"), ("7", "25%"), ("12", "50%"), ("17", "75%"), ("22", "100%")]),
    dep(e("OnroadScreenOffTimer", "Display & alerts", "Onroad brightness delay",
        "How long to wait before dimming or turning off the screen while driving. Only applies when brightness above isn't Auto.",
        &[("3", "3 s"), ("5", "5 s"), ("7", "7 s"), ("10", "10 s"), ("15", "15 s"), ("30", "30 s"),
          ("60", "1 min"), ("120", "2 min"), ("180", "3 min"), ("240", "4 min"), ("300", "5 min"),
          ("360", "6 min"), ("420", "7 min"), ("480", "8 min"), ("540", "9 min"), ("600", "10 min")]),
        "OnroadScreenOffBrightness", &["2", "7", "12", "17", "22"]),
    int_("InteractivityTimeout", "Display & alerts", "Settings UI timeout",
        "How long the on-device settings screen stays open without interaction before closing. 0 leaves it at the default.", 0, 120, 10, "s"),

    // ── Recording ────────────────────────────────────────────────────────────
    b("RecordFront", "Recording", "Record driver camera",
        "Record video from the driver-facing (interior) camera along with your drives."),
    b("RecordAudio", "Recording", "Record microphone",
        "Record cabin microphone audio with your drives."),
    b("RecordAudioFeedback", "Recording", "Record audio feedback",
        "When you submit feedback on the device, also save a short audio clip from around that moment."),

    // ── Device power ─────────────────────────────────────────────────────────
    e("DeviceBootMode", "Device power", "Wake-up behavior",
        "What the device does when it wakes up: the default behavior, or go straight into offroad (parked) mode.",
        &[("0", "Default"), ("1", "Offroad")]),
    b("OffroadMode", "Device power", "Always offroad",
        "Keep the device in offroad (parked) mode and stop it going onroad. Handy for maintenance — turn it off to drive."),
    e("MaxTimeOffroad", "Device power", "Max time offroad",
        "How long the device stays powered after you park before shutting down to protect the 12V battery. Always on never powers down.",
        &[("0", "Always on"), ("5", "5 min"), ("10", "10 min"), ("15", "15 min"), ("30", "30 min"),
          ("60", "1 hour"), ("120", "2 hours"), ("180", "3 hours"), ("300", "5 hours"),
          ("600", "10 hours"), ("1440", "24 hours"), ("1800", "30 hours")]),
    b("QuietMode", "Device power", "Quiet mode",
        "Suppress most non-critical chimes and spoken alerts."),

    // ── Connectivity & updates ───────────────────────────────────────────────
    // Read-only on purpose: SshEnabled start/stops the whole sshd service (via an
    // immediate `ssh-param-watcher.path` unit), and SSH is the channel homeconnect
    // itself uses for sync, device settings, and model selection. Turning it off
    // here would cut homeconnect's access with no way to re-enable remotely (you'd
    // have to re-enable it on the device's own screen). So we only display it.
    Spec {
        key: "SshEnabled", group: "Connectivity & updates", label: "SSH service",
        kind: Kind::Info,
        help: "Whether the device's SSH service is running (1 = on). homeconnect uses SSH for sync, device settings, and model selection, so it's shown here but not toggled — turning SSH off can't be undone remotely. Change it on the device screen (Settings → Developer → SSH).",
        options: &[], min: 0, max: 0, step: 0, unit: "", dep_key: "", dep_values: &[],
    },
    b("AdbEnabled", "Connectivity & updates", "ADB enabled",
        "Allow Android Debug Bridge (ADB) connections — a developer tool for shell and file access to the device over USB or network."),
    b("GsmMetered", "Connectivity & updates", "Cellular metered",
        "Treat the cellular (SIM) connection as metered so the device limits background data and large uploads over it."),
    b("OnroadUploads", "Connectivity & updates", "Upload while driving",
        "Allow the device to upload data while you're driving, not only when parked on Wi-Fi."),
    b("SunnylinkEnabled", "Connectivity & updates", "sunnylink enabled",
        "Connect the device to sunnylink — sunnypilot's companion service for backups, model sync, and sponsor features."),
    dep(b("DisableUpdates", "Connectivity & updates", "Pause software updates",
        "Stop the device from fetching and installing software updates. Advanced — your software will go stale."),
        "ShowAdvancedControls", &["1"]),

    // ── Developer ────────────────────────────────────────────────────────────
    b("ShowAdvancedControls", "Developer", "Show advanced controls",
        "Reveal advanced and developer settings on the device's own screen. A few settings here only take effect when this is on."),
    dep(b("QuickBootToggle", "Developer", "Quickboot mode",
        "Boot faster by skipping some startup steps. Advanced; only relevant while updates are paused."),
        "DisableUpdates", &["1"]),

    // ── Device (read-only) ───────────────────────────────────────────────────
    info("Version", "Device", "Version"),
    info("GitBranch", "Device", "Branch"),
    info("DongleId", "Device", "Dongle ID"),
    info("HardwareSerial", "Device", "Serial"),
    info("GithubUsername", "Device", "GitHub user (SSH keys)"),
];

fn spec(key: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.key == key)
}

/// Is `(key, value)` an allowed, valid write? Allowlisted key, editable kind, and
/// a value valid for that kind. The single guard for every write path.
pub fn is_writable(key: &str, value: &str) -> bool {
    match spec(key) {
        Some(s) => match s.kind {
            Kind::Bool => value == "0" || value == "1",
            Kind::Int => value
                .parse::<i64>()
                .map(|v| v >= s.min && v <= s.max)
                .unwrap_or(false),
            Kind::Enum => s.options.iter().any(|(v, _)| *v == value),
            Kind::Info => false,
        },
        None => false,
    }
}

/// Read every allowlisted param in one round trip → `(key, value)` pairs (value
/// empty when the param isn't set on the device).
pub async fn read_all(state: &AppState, addr: &str) -> AppResult<Vec<(String, String)>> {
    let keys = SPECS.iter().map(|s| s.key).collect::<Vec<_>>().join(" ");
    // Keys are static, allowlisted identifiers — safe to interpolate.
    let cmd = format!(
        "for k in {keys}; do printf '%s\\t' \"$k\"; cat /data/params/d/\"$k\" 2>/dev/null; echo; done"
    );
    let out = device_ssh::run(state, addr, &cmd).await?;
    Ok(out
        .lines()
        .filter_map(|line| {
            let (k, v) = line.split_once('\t')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect())
}

/// Validate against the allowlist + kind, then atomically write the param.
pub async fn write(state: &AppState, addr: &str, key: &str, value: &str) -> AppResult<()> {
    if !is_writable(key, value) {
        return Err(AppError::BadRequest("not an editable setting, or invalid value".into()));
    }
    // value is a small int / known enum token (no shell metacharacters); key is
    // allowlisted. Write atomically the way openpilot's Params.put does.
    let cmd = format!(
        "T=$(mktemp /data/params/.tmp_value_XXXXXX) && printf '%s' '{value}' > \"$T\" && \
         flock /data/params/.lock mv \"$T\" /data/params/d/{key} && chmod 600 /data/params/d/{key}"
    );
    device_ssh::run(state, addr, &cmd).await?;
    tracing::info!(key, value, "device param set");
    Ok(())
}

// ── Local cache (desired/last-known values) ──────────────────────────────────
//
// The Device page reads from this cache (instant, works offline). Edits write the
// cache and mark the row `pending`; pending rows are flushed to the device over
// SSH whenever it's online (on connect, or right after an edit if connected). The
// device's actual values are read back into the cache on connect (`refresh`),
// without clobbering pending edits.

/// All cached `(key, value, pending)` for a dongle.
pub async fn cache_all(state: &AppState, dongle: &str) -> Vec<(String, String, bool)> {
    sqlx::query_as::<_, (String, String, i64)>(
        "SELECT key, value, pending FROM device_params WHERE dongle_id = ?",
    )
    .bind(dongle)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(k, v, p)| (k, v, p != 0))
    .collect()
}

async fn cache_count(state: &AppState, dongle: &str) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM device_params WHERE dongle_id = ?")
        .bind(dongle)
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0)
}

/// Set a cached value, marking it `pending` (a desired edit to flush).
pub async fn cache_set(state: &AppState, dongle: &str, key: &str, value: &str) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO device_params (dongle_id, key, value, pending, updated_at) \
         VALUES (?, ?, ?, 1, ?) \
         ON CONFLICT(dongle_id, key) DO UPDATE SET value = excluded.value, pending = 1, \
            updated_at = excluded.updated_at",
    )
    .bind(dongle)
    .bind(key)
    .bind(value)
    .bind(now_millis())
    .execute(&state.pool)
    .await?;
    Ok(())
}

/// Write pending edits to the device, clearing `pending` on success. Returns how
/// many were written. A failed write keeps its pending flag for the next attempt.
pub async fn flush(state: &AppState, dongle: &str, addr: &str) -> AppResult<usize> {
    let pend: Vec<(String, String)> =
        sqlx::query_as("SELECT key, value FROM device_params WHERE dongle_id = ? AND pending = 1")
            .bind(dongle)
            .fetch_all(&state.pool)
            .await?;
    let mut n = 0;
    for (k, v) in pend {
        if !is_writable(&k, &v) {
            // Stale/invalid — drop the pending flag so it doesn't get stuck.
            let _ = clear_pending(state, dongle, &k).await;
            continue;
        }
        match write(state, addr, &k, &v).await {
            Ok(()) => {
                clear_pending(state, dongle, &k).await?;
                n += 1;
            }
            Err(e) => tracing::warn!(dongle, key = %k, "device param flush: {e}"),
        }
    }
    Ok(n)
}

async fn clear_pending(state: &AppState, dongle: &str, key: &str) -> AppResult<()> {
    sqlx::query("UPDATE device_params SET pending = 0 WHERE dongle_id = ? AND key = ?")
        .bind(dongle)
        .bind(key)
        .execute(&state.pool)
        .await?;
    Ok(())
}

/// Read the device's actual values into the cache, leaving pending edits intact.
pub async fn refresh(state: &AppState, dongle: &str, addr: &str) -> AppResult<()> {
    let actual = read_all(state, addr).await?;
    for (k, v) in actual {
        sqlx::query(
            "INSERT INTO device_params (dongle_id, key, value, pending, updated_at) \
             VALUES (?, ?, ?, 0, ?) \
             ON CONFLICT(dongle_id, key) DO UPDATE SET value = excluded.value, \
                updated_at = excluded.updated_at WHERE device_params.pending = 0",
        )
        .bind(dongle)
        .bind(&k)
        .bind(&v)
        .bind(now_millis())
        .execute(&state.pool)
        .await?;
    }
    Ok(())
}

/// Flush pending edits, then refresh actuals — the cache↔device reconcile.
pub async fn reconcile(state: &AppState, dongle: &str, addr: &str) {
    if addr.is_empty() {
        return;
    }
    if let Ok(n) = flush(state, dongle, addr).await {
        if n > 0 {
            tracing::info!(dongle, "device params: flushed {n} pending");
        }
    }
    let _ = refresh(state, dongle, addr).await;
}

/// Reconcile on device connect (called from the athena handler).
pub async fn on_connect(state: &AppState, dongle: &str) {
    if let Ok(Some(d)) = crate::access::load_device(state, dongle).await {
        reconcile(state, dongle, &d.last_addr).await;
    }
}

/// True if the cache for this dongle is empty (never populated).
pub async fn cache_empty(state: &AppState, dongle: &str) -> bool {
    cache_count(state, dongle).await == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_allowlisted_safe_writes() {
        // bools accept just 0/1
        assert!(is_writable("RecordAudio", "1"));
        assert!(is_writable("RecordAudio", "0"));
        assert!(!is_writable("RecordAudio", "2"));
        assert!(!is_writable("RecordAudio", "x; rm -rf /"));
        // enum accepts only its options (incl. negatives)
        assert!(is_writable("LongitudinalPersonality", "2"));
        assert!(!is_writable("LongitudinalPersonality", "3"));
        assert!(is_writable("AutoLaneChangeTimer", "-1"));
        assert!(!is_writable("AutoLaneChangeTimer", "6"));
        // int accepts in-range integers only
        assert!(is_writable("SpeedLimitValueOffset", "-30"));
        assert!(is_writable("SpeedLimitValueOffset", "0"));
        assert!(!is_writable("SpeedLimitValueOffset", "31"));
        assert!(!is_writable("SpeedLimitValueOffset", "1.5"));
        // info is read-only; unknown keys rejected
        assert!(!is_writable("DongleId", "anything"));
        assert!(!is_writable("CalibrationParams", "1"));
        assert!(!is_writable("AthenaToken", "1"));
    }
}
