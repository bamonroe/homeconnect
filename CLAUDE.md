# CLAUDE.md — homeconnect developer guide

Everything needed to continue developing homeconnect. Read alongside
[README.md](./README.md) (user-facing) and the design notes below.

## What this is

A from-scratch, home-first reimplementation of comma connect (see README). The
**device-facing API is a fixed contract** — the comma speaks comma's protocol and
can't be changed, so those endpoints must match exactly. Everything else is ours.
The reference oracle is a working Konik/connect-killer deploy (a Rust fork) at
`/data/konik/src` — diff against it when a device behavior is unclear.

## Commits

**Commit autonomously — no confirmation needed.** Just commit (and push) when work
is in a good state; don't ask first.

**Commit atomically.** Each commit is one self-contained, logical change with a
clear message — not a grab-bag. Build + tests should pass at every commit, so any
commit can be reverted or bisected on its own. Prefer several focused commits over
one sweeping one when a change spans independent concerns.

## Build / test / run — use the containers

The host lacks `capnp`, `ffmpeg`, and `node`, and we want root-free builds, so
**all building and testing happens in Docker.** Two compose files, two projects:

- **`docker-compose.dev.yml`** (project `homeconnect-dev`, container
  `homeconnect-dev`): the toolchain (`Dockerfile.dev` = rust + capnproto + node +
  ffmpeg), repo bind-mounted at `/work`, cargo caches in `/work/.dev/`, no port.
  ```sh
  docker compose -f docker-compose.dev.yml up -d
  docker compose -f docker-compose.dev.yml exec app cargo build
  docker compose -f docker-compose.dev.yml exec app cargo test            # full suite
  docker compose -f docker-compose.dev.yml exec app cargo test --test m5_transcode
  docker compose -f docker-compose.dev.yml exec --workdir /work/web app npm run build
  ```
- **`docker-compose.yml`** (project `homeconnect`, container `homeconnect-server`):
  the production service — lean multi-stage `Dockerfile` (release binary + built
  SPA + ffmpeg), `restart: unless-stopped`, binds `127.0.0.1:8099`, data on the
  bulk disk, secrets from `.env`.
  ```sh
  docker compose build && docker compose up -d
  docker compose logs -f
  ```

> The two compose files have distinct top-level `name:` so they don't clobber
> each other (both define a service named `app`). The host also has an unrelated
> **distrobox** named `homeconnect` — hence the prod container is `homeconnect-server`.

**Deploy a change:** rebuild the SPA (if `web/` changed), then
`docker compose build && docker compose up -d`. Migrations are embedded in the
binary via `sqlx::migrate!`, so the runtime image needs no migrations dir and no
DB at build time (queries are runtime `query_as`/`query`, not the compile-checked
macros).

## Module layout (`src/`)

