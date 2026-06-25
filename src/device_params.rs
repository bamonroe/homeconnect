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
    pub kind: Kind,
    pub help: &'static str,
    /// (value, label) choices for `Enum`.
    pub options: &'static [(&'static str, &'static str)],
}

/// The allowlist. Deliberately conservative: reversible, user-facing toggles only
/// — nothing touching identity, calibration, credentials, or safety internals.
pub const SPECS: &[Spec] = &[
    Spec { key: "OpenpilotEnabledToggle", label: "openpilot enabled", kind: Kind::Bool,
        help: "Master switch for openpilot engagement.", options: &[] },
    Spec { key: "ExperimentalMode", label: "Experimental mode", kind: Kind::Bool,
        help: "End-to-end longitudinal control.", options: &[] },
    Spec { key: "DisengageOnAccelerator", label: "Disengage on gas", kind: Kind::Bool,
        help: "Pressing the accelerator disengages openpilot.", options: &[] },
    Spec { key: "IsLdwEnabled", label: "Lane-departure warnings", kind: Kind::Bool,
        help: "Warn on lane drift when not engaged.", options: &[] },
    Spec { key: "RecordFront", label: "Record driver camera", kind: Kind::Bool,
        help: "Record the driver-facing camera with drives.", options: &[] },
    Spec { key: "RecordAudio", label: "Record microphone", kind: Kind::Bool,
        help: "Record cabin audio with drives.", options: &[] },
    Spec { key: "LongitudinalPersonality", label: "Following distance", kind: Kind::Enum,
        help: "Gap kept from the lead car.",
        options: &[("0", "Relaxed"), ("1", "Standard"), ("2", "Aggressive")] },
    // Read-only info.
    Spec { key: "DongleId", label: "Dongle ID", kind: Kind::Info, help: "", options: &[] },
    Spec { key: "GitBranch", label: "Software branch", kind: Kind::Info, help: "", options: &[] },
    Spec { key: "GithubUsername", label: "GitHub user (SSH keys)", kind: Kind::Info, help: "", options: &[] },
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
