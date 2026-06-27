//! Serves a device-onboarding shell script with this server's URL + SSH public
//! key baked in. Run it on the comma (directly or over SSH):
//!   curl -fsSL http://hc.bam/onboard.sh | bash
//! It repoints openpilot at homeconnect, installs homeconnect's device-scoped
//! SSH key (for device management — NOT a GitHub key), and forces
//! re-registration; the user then claims the device in the UI.

use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;

use crate::device_ssh;
use crate::state::AppState;

const SCRIPT: &str = r#"#!/usr/bin/env bash
# homeconnect device onboarding. Run ON the comma, or over SSH:
#   curl -fsSL __HC_HOST__/onboard.sh | bash
# Append `-s -- --reboot` to reboot automatically when done.
set -euo pipefail

HOST="__HC_HOST__"
WS="${HOST/http/ws}"
CONT=/data/continue.sh
KEYFILE=/data/homeconnect_authorized_key
# homeconnect's device-scoped key, restricted to the tailnet/LAN (no GitHub).
HC_KEY='from="100.64.0.0/10,192.168.0.0/16,10.0.0.0/8,172.16.0.0/12" __HC_PUBKEY__'

echo "homeconnect onboarding -> $HOST"

# 1) Point openpilot/sunnypilot at homeconnect, and re-apply our SSH key each
#    boot. continue.sh lives OUTSIDE /data/openpilot, so this survives updates.
if [ ! -f "$CONT" ]; then
  printf '#!/usr/bin/env bash\nexec /data/openpilot/launch_openpilot.sh\n' > "$CONT"
fi
cp -n "$CONT" "$CONT.pre-homeconnect" 2>/dev/null || true
# Drop any prior homeconnect-managed lines, then (re)insert before the launcher.
sed -i '/export API_HOST=/d; /export ATHENA_HOST=/d; /export MAPS_HOST=/d; /homeconnect-ssh/d' "$CONT"
if grep -qE '^[[:space:]]*exec ' "$CONT"; then
  awk -v h="$HOST" -v w="$WS" '
    /^[[:space:]]*exec /&&!p { print "export API_HOST=\""h"\"";
                               print "export ATHENA_HOST=\""w"\"";
                               print "export MAPS_HOST=\""h"\"";
                               print "cat /data/homeconnect_authorized_key >> /tmp/authorized_keys 2>/dev/null; chmod 600 /tmp/authorized_keys 2>/dev/null # homeconnect-ssh";
                               p=1 }
    { print }' "$CONT" > "$CONT.hc" && mv "$CONT.hc" "$CONT"
else
  { echo "export API_HOST=\"$HOST\""; echo "export ATHENA_HOST=\"$WS\""; echo "export MAPS_HOST=\"$HOST\"";
    echo "cat /data/homeconnect_authorized_key >> /tmp/authorized_keys 2>/dev/null; chmod 600 /tmp/authorized_keys 2>/dev/null # homeconnect-ssh"; } >> "$CONT"
fi
chmod +x "$CONT"
echo "OK: patched $CONT (backup: $CONT.pre-homeconnect)"

# 2) Install homeconnect's SSH key now (so it works before the next reboot too).
echo "$HC_KEY" > "$KEYFILE"
touch /tmp/authorized_keys
grep -qF "$HC_KEY" /tmp/authorized_keys || cat "$KEYFILE" >> /tmp/authorized_keys
chmod 600 /tmp/authorized_keys
echo "OK: installed homeconnect device SSH key (tailnet/LAN only, no GitHub)"

# 3) Force re-registration so homeconnect learns this device's key.
rm -f /data/params/d/DongleId
echo "OK: cleared cached DongleId (device re-registers on next boot)"

# 4) Apply.
if [ "${1:-}" = "--reboot" ]; then
  echo "rebooting..."
  sudo reboot
else
  echo
  echo "Done. Reboot to apply:   sudo reboot"
  echo "After it boots: open $HOST, log in, and Claim the device under '+ Add device'."
fi
"#;

/// GET /onboard.sh — public; returns the setup script with host + SSH pubkey.
pub async fn onboard_script(State(state): State<AppState>) -> impl IntoResponse {
    let pubkey = device_ssh::public_key(&state).await.unwrap_or_default();
    let body = SCRIPT
        .replace("__HC_HOST__", &state.config.public_url)
        .replace("__HC_PUBKEY__", pubkey.trim());
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        body,
    )
}
