//! openpilot software updates, driven over the same SSH channel as
//! `device_params`. This mirrors exactly what the on-device Settings → Software
//! screen does (`selfdrive/ui/layouts/settings/software.py`), no more:
//!
//! - **check**  → `SIGUSR1` to `system.updated.updated` (fetch the branch list /
//!   see whether a newer commit exists, without downloading)
//! - **download** → `SIGHUP` (fetch + finalize into the staging overlay)
//! - **install** → set `DoReboot`; the staged update is swapped in on boot
//! - **branch** → write `UpdaterTargetBranch`, then `SIGUSR1`
//!
//! Everything mutating is gated on the car being **off** (`IsOffroad = 1`), which
//! is the same rule the device UI enforces — openpilot only downloads offroad,
//! and a reboot mid-drive is obviously unacceptable. The gate is re-read live
//! from the device on every call, not cached, so a car that woke up in between
//! blocks the action.
//!
//! `updated` itself exits at startup when the `DisableUpdates` param is set, so
//! the status reports whether the daemon is actually running; turning updates
//! back on needs a reboot before check/download do anything.

use serde_json::{json, Value};

use crate::device_ssh;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Unlikely-to-collide delimiter between param dumps.
const SEP: &str = "__HCupdsep__";

/// Params read for the status view. (`/data/params/d` is a symlink to `d_tmp` —
/// one directory holds both the persistent and the clear-on-start keys.)
const STATUS_KEYS: &[&str] = &[
    "IsOffroad",
    "IsOnroad",
    "DisableUpdates",
    "UpdaterState",
    "UpdaterFetchAvailable",
    "UpdateAvailable",
    "UpdaterCurrentDescription",
    "UpdaterNewDescription",
    "UpdaterTargetBranch",
    "UpdaterAvailableBranches",
    "UpdateFailedCount",
    "LastUpdateException",
    "LastUpdateTime",
    "GitBranch",
    "GitCommitDate",
];

/// The `pgrep`/`pkill -f` pattern for the updater daemon. The device UI matches
/// `system.updated.updated`; we bracket the first letter (`[s]ystem…`) because
/// our own SSH shell's command line *contains* that pattern — an unbracketed
/// `pkill -f` would match and signal the shell running it.
const UPDATED_PROC: &str = "[s]ystem.updated.updated";

/// Shell to dump one param (empty when unset). Release notes can be long, so cap it.
fn read_snippet(key: &str) -> String {
    format!("cat /data/params/d/{key} 2>/dev/null | head -c 4000")
}

/// Live update status: what version the device is on, whether an update is
/// available or staged, and whether it's safe (car off) to act.
pub async fn status(state: &AppState, addr: &str) -> AppResult<Value> {
    let dumps = STATUS_KEYS
        .iter()
        .map(|k| format!("{}; printf '\\n{SEP}\\n'", read_snippet(k)))
        .collect::<Vec<_>>()
        .join("; ");
    // Keys are static identifiers — safe to interpolate.
    let cmd = format!("{dumps}; pgrep -f '{UPDATED_PROC}' >/dev/null && echo running || echo stopped");
    let out = device_ssh::run(state, addr, &cmd).await?;
    Ok(parse_status(&out))
}

/// Parse the delimited param dump produced by `status`.
fn parse_status(out: &str) -> Value {
    let sep = format!("\n{SEP}\n");
    let mut parts = out.split(sep.as_str()).map(str::trim);
    let mut vals = serde_json::Map::new();
    for k in STATUS_KEYS {
        vals.insert(k.to_string(), Value::String(parts.next().unwrap_or("").to_string()));
    }
    let running = parts.next().unwrap_or("").contains("running");

    let get = |k: &str| vals.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let flag = |k: &str| get(k) == "1";
    let branches: Vec<String> = get("UpdaterAvailableBranches")
        .split(',')
        .filter(|b| !b.is_empty())
        .map(str::to_string)
        .collect();

    json!({
        // Safety gate: the car must be off. `IsOffroad` is authoritative; treat a
        // missing param (device booting, params not written yet) as NOT offroad.
        "offroad": flag("IsOffroad"),
        "updates_disabled": flag("DisableUpdates"),
        "updater_running": running,
        "state": get("UpdaterState"),
        "fetch_available": flag("UpdaterFetchAvailable"),
        "update_ready": flag("UpdateAvailable"),
        "current": get("UpdaterCurrentDescription"),
        "new": get("UpdaterNewDescription"),
        "branch": get("GitBranch"),
        "target_branch": get("UpdaterTargetBranch"),
        "branches": branches,
        "commit_date": get("GitCommitDate"),
        "failed_count": get("UpdateFailedCount").parse::<i64>().unwrap_or(0),
        "last_error": get("LastUpdateException"),
        "last_checked": get("LastUpdateTime"),
    })
}

/// Re-read the car's state and refuse if it isn't off. Never trust a cached
/// value here — the car may have been started since the page was loaded.
async fn require_offroad(state: &AppState, addr: &str) -> AppResult<()> {
    let out = device_ssh::run(state, addr, &read_snippet("IsOffroad")).await?;
    if out.trim() == "1" {
        return Ok(());
    }
    Err(AppError::BadRequest(
        "the car must be off before changing software on the device".into(),
    ))
}

