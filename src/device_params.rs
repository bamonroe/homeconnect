//! Curated read/write of device (openpilot) params over SSH — the basis for
//! replacing sunnylink for a few device toggles. Athena has no `setParam`, so SSH
//! is the lever.
//!
//! Params are 0600 files under `/data/params/d`; we write them the same atomic
//! way openpilot does (a temp file in `/data/params` + an flock'd rename), so a
//! concurrent openpilot write can't tear. **Only allowlisted keys are writable**
//! and values are validated per kind, so the UI can't brick the device. Reads are
//! likewise limited to the allowlist (no dumping arbitrary params / secrets).

use crate::device_ssh;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Bool,
    Enum,
    Info, // read-only (informational)
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Bool => "bool",
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
}

// Concise constructors keep the (large) allowlist readable.
const fn b(key: &'static str, group: &'static str, label: &'static str, help: &'static str) -> Spec {
    Spec { key, group, label, kind: Kind::Bool, help, options: &[] }
}
const fn info(key: &'static str, group: &'static str, label: &'static str) -> Spec {
    Spec { key, group, label, kind: Kind::Info, help: "", options: &[] }
}

/// The allowlist. Deliberately conservative: reversible, user-facing settings
/// (sunnypilot/openpilot toggles) — nothing touching identity, calibration,
/// credentials, tuning blobs, or safety internals. Booleans validate to 0/1; the
/// one enum has a known mapping. Grouped for display in source order.
pub const SPECS: &[Spec] = &[
    // ── Driving ──────────────────────────────────────────────────────────────
    b("OpenpilotEnabledToggle", "Driving", "openpilot enabled", "Master switch for openpilot engagement."),
    b("ExperimentalMode", "Driving", "Experimental mode", "End-to-end longitudinal (let the model brake/accelerate)."),
    b("AlphaLongitudinalEnabled", "Driving", "openpilot longitudinal", "openpilot controls gas + brake (off = stock ACC)."),
    b("DynamicExperimentalControl", "Driving", "Dynamic experimental control", "Auto-switch between experimental and chill."),
    b("DisengageOnAccelerator", "Driving", "Disengage on gas", "Pressing the accelerator disengages openpilot."),
    Spec { key: "LongitudinalPersonality", group: "Driving", label: "Following distance", kind: Kind::Enum,
        help: "Gap kept from the lead car.",
        options: &[("0", "Relaxed"), ("1", "Standard"), ("2", "Aggressive")] },

    // ── Steering (MADS) ──────────────────────────────────────────────────────
    b("Mads", "Steering (MADS)", "Enable MADS", "Modified Assistive Driving — steering independent of cruise."),
    b("MadsMainCruiseAllowed", "Steering (MADS)", "Engage with main cruise", "Allow MADS to engage from the MAIN cruise button."),
    b("MadsUnifiedEngagementMode", "Steering (MADS)", "Unified engagement", "Engage lateral + longitudinal together."),
    b("NeuralNetworkLateralControl", "Steering (MADS)", "Neural-net lateral control", "Use the NNLC steering model when available."),
    b("AutoLaneChangeBsmDelay", "Steering (MADS)", "Blind-spot lane-change delay", "Wait on blind-spot monitor before auto lane change."),
    b("BlinkerPauseLateralControl", "Steering (MADS)", "Pause steering on blinker", "Hand back steering while the turn signal is on."),

    // ── Speed & navigation ───────────────────────────────────────────────────
    b("SmartCruiseControlVision", "Speed & nav", "Vision curve slowing", "Slow for curves the camera sees."),
    b("SmartCruiseControlMap", "Speed & nav", "Map speed limits", "Use offline map speed limits."),
    b("RoadNameToggle", "Speed & nav", "Show road name", "Display the current road name."),

    // ── Display & alerts ─────────────────────────────────────────────────────
    b("IsLdwEnabled", "Display & alerts", "Lane-departure warnings", "Warn on lane drift when not engaged."),
    b("GreenLightAlert", "Display & alerts", "Green-light alert", "Chime when a stopped light turns green."),
    b("LeadDepartAlert", "Display & alerts", "Lead-departure alert", "Chime when the lead car pulls away."),
    b("ShowTurnSignals", "Display & alerts", "Turn-signal icons", "Show blinker icons on screen."),
    b("DevUIInfo", "Display & alerts", "Developer UI", "Show extra developer readouts."),
    b("TorqueBar", "Display & alerts", "Torque bar", "Show the steering-torque bar."),
    b("StandstillTimer", "Display & alerts", "Standstill timer", "Show how long you've been stopped."),
    b("QuietMode", "Display & alerts", "Quiet mode", "Mute most non-critical chimes."),
    b("RainbowMode", "Display & alerts", "Rainbow path", "Rainbow-colored driving path. For fun."),

    // ── Recording ────────────────────────────────────────────────────────────
    b("RecordFront", "Recording", "Record driver camera", "Record the driver-facing camera with drives."),
    b("RecordAudio", "Recording", "Record microphone", "Record cabin audio with drives."),
    b("RecordAudioFeedback", "Recording", "Record audio feedback", "Record a short clip when you give feedback."),

    // ── Connectivity & updates ───────────────────────────────────────────────
    b("SshEnabled", "Connectivity & updates", "SSH enabled", "Allow SSH access to the device."),
    b("AdbEnabled", "Connectivity & updates", "ADB enabled", "Allow Android Debug Bridge access."),
    b("GsmMetered", "Connectivity & updates", "Cellular metered", "Treat the SIM connection as metered."),
    b("OnroadUploads", "Connectivity & updates", "Upload while driving", "Allow uploads during drives (not just parked)."),
    b("SunnylinkEnabled", "Connectivity & updates", "sunnylink enabled", "Connect to sunnylink."),
    b("DisableUpdates", "Connectivity & updates", "Pause software updates", "Stop fetching/installing updates."),

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
    // value is "0"/"1" or a known enum token (no shell metacharacters); key is
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
        // enum accepts only its options
        assert!(is_writable("LongitudinalPersonality", "2"));
        assert!(!is_writable("LongitudinalPersonality", "3"));
        // info is read-only; unknown keys rejected
        assert!(!is_writable("DongleId", "anything"));
        assert!(!is_writable("CalibrationParams", "1"));
        assert!(!is_writable("AthenaToken", "1"));
    }
}
