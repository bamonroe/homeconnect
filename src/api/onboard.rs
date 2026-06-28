//! Serves a device-onboarding shell script with this server's URL + SSH public
//! key baked in. Run it on the comma (directly or over SSH):
//!   curl -fsSL http://hc.bam/onboard.sh | bash
//! It repoints openpilot at homeconnect, installs homeconnect's device-scoped
//! SSH key (for device management — NOT a GitHub key), and forces
//! re-registration; the user then claims the device in the UI.
//!
//! Optional `--tailscale <authkey>` installs a tailscale client and joins the
//! tailnet (the coordination/login server is templated from config; the authkey
//! is a runtime arg and is never baked into this public script).

use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

use crate::auth::AuthUser;
use crate::device_ssh;
use crate::state::AppState;

const SCRIPT: &str = r#"#!/usr/bin/env bash
# homeconnect device onboarding. Run ON the comma, or over SSH:
#   curl -fsSL __HC_HOST__/onboard.sh | bash
# Options (any order, after `-s --`):
#   --reboot                 reboot automatically when done
#   --tailscale <authkey>    install tailscale and join the tailnet using <authkey>
#   --ts-login-server <url>  coordination/login server (default: baked-in, if any)
#   --ts-hostname <name>     tailnet hostname (default: comma-<HardwareSerial>)
#   --ts-version <ver>       tailscale version to fetch (default: __HC_TS_VERSION__)
# e.g.  curl -fsSL __HC_HOST__/onboard.sh | bash -s -- --tailscale <authkey> --reboot
set -euo pipefail

HOST="__HC_HOST__"
WS="${HOST/http/ws}"
CONT=/data/continue.sh
KEYFILE=/data/homeconnect_authorized_key
# homeconnect's device-scoped key, restricted to the tailnet/LAN (no GitHub).
HC_KEY='from="100.64.0.0/10,192.168.0.0/16,10.0.0.0/8,172.16.0.0/12" __HC_PUBKEY__'

# --- args ---
DO_REBOOT=0
TS_AUTHKEY=""
TS_LOGIN="__HC_TS_LOGIN__"
TS_HOSTNAME=""
TS_VERSION="__HC_TS_VERSION__"
while [ $# -gt 0 ]; do
  case "$1" in
    --reboot) DO_REBOOT=1 ;;
    --tailscale) TS_AUTHKEY="${2:-}"; shift ;;
    --tailscale=*) TS_AUTHKEY="${1#*=}" ;;
    --ts-login-server) TS_LOGIN="${2:-}"; shift ;;
    --ts-login-server=*) TS_LOGIN="${1#*=}" ;;
    --ts-hostname) TS_HOSTNAME="${2:-}"; shift ;;
    --ts-hostname=*) TS_HOSTNAME="${1#*=}" ;;
    --ts-version) TS_VERSION="${2:-}"; shift ;;
    --ts-version=*) TS_VERSION="${1#*=}" ;;
    *) echo "ignoring unknown arg: $1" >&2 ;;
  esac
  shift
done

echo "homeconnect onboarding -> $HOST"

# Download + install tailscale, register with the authkey (one time), and write a
# persistent bring-up (up.sh, no authkey stored) + boot hook (boot.sh). Returns
# non-zero on failure so the rest of onboarding still completes. Mirrors the
# transient-systemd-unit pattern continue.sh uses.
install_tailscale() {
  local TS=/data/tailscale ST=/data/tailscale/state tmp d LS=""
  [ -n "$TS_LOGIN" ] && LS="--login-server $TS_LOGIN"
  mkdir -p "$ST"
  if [ ! -x "$TS/tailscaled" ]; then
    echo "downloading tailscale ${TS_VERSION} (arm64)..."
    tmp=$(mktemp -d)
    curl -fsSL "https://pkgs.tailscale.com/stable/tailscale_${TS_VERSION}_arm64.tgz" -o "$tmp/ts.tgz" || { rm -rf "$tmp"; return 1; }
    tar -xzf "$tmp/ts.tgz" -C "$tmp" || { rm -rf "$tmp"; return 1; }
    d=$(find "$tmp" -maxdepth 1 -type d -name 'tailscale_*' | head -1)
    cp "$d/tailscale" "$d/tailscaled" "$TS/" || { rm -rf "$tmp"; return 1; }
    chmod +x "$TS/tailscale" "$TS/tailscaled"
    rm -rf "$tmp"
  fi
  # Persistent reconnect (no authkey; node is already registered after first run).
  cat > "$TS/up.sh" <<UPSH
#!/usr/bin/env bash
set +e
TS=/data/tailscale
SOCK=\$TS/state/tailscaled.sock
T="\$TS/tailscale --socket=\$SOCK"
for i in \$(seq 1 30); do [ -S "\$SOCK" ] && break; sleep 1; done
\$T up ${LS} --hostname ${TS_HOSTNAME} --accept-dns=true
# NetworkManager contends with Tailscale's DNS push; toggle + pin the .bam split.
\$T set --accept-dns=false; sleep 1
\$T set --accept-dns=true;  sleep 2
resolvectl dns    tailscale0 100.100.100.100 2>/dev/null
resolvectl domain tailscale0 "~bam" "~bam.net" 2>/dev/null
UPSH
  chmod +x "$TS/up.sh"
  # Boot hook: start the daemon + run up.sh as transient root units (called from
  # continue.sh with sudo, so no sudo needed inside).
  cat > "$TS/boot.sh" <<'BOOTSH'
#!/usr/bin/env bash
TS=/data/tailscale; ST=$TS/state
[ -x "$TS/tailscaled" ] || exit 0
systemctl reset-failed tailscaled tailscale-up 2>/dev/null
systemctl is-active --quiet tailscaled || \
  systemd-run --unit=tailscaled --collect --property=Restart=on-failure \
    "$TS/tailscaled" --state="$ST/tailscaled.state" --socket="$ST/tailscaled.sock" --tun=tailscale0 >>"$TS/up.log" 2>&1
systemd-run --unit=tailscale-up --collect "$TS/up.sh" >>"$TS/up.log" 2>&1
BOOTSH
  chmod +x "$TS/boot.sh"
  # Start the daemon now and register with the authkey (the one-time step).
  sudo systemctl reset-failed tailscaled 2>/dev/null || true
  sudo systemctl is-active --quiet tailscaled || \
    sudo systemd-run --unit=tailscaled --collect --property=Restart=on-failure \
      "$TS/tailscaled" --state="$ST/tailscaled.state" --socket="$ST/tailscaled.sock" --tun=tailscale0 >>"$TS/up.log" 2>&1 || return 1
  for i in $(seq 1 30); do [ -S "$ST/tailscaled.sock" ] && break; sleep 1; done
  "$TS/tailscale" --socket="$ST/tailscaled.sock" up $LS --authkey "$TS_AUTHKEY" --hostname "$TS_HOSTNAME" --accept-dns=true || return 1
  echo "OK: tailscale up as ${TS_HOSTNAME}${TS_LOGIN:+ (login-server $TS_LOGIN)}"
}

