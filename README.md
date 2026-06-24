# homeconnect

A self-hosted, **home-first** server for [comma.ai](https://comma.ai) devices —
a from-scratch alternative to comma connect for people running a handful of
devices on their own hardware, not a fleet.

It speaks the comma device protocol exactly (so your comma works with zero app
changes — just repoint it), but everything behind that is redesigned for a home:
a **single Rust binary + SQLite + files on disk**, local logins, and no fleet
plumbing. It **does less** (no GitHub OAuth, no Postgres/Redis/object store) and
**more** (in-browser playback of the **driver and full-res cameras** via on-the-fly
transcoding, plus log **retention**) than the fleet-oriented stacks.

## Features

- **Drop-in device support** — implements comma's contract (`pilotauth`, athena
  websocket, `upload_url`, `connectincoming`, …). Point the device at this server
  and it registers, stays online, and uploads.
- **Drive browsing** — trips parsed from `qlog`s into routes/segments with GPS
  path, mileage, engage/disengage events, and thumbnail sprites.
- **In-browser playback** — qcamera HLS plus **on-demand HEVC→H.264 transcoding**
  so the **road, wide, and driver** full-res cameras all play in a normal browser
  (cached after first view). Optional **GPU acceleration** (VAAPI) with a
  CPU fallback, and a runtime **device selector** (CPU / any detected GPU).
- **Telemetry overlay** — a HUD synced to playback shows **speed, gear, turn
  signals, brake, and openpilot engagement** (parsed from `CarState`).
- **Map + synced video** — MapLibre route path with a marker that tracks playback;
  resizable panes; 0.5×–8× playback speed.
- **Split audio** — the qcamera mic track is extracted to a separate file and
  played in sync over the (silent) full-res/driver cameras, without re-muxing.
- **Manage data** — per drive, download selected file types as a (stored) zip, or
  delete them off the server.
- **Local accounts** — username/password (Argon2), server-issued JWTs. No OAuth.
- **Retention** — auto-prune by age / per-device count / total size, with an admin
  settings page.
- **Easy onboarding** — a served `onboard.sh` repoints a device in one command;
  unpaired devices are then claimed with a click.

## Architecture

```
            ┌──────────────────────── homeconnect (one binary) ───────────────────────┐
 comma ──►  │ axum HTTP/WS  ·  SQLite (WAL, sqlx)  ·  filesystem blob store            │
 browser ►  │ background workers: athena keepalive · qlog parser · retention · ffmpeg  │
            │ serves the built Svelte SPA at /                                          │
            └───────────────────────────────────────────────────────────────────────────┘
```

- **One process, one origin.** The binary serves the API, the device endpoints,
  the media, *and* the SPA — no CORS, no second service.
- **SQLite** via `sqlx` (runtime queries; migrations embedded in the binary).
- **Filesystem blob store** under the data dir — replaces an object store entirely.
- **Cereal/capnp** qlog parsing (schemas vendored under `vendor/cereal`).
- **ffmpeg** (CLI) for HEVC→H.264 transcoding and audio extraction (VAAPI or CPU).
- **Svelte + Vite** SPA (`web/`), MapLibre for maps, hls.js for video.

Stack: axum 0.8, tokio, sqlx 0.9 (SQLite), jsonwebtoken 10, argon2, capnp 0.26,
zstd/bzip2, image, zip; Svelte 5 + Vite + MapLibre + hls.js.

## Quick start

Requires Docker (with Compose). The production image bundles the release binary,
the built SPA, and ffmpeg.

```sh
# 1. Set a secret (any random base64). Kept out of git and the image.
echo "HC_JWT_SECRET=$(head -c 32 /dev/urandom | base64)" >  .env
echo "HC_PUBLIC_URL=http://homeconnect.bam"               >> .env   # how device+browser reach you

# 2. Build and run (data persists in the mounted volume).
docker compose build
docker compose up -d

# 3. Create your first (admin) user.
docker compose run --rm app create-user <username> <password> [email]
```

Edit the volume path in `docker-compose.yml` (`/data/storage/homeconnect:/data`)
to wherever you want SQLite + blobs to live, and put a reverse proxy
(e.g. Caddy) in front mapping your hostname to `127.0.0.1:8099`. Then open
`HC_PUBLIC_URL` and log in.

### Onboard a device

In the SPA, **+ Add device** shows a one-liner to run on the comma (or over SSH):