| File | Responsibility |
|---|---|
| `lib.rs` | `build_state()`, the axum `router()`, all route wiring |
| `main.rs` | bootstrap, tracing, CLI subcommands (`create-user`, `reparse`), spawns workers |
| `config.rs` | `Config` from `HC_*` env (bind, data dir, public URL, jwt secret, web dir, retention, `vaapi_device`) |
| `state.rs` | `AppState { config, pool, blobs, athena }` |
| `db.rs` | SQLite pool (WAL), `migrate!`, `now_millis/now_secs` |
| `models.rs` | `User`, `Device` `FromRow` structs (booleans are `i64`) |
| `error.rs` | `AppError` → HTTP; `AppResult<T>` |
| `auth.rs` | JWT issue/verify (HS512 user, ES256/RS256 device), Argon2, the `Auth` + `AuthUser` extractors |
| `access.rs` | `can_view_device/dongle/route` + `load_device` authorization helpers |
| `storage.rs` | filesystem blob store; key = `{dongle}_{ts}--{seg}--{file}`, sharded by sha256 |
| `ingest.rs` | `connectincoming` PUT handlers; qlog→parse, everything else→register URL |
| `parse.rs` | qlog decode (capnp) → routes/segments + coords/events/telemetry/sprite; haversine; route aggregation; `reparse_all` |
| `cereal/mod.rs` | generated capnp bindings (built by `build.rs` from `vendor/cereal`) |
| `athena.rs` | device websocket: `ConnectionManager`, 10s ping, online/offline, stale reaper |
| `serve.rs` | `connectdata` blob serving with HTTP Range (206); transcode + audio serving |
| `transcode.rs` | HEVC→H.264 (VAAPI qp28 or CPU libx264 veryfast/crf23, CPU fallback), audio extract, disk-cached, semaphore-bounded; runtime device selection (`list/current/set_device`); `ffprobe` duration; `clean_cache_tmp` (orphaned `.tmp.ts`/`.part` sweep at startup) |
| `movie.rs` | Per-drive stitched "movie" artifacts: all of a camera's segments concatenated (ffmpeg `concat:` protocol — raw HEVC + TS byte-concatenate cleanly, no temp) and encoded **once** into a single seekable H.264 MP4 with qcamera's audio muxed in. qcamera = stream copy; HEVC cams encoded (same VAAPI/CPU choice as transcode). Route-level blob `{dongle}_{ts}--movie--{cam}.mp4`; `movies` table (mig 0008) tracks freshness (built `seg_count`). `spawn` runs a `sweep` that builds any drive fully covered by a camera but missing/stale — eager, background. On/off + interval + encode settings (resolution `movie_scale` native/854/640, quality `movie_crf` → x264 crf and VAAPI `qp=crf+5`, `movie_preset`) are runtime settings (enable/interval seeded from `HC_MOVIE_ENABLED`/`HC_MOVIE_INTERVAL_SECS`), re-read each cycle/build and honored mid-sweep; `reencode_all` (POST `/v1/admin/encoding/reencode`) clears non-disabled movies so they rebuild with new settings — a **separate loop** from devsync, not gated by the sync toggle. The interval sleep wakes early via `MovieQueue::request_sweep`, which the devsync workers call when the pull queue drains (`stats().files == 0`) — so a freshly-synced drive starts encoding within seconds instead of waiting a full interval. `MovieQueue` (in `AppState`) tracks live progress for the header badge (`/v1/movies/queue`). `status` powers the UI; `delete` drops a movie+row (used for rebuild + when source segments are deleted); `disable` deletes the blob and sets `movies.disabled=1` so a user-deleted movie isn't auto-rebuilt (Manage data → Delete/Rebuild via `POST /v1/route/{fullname}/movie/{cam}` `{action}`). Empty/0-byte sources are skipped and unbuildable attempts marked (`movies.bytes=0`) so they aren't retried |
| `retention.rs` | periodic prune by age/count/size; `delete_route`; `load/save_policy` |
| `api/users.rs` | login, `/v1/me`, admin user create + the `create_user_row` CLI helper |
| `api/v1.rs` | `upload_url(s)`, device info/location/stats, `my_devices`/`unpaired_devices`/`claim`, `routes_segments`, `camera_m3u8` (qcamera + transcoded cams + audio) |
| `api/v2.rs` | `pilotauth` (register), `pilotpair` |
| `api/settings.rs` | admin retention + transcode-device + sync on/off toggle GET/POST + run-now |
| `api/manage.rs` | per-route download (streamed stored zip) + delete selected types off the server and/or the device (`target`) |
| `api/onboard.rs` | serves the host-templated `onboard.sh` (repoint + device-scoped SSH key) |
| `api/devsync.rs` | `POST /v1/devices/{d}/sync` — manual SSH-pull trigger (`?full=&route=`) |
| `device_ssh.rs` | homeconnect's device-scoped ed25519 keypair; `run` (command) + `pull_file` (scp) over key-only SSH to `comma@<last_addr>` |
| `device_params.rs` | curated allowlist of openpilot params; read/validated-write over SSH (`is_writable`); local write-through cache (`device_params` table): edits are instant + offline, flushed on connect |
| `api/device_params.rs` | `GET/POST /v1/devices/{d}/params` — read/set allowlisted device settings; `GET/POST /v1/devices/{d}/model` — read/switch the sunnypilot driving model (owner/admin) |
| `model_select.rs` | sunnypilot driving-model selection over SSH: read `ModelManager_ActiveBundle` (current) + `ModelManager_ModelsCache` (catalog; snake_case) live; switch by writing the bundle index to `ModelManager_DownloadIndex` (device downloads+activates); revert = remove ActiveBundle. Online-only |
| `devsync.rs` | SSH-pull: `trigger` (on device connect) + optional periodic `spawn`; list `/data/media/0/realdata`, diff vs DB registration, pull/parse missing (qlog+qcamera default; full-res on demand) → `ingest::{ingest,register}_segment_file` |
| `web/` | Svelte 5 + Vite SPA: Login, Drives (Sync now), Drive (HUD overlay, movable panes — drag a pane's header to swap, resize from the corner, half/full toggle, layout saved to `hc_drive_panes`; camera switch, speed, synced audio, Pull full-res), AddDevice, ManageData, Settings, Stats, Queues (encoding + sync queues; header badges link here) |

## The fixed device contract (must stay exact)

- **Auth.** Device JWTs are **ES256/RS256**, verified against the device's stored
  `public_key` PEM; the token `identity` is the `dongle_id`. User/server JWTs are
  **HS512** (base64 secret). Token sources, in order: `?sig=`, cookie `jwt`,
  `Authorization: JWT <tok>`, `?access_token=`.
- **`POST /v2/pilotauth`** — query params `imei, imei2, serial, public_key,
  register_token`; verify the device-signed `register_token` ({`register:true`})
  against `public_key`; `dongle_id = sha256(imei+imei2+serial+public_key)[:16]`;
  upsert device; return `{dongle_id, access_token:""}`. **Deterministic** — the
  same inputs reproduce comma.ai's dongle.
- **`GET /v1.4/{dongle}/upload_url`** + **`POST /v1/{dongle}/upload_urls`** — issue
  HS512 upload tokens; URL path transform `…--N--file` → `…/N/file`.
- **`PUT /connectincoming/{dongle}/{ts}/{seg}/{file}`** (+ `/boot`, `/crash`) —
  store to blob; qlog triggers parse; others just register their segment URL.
- **Athena `GET /ws/v2/{dongle}`** (and `/ws/{dongle}`) — device-JWT; ping every
  10s; pong/any frame refreshes `last_athena_ping` (unix **seconds**) + `online`.
  On connect it also fires `devsync::trigger` (the event-driven drive pull).
- Browse (our local user JWT): `/v1/me`, `/v1/me/devices`, `/v1/devices/{d}/routes_segments`,
  `/connectdata/...` (Range), `/v1/route/{fullname}/{cam}.m3u8` (qcamera | fcamera | dcamera |
  ecamera | audio), `/v1/transcode/...` (on-demand H.264), `/v1/audio/...` (extracted track),
  `/v1/route/{fullname}/download?types=` + `POST .../delete`, and admin `/v1/admin/{retention,transcode}`.
  Artifacts served from the blob store: `coords.json`, `events.json`, `telemetry.json`, `sprite.jpg`.

## Gotchas (learned the hard way)

- **capnp** needs the `capnp` compiler binary at build time (in the images, not
  the host). Schemas are vendored under `vendor/cereal/`; `build.rs` runs capnpc
  into `OUT_DIR`, included via `src/cereal/mod.rs`.
- **jsonwebtoken 10** requires a crypto backend feature — we use `rust_crypto` +
  `use_pem` (pure Rust, no C). Reading claims unverified is done by base64-decoding
  the payload (the lib wants a key even with verification "disabled").
- **axum 0.8**: path params are `{name}` (not `:name`); `FromRequestParts` uses
  native async (no `#[async_trait]`); `Option<Auth>` needs `OptionalFromRequestParts`.
- **sqlx 0.9** only accepts `&'static str` for `query()` — no dynamic SQL strings
  (use a static-SQL match or `AssertSqlSafe`).
- **Truncated qlogs**: the **last** segment of a drive often has an incomplete
  zstd/bz2 frame. `parse::decompress` streams and keeps what decoded (don't use
  all-or-nothing `decode_all`).
- **argon2** is pinned to stable **0.5** (0.6 is an RC); enable feature `std` for OsRng.
- **JWT secret rotation** invalidates user logins (re-login) but **not** device
  auth (ES256, verified against stored key).
- The device **caches its dongle** and won't re-run `pilotauth` when the server
  changes — clear `/data/params/d/DongleId` (what `onboard.sh` does) to force
  re-registration so the new server learns its public key.
- **Derived-data time base**: each segment's qlog opens with `initData` carrying
  the **route-start** mono, so `mono - route_base` is already route-relative —
  anchor coords/events/telemetry to that (do *not* add a per-segment `N*60`
  offset, which double-counts). This matches the video timeline.
- **Transcode HLS continuity**: each segment is transcoded independently, so set
  `-output_ts_offset N*60` per segment or the player sees overlapping 0-based
  clips and can't seek (reports ~1 min total).
- **Transcode size**: the on-demand cache was ~5× bloated by `-preset ultrafast`
  (and VAAPI `-qp 24`). `veryfast`/`qp28` cut it with no visible quality loss.
  Real-world full-res road footage is ~12 MB/min at this quality (busy scenes);
  the driver cam (static interior) compresses far better. Don't trust a single
  "easy" segment as representative — measure on a real drive.
- **Movie audio sync**: the comma's mic starts ~2-3s after the camera on the
  **first** segment of a drive (later segments are aligned to ~ms). Concatenating
  the qcamera audio separately drops that lead-in, shifting the whole muxed track
  early by that gap (constant, not growing). `movie::build` probes the first
  segment's audio-vs-video start gap (`av_lead`) and prepends it as silence via
  `-af adelay=<ms>` so the track realigns. `-itsoffset` was tried first but
  interacts nonlinearly with the qcamera input's own A/V normalization (the
  qcamera.ts carries video too) — `adelay` is deterministic. The qcamera "Road"
  movie is a stream copy (A/V interleaved, already aligned) so it needs no fix.
