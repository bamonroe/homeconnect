//! Device-scoped SSH: homeconnect holds its own ed25519 keypair (NOT a GitHub
//! key). The public key is installed on the device during onboarding (into
//! `/tmp/authorized_keys` via `continue.sh`, restricted with `from=` to the
//! tailnet/LAN), so a compromise of this server only exposes SSH to the paired
//! comma(s) — not everything that trusts a GitHub account.
//!
//! Commands run as `comma@<addr>`, where `<addr>` is the device's tailnet IP
//! captured from the athena websocket connection.

use std::path::PathBuf;
use std::time::Duration;

use tokio::process::Command;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

const SSH_USER: &str = "comma";
const CMD_TIMEOUT: Duration = Duration::from_secs(20);
/// Pulls can move tens of MB (full-res camera), so allow much longer than `run`.
const PULL_TIMEOUT: Duration = Duration::from_secs(180);

/// Shared ssh/scp hardening flags (no host-key prompt, no agent, key-only).
const SSH_OPTS: &[&str] = &[
    "-o", "IdentitiesOnly=yes",
    "-o", "StrictHostKeyChecking=accept-new",
    "-o", "UserKnownHostsFile=/dev/null",
    "-o", "BatchMode=yes",
    "-o", "ConnectTimeout=10",
    "-o", "LogLevel=ERROR",
];

fn key_path(state: &AppState) -> PathBuf {
    state.config.ssh_dir().join("id_ed25519")
}

/// Ensure the keypair exists (generate with ssh-keygen on first use), returning
/// the public key text (single line).
pub async fn ensure_keypair(state: &AppState) -> AppResult<String> {
    let dir = state.config.ssh_dir();
    tokio::fs::create_dir_all(&dir).await.map_err(|e| AppError::Other(e.into()))?;
    let key = key_path(state);
    let pubk = key.with_extension("pub");
    if !tokio::fs::try_exists(&pubk).await.unwrap_or(false) {
        let status = Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-N", "", "-C", "homeconnect", "-f"])
            .arg(&key)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("ssh-keygen: {e}")))?;
        if !status.success() {
            return Err(AppError::Other(anyhow::anyhow!("ssh-keygen failed")));
        }
    }
    let pub_text = tokio::fs::read_to_string(&pubk)
        .await
        .map_err(|e| AppError::Other(e.into()))?;
    Ok(pub_text.trim().to_string())
}

/// homeconnect's public key (ensuring it exists).
pub async fn public_key(state: &AppState) -> AppResult<String> {
    ensure_keypair(state).await
}

/// Run a command on the device, returning stdout. Errors on connect failure,
/// timeout, or non-zero exit.
pub async fn run(state: &AppState, addr: &str, command: &str) -> AppResult<String> {
    if addr.is_empty() {
        return Err(AppError::BadRequest("device address unknown (not seen online yet)".into()));
    }
    ensure_keypair(state).await?;
    let key = key_path(state);
    let target = format!("{SSH_USER}@{addr}");
    let mut cmd = Command::new("ssh");
    cmd.arg("-i").arg(&key).args(SSH_OPTS).arg(&target).arg(command);
    let out = tokio::time::timeout(CMD_TIMEOUT, cmd.stdin(std::process::Stdio::null()).output())
        .await
        .map_err(|_| AppError::Other(anyhow::anyhow!("ssh timed out")))?
        .map_err(|e| AppError::Other(anyhow::anyhow!("ssh spawn: {e}")))?;

    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(AppError::Other(anyhow::anyhow!(
            "ssh to {addr} failed: {}",
            err.trim()
        )))
    }
}

/// Copy a remote file off the device to a local path via `scp`. Binary-safe and
/// allowed a long timeout (full-res camera segments are tens of MB). The caller
/// supplies an absolute `remote` path; `local` is created/overwritten.
pub async fn pull_file(
    state: &AppState,
    addr: &str,
    remote: &str,
    local: &std::path::Path,
) -> AppResult<()> {
    if addr.is_empty() {
        return Err(AppError::BadRequest("device address unknown (not seen online yet)".into()));
    }
    ensure_keypair(state).await?;
    let key = key_path(state);
    // scp treats `:` as a host separator, so the remote path must not contain one
    // (realdata paths don't). Pass it as `user@addr:/abs/path`.
    let source = format!("{SSH_USER}@{addr}:{remote}");

    let mut cmd = Command::new("scp");
    cmd.arg("-i").arg(&key).args(SSH_OPTS).arg(&source).arg(local);
    let out = tokio::time::timeout(PULL_TIMEOUT, cmd.stdin(std::process::Stdio::null()).output())
        .await
        .map_err(|_| AppError::Other(anyhow::anyhow!("scp timed out")))?
        .map_err(|e| AppError::Other(anyhow::anyhow!("scp spawn: {e}")))?;

    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(AppError::Other(anyhow::anyhow!(
            "scp {remote} from {addr} failed: {}",
            err.trim()
        )))
    }
}