```sh
ssh comma@<device-ip> 'curl -fsSL http://homeconnect.bam/onboard.sh | bash -s -- --reboot'
```

`onboard.sh` repoints openpilot at this server (patching `/data/continue.sh`,
which survives openpilot updates) and clears the cached dongle so it
re-registers. After it reboots and registers, **Claim** it in the UI. (A
device-signed pairing token via `pilotpair` is also supported.)

## Configuration

All via env (`.env` for secrets):

| Var | Default | Meaning |
|---|---|---|
| `HC_BIND` | `0.0.0.0:8099` | listen address |
| `HC_DATA_DIR` | `./data` | SQLite + blobs + transcode cache |
| `HC_PUBLIC_URL` | `http://localhost:8099` | base URL baked into media/upload/onboard URLs |
| `HC_JWT_SECRET` | dev placeholder | **set this** — base64 HMAC secret for user JWTs |
| `HC_WEB_DIR` | `./web/dist` | built SPA directory |
| `HC_VAAPI_DEVICE` | (unset → CPU) | default DRM render node for GPU transcoding, e.g. `/dev/dri/renderD128` |
| `HC_RETAIN_DAYS` | `30` | keep drives newer than N days (0 = unlimited) |
| `HC_RETAIN_DRIVES` | `30` | max drives per device (0 = unlimited) |
| `HC_RETAIN_GB` | `100` | max total storage GB (0 = unlimited) |

Retention and the transcode device are overridable at runtime from the admin
**Settings** page (the env values are just defaults).

### GPU transcoding (optional)

To offload full-res/driver transcoding to a GPU via VAAPI, give the container the
DRM devices and the host `render` group, then point `HC_VAAPI_DEVICE` at a render
node (see `docker-compose.yml`):

```yaml
    devices: [ "/dev/dri:/dev/dri" ]
    group_add: [ "989" ]   # host 'render' group gid (check: getent group render)
```

The image ships both the Mesa (AMD) and Intel VAAPI drivers; the **Settings →
Transcoding device** dropdown lists every detected GPU plus CPU, and any GPU
failure falls back to CPU per-transcode. Note that a low-end discrete card can be
*slower* than a modern iGPU's fixed-function encoder — pick by measuring.

## Roadmap / ideas

More can be mined from the qlog data we already collect. Rough priority:

**Quick wins (data already in hand)**
- [ ] Trip stats + **autonomy %** — engaged ÷ total miles, disengagements/100 mi, avg/max speed, drive time; per-drive and all-time.
- [ ] **Hard-event highlights** — auto-flag hard braking / acceleration / cornering (from `aEgo`/`yawRate`) and list them like engagements (click to jump).
- [ ] Speed/accel **graph along the scrubber**.

**Higher value, moderate effort (verify the message is in the qlog first)**
- [ ] **Disengagement reasons** (`onroadEvents`) — annotate each disengage with *why* (override, distracted, model-uncertain, …). The standout review feature.
- [ ] **Driver attention** timeline + distracted markers (`driverMonitoringState`).
- [ ] **Lead car / following distance** over time (`radarState`).
- [ ] **Device health** — CPU temp, free space, network, thermal-throttle events (`DeviceState`).

**Flashy / bigger projects**
- [ ] openpilot-style **overlay on the road camera** — predicted path + lane lines + lead box (`modelV2`; needs camera projection + a canvas synced to playback).
- [ ] **All-drives heatmap** — every trip's GPS aggregated on one map.

**Intentionally out of scope**
- Live view / remote control (WebRTC + steering/throttle) — security blast radius.
- EV battery/SoC & power — not in the logged CAN (BMS is behind the powertrain gateway). See CLAUDE.md.

## Development

See [CLAUDE.md](./CLAUDE.md) for the full developer guide (build/test workflow,
module layout, the device contract, and gotchas). In short: build/test happen in
a toolchain container because the host needs capnp/ffmpeg/node —

```sh
docker compose -f docker-compose.dev.yml up -d
docker compose -f docker-compose.dev.yml exec app cargo test
docker compose -f docker-compose.dev.yml exec --workdir /work/web app npm run build
```

## Attribution & license

homeconnect is released under the [MIT License](./LICENSE).

The Cap'n Proto schemas under `vendor/cereal/` are from
[commaai/cereal](https://github.com/commaai/cereal) and retain their original
(MIT) license.
