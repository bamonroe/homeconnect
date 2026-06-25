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
    b("OpenpilotEnabledToggle", "Driving", "openpilot enabled", "Master switch for openpilot engagement."),
    b("ExperimentalMode", "Driving", "Experimental mode", "End-to-end longitudinal (let the model brake/accelerate)."),
    b("AlphaLongitudinalEnabled", "Driving", "openpilot longitudinal", "openpilot controls gas + brake (off = stock ACC)."),
    b("DynamicExperimentalControl", "Driving", "Dynamic experimental control", "Auto-switch between experimental and chill."),
    b("DisengageOnAccelerator", "Driving", "Disengage on gas", "Pressing the accelerator disengages openpilot."),
    e("LongitudinalPersonality", "Driving", "Following distance", "Gap kept from the lead car.",
        &[("0", "Relaxed"), ("1", "Standard"), ("2", "Aggressive")]),
    e("HyundaiLongitudinalTuning", "Driving", "Hyundai longitudinal tuning", "Custom longitudinal tuning for Hyundai/Kia/Genesis.",
        &[("0", "Off"), ("1", "Dynamic"), ("2", "Predictive")]),

    // ── Cruise ───────────────────────────────────────────────────────────────
    b("IntelligentCruiseButtonManagement", "Cruise", "Intelligent cruise buttons (alpha)", "Auto-manage the cruise set-speed buttons."),
    b("SmartCruiseControlVision", "Cruise", "Vision curve slowing", "Slow for curves the camera sees."),
    b("SmartCruiseControlMap", "Cruise", "Map curve/speed slowing", "Use offline map data to slow for curves/limits."),
    b("CustomAccIncrementsEnabled", "Cruise", "Custom ACC speed steps", "Use custom set-speed increments."),
    dep(int_("CustomAccShortPressIncrement", "Cruise", "ACC short-press step", "Speed change per short button press.", 1, 10, 1, ""),
        "CustomAccIncrementsEnabled", &["1"]),

    // ── Speed limits ─────────────────────────────────────────────────────────
    e("SpeedLimitMode", "Speed limits", "Speed limit control", "How posted limits are used.",
        &[("0", "Off"), ("1", "Info"), ("2", "Warning"), ("3", "Assist")]),
    e("SpeedLimitPolicy", "Speed limits", "Speed limit source", "Where limit data comes from.",
        &[("0", "Car only"), ("1", "Map only"), ("2", "Car first"), ("3", "Map first"), ("4", "Combined")]),
    e("SpeedLimitOffsetType", "Speed limits", "Speed limit offset type", "How the offset is applied.",
        &[("0", "None"), ("1", "Fixed"), ("2", "Percent")]),
    dep(int_("SpeedLimitValueOffset", "Speed limits", "Speed limit offset", "Amount to add to the posted limit.", -30, 30, 1, ""),
        "SpeedLimitOffsetType", &["1", "2"]),

    // ── Steering (MADS) ──────────────────────────────────────────────────────
    b("Mads", "Steering (MADS)", "Enable MADS", "Modified Assistive Driving — steering independent of cruise."),
    dep(b("MadsMainCruiseAllowed", "Steering (MADS)", "Engage with main cruise", "Allow MADS to engage from the MAIN cruise button."),
        "Mads", &["1"]),
    dep(b("MadsUnifiedEngagementMode", "Steering (MADS)", "Unified engagement", "Engage lateral + longitudinal together."),
        "Mads", &["1"]),
    dep(e("MadsSteeringMode", "Steering (MADS)", "On brake pedal", "What steering does when you brake.",
        &[("0", "Remain active"), ("1", "Pause"), ("2", "Disengage")]),
        "Mads", &["1"]),
    dep(b("NeuralNetworkLateralControl", "Steering (MADS)", "Neural-net lateral control", "Use the NNLC steering model when available."),
        "EnforceTorqueControl", &["0"]),
    dep(b("EnforceTorqueControl", "Steering (MADS)", "Enforce torque control", "Force torque-based lateral control."),
        "NeuralNetworkLateralControl", &["0"]),
    b("BlinkerPauseLateralControl", "Steering (MADS)", "Pause steering on blinker", "Hand back steering while the turn signal is on."),
    dep(int_("BlinkerMinLateralControlSpeed", "Steering (MADS)", "Min speed to pause on blinker", "Below this speed, the blinker pauses steering.", 0, 255, 5, ""),
        "BlinkerPauseLateralControl", &["1"]),
    dep(int_("BlinkerLateralReengageDelay", "Steering (MADS)", "Post-blinker delay", "Wait this long after the blinker before re-steering.", 0, 10, 1, "s"),
        "BlinkerPauseLateralControl", &["1"]),
    e("AutoLaneChangeTimer", "Steering (MADS)", "Auto lane change", "Delay before an auto lane change (no steering nudge needed when set).",
        &[("-1", "Off"), ("0", "Nudge"), ("1", "Nudgeless"), ("2", "0.5 s"), ("3", "1 s"), ("4", "2 s"), ("5", "3 s")]),
    dep(b("AutoLaneChangeBsmDelay", "Steering (MADS)", "Blind-spot lane-change delay", "Wait on blind-spot monitor before auto lane change."),
        "AutoLaneChangeTimer", &["1", "2", "3", "4", "5"]),

    // ── Display & alerts ─────────────────────────────────────────────────────
    b("IsLdwEnabled", "Display & alerts", "Lane-departure warnings", "Warn on lane drift when not engaged."),
    b("BlindSpot", "Display & alerts", "Blind-spot warnings", "Show blind-spot warnings on screen."),
    b("GreenLightAlert", "Display & alerts", "Green-light alert", "Chime when a stopped light turns green."),
    b("LeadDepartAlert", "Display & alerts", "Lead-departure alert", "Chime when the lead car pulls away."),
    b("RoadNameToggle", "Display & alerts", "Show road name", "Display the current road name."),
    b("ShowTurnSignals", "Display & alerts", "Turn-signal icons", "Show blinker icons on screen."),
    b("TorqueBar", "Display & alerts", "Steering arc", "Show the steering-torque arc."),
    b("RocketFuel", "Display & alerts", "Acceleration bar", "Real-time acceleration bar."),
    b("StandstillTimer", "Display & alerts", "Standstill timer", "Show how long you've been stopped."),
    b("TrueVEgoUI", "Display & alerts", "Always show true speed", "Speedometer shows true speed."),
    b("HideVEgoUI", "Display & alerts", "Hide speedometer", "Hide the speed from the onroad screen."),
    b("RainbowMode", "Display & alerts", "Rainbow path", "Rainbow-colored driving path. For fun."),
    e("ChevronInfo", "Display & alerts", "Metrics below chevron", "Info shown under the lead-car marker.",
        &[("0", "Off"), ("1", "Distance"), ("2", "Speed"), ("3", "Time"), ("4", "All")]),
    e("DevUIInfo", "Display & alerts", "Developer UI", "Extra developer readouts on screen.",
        &[("0", "Off"), ("1", "Bottom"), ("2", "Right"), ("3", "Right & bottom")]),
    e("OnroadScreenOffBrightness", "Display & alerts", "Onroad brightness", "Screen brightness while driving.",
        &[("0", "Auto"), ("1", "Auto (dark)"), ("2", "Screen off"), ("7", "25%"), ("12", "50%"), ("17", "75%"), ("22", "100%")]),
    dep(e("OnroadScreenOffTimer", "Display & alerts", "Onroad brightness delay", "Dim the screen after this long onroad.",
        &[("3", "3 s"), ("5", "5 s"), ("7", "7 s"), ("10", "10 s"), ("15", "15 s"), ("30", "30 s"),
          ("60", "1 min"), ("120", "2 min"), ("180", "3 min"), ("240", "4 min"), ("300", "5 min"),
          ("360", "6 min"), ("420", "7 min"), ("480", "8 min"), ("540", "9 min"), ("600", "10 min")]),
        "OnroadScreenOffBrightness", &["2", "7", "12", "17", "22"]),
    int_("InteractivityTimeout", "Display & alerts", "Settings UI timeout", "Auto-close settings after inactivity (0 = default).",
        0, 120, 10, "s"),

    // ── Recording ────────────────────────────────────────────────────────────
    b("RecordFront", "Recording", "Record driver camera", "Record the driver-facing camera with drives."),
    b("RecordAudio", "Recording", "Record microphone", "Record cabin audio with drives."),
    b("RecordAudioFeedback", "Recording", "Record audio feedback", "Record a short clip when you give feedback."),

    // ── Device power ─────────────────────────────────────────────────────────
    e("DeviceBootMode", "Device power", "Wake-up behavior", "What the device does on wake.",
        &[("0", "Default"), ("1", "Offroad")]),
    b("OffroadMode", "Device power", "Always offroad", "Keep the device offroad (won't go onroad)."),
    e("MaxTimeOffroad", "Device power", "Max time offroad", "Power down this long after parking.",
        &[("0", "Always on"), ("5", "5 min"), ("10", "10 min"), ("15", "15 min"), ("30", "30 min"),
          ("60", "1 hour"), ("120", "2 hours"), ("180", "3 hours"), ("300", "5 hours"),
          ("600", "10 hours"), ("1440", "24 hours"), ("1800", "30 hours")]),
    b("QuietMode", "Device power", "Quiet mode", "Mute most non-critical chimes."),

    // ── Connectivity & updates ───────────────────────────────────────────────
    b("SshEnabled", "Connectivity & updates", "SSH enabled", "Allow SSH access to the device."),
    b("AdbEnabled", "Connectivity & updates", "ADB enabled", "Allow Android Debug Bridge access."),
    b("GsmMetered", "Connectivity & updates", "Cellular metered", "Treat the SIM connection as metered."),
    b("OnroadUploads", "Connectivity & updates", "Upload while driving", "Allow uploads during drives (not just parked)."),
    b("SunnylinkEnabled", "Connectivity & updates", "sunnylink enabled", "Connect to sunnylink."),
    dep(b("DisableUpdates", "Connectivity & updates", "Pause software updates", "Stop fetching/installing updates."),
        "ShowAdvancedControls", &["1"]),

    // ── Developer ────────────────────────────────────────────────────────────
    b("ShowAdvancedControls", "Developer", "Show advanced controls", "Reveal advanced settings on the device."),
    dep(b("QuickBootToggle", "Developer", "Quickboot mode", "Faster boot (skips some checks)."),
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
