//! homeconnect — a home-first, self-hosted comma connect server.
//!
//! Library crate: exposes the modules and the router builder so both the binary
//! and integration tests share one definition of the app.

pub mod access;
pub mod api;
pub mod athena;
pub mod auth;
pub mod cereal;
pub mod config;
pub mod db;
pub mod error;
pub mod ingest;
pub mod models;
pub mod parse;
pub mod retention;
pub mod serve;
pub mod state;
pub mod storage;
pub mod transcode;

use axum::routing::{get, post, put};
use axum::Router;
use std::sync::Arc;

use crate::config::Config;
use crate::state::AppState;

/// Build the application state from a config: open the DB (running migrations),
/// create the blob store, and initialise the athena connection manager.
pub async fn build_state(config: Config) -> anyhow::Result<AppState> {
    std::fs::create_dir_all(&config.data_dir)?;
    let pool = db::init(&config.db_url()).await?;
    let blobs = storage::BlobStore::new(config.blobs_dir());
    std::fs::create_dir_all(blobs.root())?;
    Ok(AppState {
        config: Arc::new(config),
        pool,
        blobs,
        athena: athena::ConnectionManager::default(),
    })
}

/// The full HTTP router (all device + browse + auth routes).
pub fn router(state: AppState) -> Router {
    use tower_http::services::{ServeDir, ServeFile};
    use tower_http::trace::TraceLayer;

    // SPA: serve built assets from web_dir, falling back to index.html so
    // client-side routes resolve.
    let index = state.config.web_dir.join("index.html");
    let spa = ServeDir::new(&state.config.web_dir).fallback(ServeFile::new(index));

    Router::new()
        .route("/health", get(health))
        .route("/onboard.sh", get(api::onboard::onboard_script))
        // local user auth
        .route("/v1/auth/login", post(api::users::login))
        .route("/v1/me", get(api::users::me))
        .route("/v1/admin/users", post(api::users::create_user))
        // device registration + pairing
        .route("/v2/pilotauth", post(api::v2::pilotauth))
        .route("/v2/pilotpair", post(api::v2::pilotpair))
        // upload URL issuance
        .route("/v1.4/{dongle_id}/upload_url", get(api::v1::upload_url))
        .route("/v1.4/{dongle_id}/upload_url/", get(api::v1::upload_url))
        .route("/v1/{dongle_id}/upload_urls", post(api::v1::upload_urls))
        .route("/v1/{dongle_id}/upload_urls/", post(api::v1::upload_urls))
        // device info
        .route("/v1.1/devices/{dongle_id}", get(api::v1::device_info))
        .route("/v1.1/devices/{dongle_id}/stats", get(api::v1::device_stats))
        .route("/v1/devices/{dongle_id}/location", get(api::v1::device_location))
        // user browse
        .route("/v1/me/devices", get(api::v1::my_devices))
        .route("/v1/me/unpaired_devices", get(api::v1::unpaired_devices))
        .route("/v1/devices/{dongle_id}/claim", post(api::v1::claim_device))
        // admin: retention policy
        .route("/v1/admin/retention", get(api::settings::get_retention).post(api::settings::set_retention))
        .route("/v1/admin/retention/run", post(api::settings::run_retention))
        .route("/v1/devices/{dongle_id}/routes_segments", get(api::v1::routes_segments))
        .route("/v1/route/{fullname}/{cam}", get(api::v1::camera_m3u8))
        .route("/v1/transcode/{dongle}/{timestamp}/{segment}/{file}", get(serve::transcode))
        // blob serving (Range-capable): 5-part (with type) and 4-part variants
        .route("/connectdata/{type}/{dongle}/{timestamp}/{segment}/{file}", get(serve::connectdata))
        .route("/connectdata/{dongle}/{timestamp}/{segment}/{file}", get(serve::connectdata_notype))
        // ingest (device PUTs uploads here)
        .route("/connectincoming/{dongle}/{timestamp}/{segment}/{file}", put(ingest::upload_driving))
        .route("/connectincoming/{dongle}/boot/{file}", put(ingest::upload_boot))
        .route("/connectincoming/{dongle}/crash/{log_id}/{commit}/{name}", put(ingest::upload_crash))
        // athena outbound websocket
        .route("/ws/v2/{dongle_id}", get(athena::ws_handler))
        .route("/ws/{dongle_id}", get(athena::ws_handler))
        // uploaded segments are large; the 2 MB default body cap would reject them.
        .layer(axum::extract::DefaultBodyLimit::disable())
        .layer(TraceLayer::new_for_http())
        // everything not matched above → the SPA (assets + index.html fallback).
        .fallback_service(spa)
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}