/// Ask the updater to check for a new commit (no download). `SIGUSR1`.
pub async fn check(state: &AppState, addr: &str) -> AppResult<()> {
    require_offroad(state, addr).await?;
    device_ssh::run(state, addr, &format!("pkill -SIGUSR1 -f '{UPDATED_PROC}'; true")).await?;
    tracing::info!("device update: check requested");
    Ok(())
}

/// Fetch + finalize an update into the staging overlay. `SIGHUP`. Nothing is
/// swapped in until the device reboots.
pub async fn download(state: &AppState, addr: &str) -> AppResult<()> {
    require_offroad(state, addr).await?;
    device_ssh::run(state, addr, &format!("pkill -SIGHUP -f '{UPDATED_PROC}'; true")).await?;
    tracing::info!("device update: download requested");
    Ok(())
}

/// Install a staged update by rebooting the device. Only meaningful once
/// `update_ready` — openpilot swaps the finalized overlay in on boot.
pub async fn install(state: &AppState, addr: &str) -> AppResult<()> {
    require_offroad(state, addr).await?;
    let ready = device_ssh::run(state, addr, &read_snippet("UpdateAvailable")).await?;
    if ready.trim() != "1" {
        return Err(AppError::BadRequest("no downloaded update is staged to install".into()));
    }
    write_param(state, addr, "DoReboot", "1").await?;
    tracing::info!("device update: install (reboot) requested");
    Ok(())
}

/// Point the updater at a different branch. Only branches the device itself
/// listed in `UpdaterAvailableBranches` are accepted, so nothing user-supplied
/// reaches the shell unvalidated.
pub async fn set_branch(state: &AppState, addr: &str, branch: &str) -> AppResult<()> {
    require_offroad(state, addr).await?;
    let listed = device_ssh::run(state, addr, &read_snippet("UpdaterAvailableBranches")).await?;
    if !listed.trim().split(',').any(|b| b == branch) {
        return Err(AppError::BadRequest("unknown branch for this device".into()));
    }
    write_param(state, addr, "UpdaterTargetBranch", branch).await?;
    device_ssh::run(state, addr, &format!("pkill -SIGUSR1 -f '{UPDATED_PROC}'; true")).await?;
    tracing::info!(branch, "device update: target branch set");
    Ok(())
}

/// Turn openpilot's updater on or off (`DisableUpdates`). The daemon only reads
/// this at startup, so the change takes effect on the next reboot.
pub async fn set_updates_enabled(state: &AppState, addr: &str, enabled: bool) -> AppResult<()> {
    require_offroad(state, addr).await?;
    write_param(state, addr, "DisableUpdates", if enabled { "0" } else { "1" }).await?;
    tracing::info!(enabled, "device update: automatic updates toggled");
    Ok(())
}

/// Atomic param write, the way openpilot's `Params.put` does it. `key` is one of
/// this module's literals; `value` is validated by the caller (a listed branch
/// name or a single digit).
async fn write_param(state: &AppState, addr: &str, key: &str, value: &str) -> AppResult<()> {
    if !value.chars().all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c)) {
        return Err(AppError::BadRequest("invalid parameter value".into()));
    }
    let cmd = format!(
        "T=$(mktemp /data/params/.tmp_value_XXXXXX) && printf '%s' '{value}' > \"$T\" && \
         flock /data/params/.lock mv \"$T\" /data/params/d/{key} && chmod 600 /data/params/d/{key}"
    );
    device_ssh::run(state, addr, &cmd).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the dump the device's shell would emit for the given values.
    fn dump(vals: &[&str], proc: &str) -> String {
        let mut s = String::new();
        for v in vals {
            s.push_str(v);
            s.push_str(&format!("\n{SEP}\n"));
        }
        s.push_str(proc);
        s
    }

    #[test]
    fn parses_an_offroad_device_with_a_staged_update() {
        // Order matches STATUS_KEYS.
        let out = dump(
            &[
                "1", "0", "0", "idle", "1", "1", "release-mici 0.10.0", "release-mici 0.10.1",
                "release-mici", "release-mici,master,dev", "0", "", "2026-06-25T14:46:03", "release-mici",
                "2026-05-27 18:18:19 -0400",
            ],
            "running",
        );
        let s = parse_status(&out);
        assert_eq!(s["offroad"], true);
        assert_eq!(s["updates_disabled"], false);
        assert_eq!(s["updater_running"], true);
        assert_eq!(s["update_ready"], true);
        assert_eq!(s["current"], "release-mici 0.10.0");
        assert_eq!(s["new"], "release-mici 0.10.1");
        assert_eq!(s["branches"], json!(["release-mici", "master", "dev"]));
        assert_eq!(s["failed_count"], 0);
    }

    /// A car that's on, with the updater disabled and never run: every flag that
    /// gates an action must read false, and missing params must not panic.
    #[test]
    fn missing_params_are_not_offroad_and_not_ready() {
        let s = parse_status(&dump(&["", "1", "1"], "stopped"));
        assert_eq!(s["offroad"], false);
        assert_eq!(s["updates_disabled"], true);
        assert_eq!(s["updater_running"], false);
        assert_eq!(s["update_ready"], false);
        assert_eq!(s["fetch_available"], false);
        assert_eq!(s["branches"], json!([]));
        assert_eq!(s["failed_count"], 0);
    }
}