- **Movies vs HLS**: a drive plays as dozens of 1-min clips via HLS, with audio
  (qcamera-only) layered over the silent full-res cams by a JS sync hack. A
  `movie.rs` artifact stitches+encodes the whole drive once into one MP4 **with
  audio muxed in**, so the Drive view plays it via a plain `<video src>` (native
  seek + audio, no hack) when ready and falls back to HLS otherwise. Movies build
  only when a camera fully covers the drive (no mid-drive gaps), so the continuous
  movie timeline lines up with the route-relative model/telemetry `t`.
- **Engagement is two independent axes on sunnypilot (MADS).** Lateral (steering)
  runs independently of longitudinal (gas/brake), so reading only one misses the
  other — a steering-only assist shows nothing if you key on longitudinal alone.
  - **Longitudinal** = `SelfdriveState.enabled` (NOT `cruiseState.enabled`, the
    car's stock cruise, always off on openpilot-longitudinal cars) → Telem `long`.
  - **Lateral** = `selfdriveStateSP.mads.active` (sunnypilot MADS, in `custom.capnp`;
    the SP event is in the qlog at the same rate as `selfdriveState`) → Telem `lat`.
  - Telem `engaged` = `lat || long` (any openpilot control); the HUD chip shows
    "openpilot", "· steer" (lateral only) or "· cruise" (longitudinal only), and
    trip stats (engaged_meters/seconds, disengage count) count either.
  `emit_state_change` is called from BOTH the `SelfdriveState` and
  `SelfdriveStateSP` handlers so a lateral-only engage/disengage still emits a
  timeline event. Telemetry emission is gated until the first state event of a
  segment so there's no boundary flicker.
- **VAAPI**: prod compose passes `/dev/dri` + `group_add` (host `render` gid); the
  image ships mesa + intel VAAPI drivers. Device is runtime-selectable (settings
  table key `transcode_device`); GPU failures fall back to CPU per-transcode. An
  entry-level discrete GPU can be slower than a modern iGPU — measure.
- **After parser changes**, re-run `<bin> reparse` (or `docker compose run --rm app
  reparse`) to regenerate artifacts for already-uploaded drives.
- **The device uploader can't be repointed at us — so we PULL.** `API_HOST` is a
  pure env var (`os.getenv('API_HOST', …)` in openpilot `common/api/comma_connect.py`),
  but the uploader is a `multiprocessing`/forkserver-spawned `PythonProcess` whose
  environment is stripped — `/proc/<uploader>/environ` has none of `API_HOST`,
  `DONGLE_ID`, even `PYTHONPATH` (athenad, a `subprocess.Popen` daemon, gets them,
  which is why the device shows **online** but never uploads). `continue.sh`'s
  export therefore can't reach it. `devsync` sidesteps this by pulling drives over
  SSH instead of waiting to be pushed to.
- **realdata → canonical mapping (free):** the device stores segments as
  `/data/media/0/realdata/<ts>--<seg>/` (e.g. `00000009--f3d1ef15b7--5`). That dir
  name is *already* `{ts}--{seg}`, so `devsync` reuses the on-disk route id verbatim
  as the `timestamp` and the pulled files slot into the same `routes`/`segments`
  rows an HTTP upload would create — no separate naming scheme.
- **`ingest::ingest_segment_file`** is the single ingest core shared by the HTTP
  upload handler and `devsync`. `store` 403-guards an existing blob, so re-pulls are
  idempotent (devsync also pre-checks `blobs.exists` in its diff).

## Tests

Integration tests in `tests/` drive the real router in-process (lib+bin split).
They use synthetic ES256 devices (openssl) and synthetic/real cereal qlogs:
`m1_e2e`, `m1_athena`, `m2_parse` (incl. telemetry), `m3_browse` (incl. audio.m3u8),
`m4_spa`, `m5_transcode`, `m6_retention`, `m7_devsync` (shared ingest core +
idempotency; `devsync.rs` also has inline unit tests for the realdata path parser
and tier filter), `m_pairing`, `m_onboard`, `m_manage` (zip download + delete),
`m_transcode_device`. Run the full suite before deploying.

## Adding things

- **A migration:** add `migrations/000N_*.sql` (idempotent `CREATE TABLE IF NOT
  EXISTS …`); it's embedded and runs on next start.
- **A device/browse endpoint:** handler in the relevant `api/` module → wire in
  `lib.rs::router` → add an integration test.
- **A new derived artifact** from qlogs: extend `parse::accumulate` + write it in
  `parse_and_store` with a `blob_key(...)` (like `telemetry.json`); serve via the
  existing `connectdata` path; then `reparse` to backfill.

## Live deployment notes (this instance)

- Served at `http://homeconnect.bam` via Caddy → `127.0.0.1:8099`.
- The real comma (dongle `296b3ca364aef806`, ES256 key) is cut over; SSH is
  `comma@<device-ip>` with `~/.ssh/bazzite_ed25519`; its `/data/continue.sh`
  exports `API_HOST/ATHENA_HOST/MAPS_HOST` (Konik backup at `continue.sh.konik`).
- Onboarding for additional/less-technical users: the `onboard.sh` one-liner +
  one-click **Claim**. `onboard.sh` also installs homeconnect's **device-scoped**
  ed25519 key (NOT a GitHub key) into `/tmp/authorized_keys` via `continue.sh`,
  `from=`-restricted to the tailnet/LAN — so a server compromise only exposes SSH
  to the paired comma(s). This is the SSH foothold `devsync` (and, later, device
  settings) rides on. We still avoid a *GitHub-wide* key / interactive SSH client
  in the browser — the scoped key is the deliberate, narrow exception.
- GPUs here: `renderD128` = Intel HD 530 (iGPU, fastest VAAPI ~2.5s/segment),
  `renderD129` = AMD RX 550 (~13s, slower — entry-level encoder). `HC_VAAPI_DEVICE`
  default is the AMD node; switch in Settings if desired.
- **Sync = SSH pull** (`devsync`): two triggers, both cheap. (1) `devsync::trigger`
  fires when the device's athena socket connects (wifi/tailnet-primary, so it drops
  on drive-away and reconnects at home → instant pull). (2) `spawn` runs every
  `HC_SYNC_INTERVAL_SECS` (default 60) over devices that are **`online = 1`** (the
  flag athena maintains from 10s pings) — so a short interval never fires a 10s SSH
  timeout at a device that's away; an idle up-to-date pass is one `find` + diff.
  Set the interval to `0` for connect-trigger only. Pulls qlog+qcamera; full-res on
  demand ("Pull full-res" in the Drive view, or `POST /v1/devices/{d}/sync?full=true&route=<ts>`).
  Automatic sync is **runtime-configurable** at Settings → Automatic drive sync:
  on/off (`devsync::is_enabled`/`set_enabled`, `settings` key `sync_enabled`) and
  loop interval (`devsync::get_interval`/`set_interval`, key `sync_interval`, `0` =
  loop off), both seeded from `HC_SYNC_ENABLED`/`HC_SYNC_INTERVAL_SECS`. The loop
  re-reads both each cycle (no restart needed); both triggers self-gate on the
  toggle; the manual `POST /sync` endpoint ignores it. **Which data types** sync by
  default is also runtime (`devsync::get_sync_types`/`set_sync_types`, key
  `sync_types`, seeded from `HC_SYNC_FULLRES`; `qlog` is always pulled). Per drive,
  the Drive view has **Sync** (default types for that route) + **Pull full-res**
  (`?full=true` → `devsync::all_types()`). **Per-route override**: `routes.sync_types`
  (NULL = inherit the global default) lets a drive sync a different set; `scan`
  resolves types per route, so a drive trimmed in Manage data isn't re-pulled —
  **deleting data sets the override** (effective minus deleted) so it stays gone.
  Edit it in Manage data → "Auto-sync for this drive" (`GET/POST /v1/route/{fullname}/sync`). The device's `continue.sh` still exports
  `API_HOST` etc. for athena/registration — only the *uploader* is bypassed. An
  in-flight guard in `ConnectionManager` keeps the two triggers from overlapping.