# 0) Optional: tailscale. Do this first so DNS/connectivity is up; non-fatal.
TSLINE=""
if [ -n "$TS_AUTHKEY" ]; then
  [ -z "$TS_HOSTNAME" ] && TS_HOSTNAME="comma-$(cat /data/params/d/HardwareSerial 2>/dev/null || true)"
  [ "$TS_HOSTNAME" = "comma-" ] && TS_HOSTNAME="comma-$(tr -dc a-f0-9 </dev/urandom 2>/dev/null | head -c8)"
  if install_tailscale; then
    TSLINE='sudo /data/tailscale/boot.sh # homeconnect-tailscale'
  else
    echo "WARN: tailscale setup failed; continuing without it" >&2
  fi
fi

# 1) Point openpilot/sunnypilot at homeconnect, and re-apply our SSH key (and the
#    tailscale boot hook) each boot. continue.sh lives OUTSIDE /data/openpilot, so
#    this survives updates.
if [ ! -f "$CONT" ]; then
  printf '#!/usr/bin/env bash\nexec /data/openpilot/launch_openpilot.sh\n' > "$CONT"
fi
cp -n "$CONT" "$CONT.pre-homeconnect" 2>/dev/null || true
# Drop any prior homeconnect-managed lines, then (re)insert before the launcher.
sed -i '/export API_HOST=/d; /export ATHENA_HOST=/d; /export MAPS_HOST=/d; /homeconnect-ssh/d; /homeconnect-tailscale/d' "$CONT"
if grep -qE '^[[:space:]]*exec ' "$CONT"; then
  awk -v h="$HOST" -v w="$WS" -v ts="$TSLINE" '
    /^[[:space:]]*exec /&&!p { if (ts != "") print ts;
                               print "export API_HOST=\""h"\"";
                               print "export ATHENA_HOST=\""w"\"";
                               print "export MAPS_HOST=\""h"\"";
                               print "cat /data/homeconnect_authorized_key >> /tmp/authorized_keys 2>/dev/null; chmod 600 /tmp/authorized_keys 2>/dev/null # homeconnect-ssh";
                               p=1 }
    { print }' "$CONT" > "$CONT.hc" && mv "$CONT.hc" "$CONT"
else
  { [ -n "$TSLINE" ] && echo "$TSLINE";
    echo "export API_HOST=\"$HOST\""; echo "export ATHENA_HOST=\"$WS\""; echo "export MAPS_HOST=\"$HOST\"";
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
if [ "$DO_REBOOT" = "1" ]; then
  echo "rebooting..."
  sudo reboot
else
  echo
  echo "Done. Reboot to apply:   sudo reboot"
  echo "After it boots: open $HOST, log in, and Claim the device under '+ Add device'."
fi
"#;
/// Default tailscale version fetched by the onboard script's `--tailscale` option.
const TS_VERSION: &str = "1.98.4";

/// GET /onboard.sh — public; returns the setup script with host + SSH pubkey.
pub async fn onboard_script(State(state): State<AppState>) -> impl IntoResponse {
    let pubkey = device_ssh::public_key(&state).await.unwrap_or_default();
    let body = SCRIPT
        .replace("__HC_HOST__", &state.config.public_url)
        .replace("__HC_PUBKEY__", pubkey.trim())
        .replace("__HC_TS_LOGIN__", &state.config.tailnet_login_server)
        .replace("__HC_TS_VERSION__", TS_VERSION);
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        body,
    )
}

/// GET /v1/onboard/defaults — values the "Add device" command builder prefills
/// (the configured tailnet login server + the default tailscale version). Any
/// logged-in user; no secrets here.
pub async fn onboard_defaults(
    State(state): State<AppState>,
    AuthUser(_user): AuthUser,
) -> Json<Value> {
    Json(json!({
        "login_server": state.config.tailnet_login_server,
        "ts_version": TS_VERSION,
    }))
}
