# CLAUDE.md — homeconnect developer guide

Everything needed to continue developing homeconnect. Read alongside
[README.md](./README.md) (user-facing) and the design notes below.

## What this is

A from-scratch, home-first reimplementation of comma connect (see README). The
**device-facing API is a fixed contract** — the comma speaks comma's protocol and
can't be changed, so those endpoints must match exactly. Everything else is ours.
The reference oracle is a working Konik/connect-killer deploy (a Rust fork) at
`/data/konik/src` — diff against it when a device behavior is unclear.

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
| `main.rs` | bootstrap, tracing, the `create-user` CLI subcommand, spawns workers |
| `config.rs` | `Config` from `HC_*` env (bind, data dir, public URL, jwt secret, retention) |
| `state.rs` | `AppState { config, pool, blobs, athena }` |
| `db.rs` | SQLite pool (WAL), `migrate!`, `now_millis/now_secs` |
| `models.rs` | `User`, `Device` `FromRow` structs (booleans are `i64`) |
| `error.rs` | `AppError` → HTTP; `AppResult<T>` |
| `auth.rs` | JWT issue/verify (HS512 user, ES256/RS256 device), Argon2, the `Auth` + `AuthUser` extractors |
| `access.rs` | `can_view_device/dongle/route` authorization helpers |
| `storage.rs` | filesystem blob store; key = `{dongle}_{ts}--{seg}--{file}`, sharded by sha256 |
| `ingest.rs` | `connectincoming` PUT handlers; qlog→parse, everything else→register URL |
| `parse.rs` | qlog decode (capnp) → routes/segments + coords/events/sprite; haversine; route aggregation |
| `cereal/mod.rs` | generated capnp bindings (built by `build.rs` from `vendor/cereal`) |
| `athena.rs` | device websocket: `ConnectionManager`, 10s ping, online/offline, stale reaper |
| `serve.rs` | `connectdata` blob serving with HTTP Range (206); transcode serving |
| `transcode.rs` | HEVC→H.264 via ffmpeg, disk-cached, semaphore-bounded; `ffprobe` duration |
| `retention.rs` | periodic prune by age/count/size; `delete_route`; `load/save_policy` |
| `api/users.rs` | login, `/v1/me`, admin user create + the `create_user_row` CLI helper |
| `api/v1.rs` | `upload_url(s)`, device info/location/stats, `my_devices`, `routes_segments`, `camera_m3u8`, claim/unpaired |
| `api/v2.rs` | `pilotauth` (register), `pilotpair` |
| `api/settings.rs` | admin retention GET/POST + run-now |
| `api/onboard.rs` | serves the host-templated `onboard.sh` |
| `web/` | Svelte 5 + Vite SPA (Login, Drives, Drive, AddDevice, Settings) |

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
- Browse (our local user JWT): `/v1/me`, `/v1/me/devices`, `/v1/devices/{d}/routes_segments`,
  `/connectdata/...` (Range), `/v1/route/{fullname}/{cam}.m3u8`.

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

## Tests

Integration tests in `tests/` drive the real router in-process (lib+bin split).
They use synthetic ES256 devices (openssl) and synthetic/real cereal qlogs:
`m1_e2e`, `m1_athena`, `m2_parse`, `m3_browse`, `m4_spa`, `m5_transcode`,
`m6_retention`, `m_pairing`, `m_onboard`. Run the full suite before deploying.

## Adding things

- **A migration:** add `migrations/000N_*.sql` (idempotent `CREATE TABLE IF NOT
  EXISTS …`); it's embedded and runs on next start.
- **A device/browse endpoint:** handler in the relevant `api/` module → wire in
  `lib.rs::router` → add an integration test.
- **A new derived artifact** from qlogs: extend `parse::accumulate` + write it to
  the blob store with a `blob_key(...)`; serve via the existing `connectdata` path.

## Live deployment notes (this instance)

- Served at `http://homeconnect.bam` via Caddy → `127.0.0.1:8099`.
- The real comma (dongle `296b3ca364aef806`, ES256 key) is cut over; SSH is
  `comma@<device-ip>` with `~/.ssh/bazzite_ed25519`; its `/data/continue.sh`
  exports `API_HOST/ATHENA_HOST/MAPS_HOST` (Konik backup at `continue.sh.konik`).
- Onboarding for additional/less-technical users: the `onboard.sh` one-liner +
  one-click **Claim** (we deliberately did NOT bake an SSH client / GitHub-key
  flow into homeconnect — too much blast radius for the convenience).