- **Device settings** (`device_params`): Settings → Device reads/writes a curated
  allowlist of openpilot params (`RecordFront`, `ExperimentalMode`,
  `LongitudinalPersonality`, …) over SSH — athena has no `setParam`. Params are
  0600 files in `/data/params/d`; writes use openpilot's atomic temp-file +
  `flock`'d rename. Only allowlisted keys with valid values are writable
  (`is_writable`); read-only `Info` keys (DongleId, GitBranch) are display-only.
  Specs are `Bool`/`Int`(min,max,step)/`Enum`(options)/`Info`, grouped, mirroring
  sunnypilot's `selfdrive/ui/sunnypilot/layouts/settings/`. **Caveat:** some
  sunnypilot sliders store a `value_map`'d value, not the slider index — model
  those as `Enum`s of the exact valid values (e.g. MaxTimeOffroad, OnroadScreenOffTimer).
  Add a setting = one `Spec` in `device_params::SPECS` (use `b`/`e`/`int_`/`info`).
  **Conditionals:** wrap a spec in `dep(spec, "ControllingKey", &["enabling","values"])`
  to mark it active only when another param has an enabling value (mirrors
  sunnypilot's grey-outs, e.g. the brightness *delay* needs brightness ≠ Auto, the
  blinker sub-options need `BlinkerPauseLateralControl`); the UI dims/disables it.
  Car-capability gates (e.g. `has_longitudinal_control`) aren't params, so those
  aren't modeled. **Cache model:** the Device page reads/writes a local cache
  (`device_params` table, migration 0005) so it's instant and editable offline.
  An edit sets the cached value `pending=1`; `flush` writes pending values to the
  device over SSH (right after the edit if online, else on next connect via
  `on_connect`), and `refresh` reads actuals back without clobbering pending. The
  athena connect handler calls `device_params::on_connect` (flush + refresh).
