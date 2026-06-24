//! Serves a device-onboarding shell script with this server's URL baked in.
//! Run it on the comma (directly or over SSH):
//!   curl -fsSL http://homeconnect.bam/onboard.sh | bash
//! It repoints openpilot at homeconnect and forces re-registration; the user
//! then claims the device in the UI. No secrets are embedded.

use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;

use crate::state::AppState;

const SCRIPT: &str = r#"#!/usr/bin/env bash
# homeconnect device onboarding. Run ON the comma, or over SSH:
#   curl -fsSL __HC_HOST__/onboard.sh | bash
# Append `-s -- --reboot` to reboot automatically when done.
set -euo pipefail

HOST="__HC_HOST__"
WS="${HOST/http/ws}"
CONT=/data/continue.sh

echo "homeconnect onboarding -> $HOST"

# 1) Point openpilot/sunnypilot at homeconnect. continue.sh lives OUTSIDE
#    /data/openpilot, so this survives openpilot updates.
if [ ! -f "$CONT" ]; then
  printf '#!/usr/bin/env bash\nexec /data/openpilot/launch_openpilot.sh\n' > "$CONT"
fi
cp -n "$CONT" "$CONT.pre-homeconnect" 2>/dev/null || true
# Drop any prior host exports, then (re)insert ours before the launcher exec.
sed -i '/export API_HOST=/d; /export ATHENA_HOST=/d; /export MAPS_HOST=/d' "$CONT"
if grep -qE '^[[:space:]]*exec ' "$CONT"; then
  awk -v h="$HOST" -v w="$WS" '
    /^[[:space:]]*exec /&&!p { print "export API_HOST=\""h"\"";
                               print "export ATHENA_HOST=\""w"\"";
                               print "export MAPS_HOST=\""h"\""; p=1 }
    { print }' "$CONT" > "$CONT.hc" && mv "$CONT.hc" "$CONT"
else
  { echo "export API_HOST=\"$HOST\""; echo "export ATHENA_HOST=\"$WS\""; echo "export MAPS_HOST=\"$HOST\""; } >> "$CONT"
fi
chmod +x "$CONT"
echo "OK: patched $CONT (backup: $CONT.pre-homeconnect)"

# 2) Force re-registration so homeconnect learns this device's key.
rm -f /data/params/d/DongleId
echo "OK: cleared cached DongleId (device re-registers on next boot)"

# 3) Apply.
if [ "${1:-}" = "--reboot" ]; then
  echo "rebooting..."
  sudo reboot
else
  echo
  echo "Done. Reboot to apply:   sudo reboot"
  echo "After it boots: open $HOST, log in, and Claim the device under '+ Add device'."
fi
"#;

/// GET /onboard.sh — public; returns the setup script with the host substituted.
pub async fn onboard_script(State(state): State<AppState>) -> impl IntoResponse {
    let body = SCRIPT.replace("__HC_HOST__", &state.config.public_url);
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        body,
    )
}
