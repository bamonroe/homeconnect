//! sunnypilot driving-model selection over SSH.
//!
//! Unlike the curated param allowlist (`device_params`), the model list is
//! dynamic: sunnypilot's ModelManager caches the catalog in
//! `ModelManager_ModelsCache` and the active choice in `ModelManager_ActiveBundle`
//! (note: the cache uses snake_case keys, the active bundle camelCase). Selecting
//! a model = write the bundle's integer index to `ModelManager_DownloadIndex`; the
//! on-device manager then downloads it, activates it (sets ActiveBundle), and
//! clears the index. Removing ActiveBundle reverts to the built-in default model.
//!
//! Caveats surfaced to the UI: switching downloads in the background (needs the
//! device online + connectivity), takes effect after a reboot, and a
//! cross-generation switch warrants a calibration reset.

use serde_json::{json, Value};

use crate::device_ssh;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Unlikely-to-collide delimiter between the three param dumps.
const SEP: &str = "__HCmodelsep__";

/// Read the active model + the selectable catalog + any in-flight selection.
pub async fn get_models(state: &AppState, addr: &str) -> AppResult<Value> {
    let cmd = format!(
        "for k in ModelManager_ActiveBundle ModelManager_ModelsCache ModelManager_DownloadIndex; \
         do cat /data/params/d/$k 2>/dev/null; printf '\\n{SEP}\\n'; done"
    );
    let out = device_ssh::run(state, addr, &cmd).await?;
    let parts: Vec<&str> = out.split(&format!("\n{SEP}\n")).collect();
    let active = parts.first().and_then(|s| serde_json::from_str::<Value>(s.trim()).ok());
    let cache = parts.get(1).and_then(|s| serde_json::from_str::<Value>(s.trim()).ok());
    let downloading = parts
        .get(2)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<i64>().ok());

    // Current model (null ⇒ built-in default). ActiveBundle uses camelCase.
    let current = active.as_ref().filter(|v| v.is_object()).map(|v| {
        let name = v.get("displayName").or_else(|| v.get("display_name")).and_then(Value::as_str).unwrap_or("");
        let short = v.get("internalName").or_else(|| v.get("short_name")).and_then(Value::as_str).unwrap_or("");
        json!({ "index": v.get("index").and_then(Value::as_i64), "name": name, "short": short })
    });

    // Selectable catalog: bundles that have a display name (others are internal).
    let mut available: Vec<Value> = Vec::new();
    if let Some(bundles) = cache.as_ref().and_then(|c| c.get("bundles")).and_then(Value::as_array) {
        for b in bundles {
            let Some(name) = b.get("display_name").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
                continue;
            };
            let folder = b
                .get("overrides")
                .and_then(Value::as_array)
                .and_then(|arr| {
                    arr.iter()
                        .find(|ov| ov.get("key").and_then(Value::as_str) == Some("folder"))
                        .and_then(|ov| ov.get("value").and_then(Value::as_str))
                })
                .unwrap_or("");
            available.push(json!({
                "index": b.get("index").and_then(Value::as_i64),
                "ref": b.get("ref").and_then(Value::as_str).unwrap_or(""),
                "name": name,
                "short": b.get("short_name").and_then(Value::as_str).unwrap_or(""),
                "folder": folder,
                "gen": b.get("generation").and_then(Value::as_i64),
            }));
        }
    }
    available.sort_by(|a, b| b["index"].as_i64().unwrap_or(0).cmp(&a["index"].as_i64().unwrap_or(0)));

    Ok(json!({ "current": current, "available": available, "downloading": downloading }))
}

/// Queue a model switch: the on-device manager downloads + activates this bundle.
/// `index` is a catalog index (a plain integer — no shell metacharacters).
pub async fn select(state: &AppState, addr: &str, index: i64) -> AppResult<()> {
    if index < 0 {
        return Err(AppError::BadRequest("bad model index".into()));
    }
    let cmd = format!(
        "T=$(mktemp /data/params/.tmp_value_XXXXXX) && printf '%s' '{index}' > \"$T\" && \
         flock /data/params/.lock mv \"$T\" /data/params/d/ModelManager_DownloadIndex && \
         chmod 600 /data/params/d/ModelManager_DownloadIndex"
    );
    device_ssh::run(state, addr, &cmd).await?;
    tracing::info!(index, "device model: switch queued");
    Ok(())
}

/// Revert to the built-in default model (remove the active bundle).
pub async fn use_default(state: &AppState, addr: &str) -> AppResult<()> {
    device_ssh::run(state, addr, "flock /data/params/.lock rm -f /data/params/d/ModelManager_ActiveBundle")
        .await?;
    tracing::info!("device model: reset to default");
    Ok(())
}