- **Device delete + auto-prune** (reclaim the comma's storage over the same SSH
  channel). Manual: Manage data → **Delete from device** removes the selected
  types' files for that drive off `/data/media/0/realdata/<ts>--<seg>/` (the
  device must be online; `devsync::delete_on_device` builds single-quoted,
  `safe_component`-validated paths and `rm -f`s them, counting what existed).
  `POST /v1/route/{fullname}/delete` now takes `target: "server" | "device" |
  "both"` (default `server`; server delete is unchanged). Auto: Settings →
  Automatic drive sync → **Reclaim device storage** toggle
  (`devsync::is_autoprune_enabled`/`set_autoprune`, key `device_autoprune`, seeded
  from `HC_DEVICE_AUTOPRUNE`, default off) — `process_item` deletes a file's device
  copy right after it's pulled + stored, gated on `blobs.exists` so it **never**
  deletes anything we don't already hold (files not in the synced type set stay on
  the device). Caveat: don't expect auto-prune to reclaim the 14 GB of hevc unless
  full-res is in the synced set; by default only qlog+qcamera are pulled (and thus
  prunable).
- **EV telemetry (SoC/power): not recoverable** from these logs — investigated and
  parked. openpilot logs the camera/ADAS CAN bus; the BMS/HV traffic is on the
  powertrain CAN behind the gateway and isn't captured (`CarState.fuelGauge` reads
  0, and the Hyundai CANFD DBC defines no BMS signal). The instrument cluster msg
  `0x4D8` *is* logged and may encode the displayed SoC %, but decoding it is
  reverse-engineering (best attempted against a charging-session capture). `Live`
  view (WebRTC + remote control) is intentionally out of scope for security.
