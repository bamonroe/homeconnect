//! Athena: the device's outbound websocket. The comma dials out to us (it's
//! behind cellular NAT), so this is a reverse tunnel. In v1 we use it purely
//! for liveness: keep the socket open, Ping every 10s, and treat a recent
//! Pong/`last_athena_ping` as "online". The JSON-RPC command bridge
//! (reboot/snapshot/nav) is deferred.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use tokio::sync::Mutex;

use crate::auth::Auth;
use crate::db::now_secs;
use crate::error::AppError;
use crate::state::AppState;

const PING_INTERVAL_SECS: u64 = 10;
/// A device is considered offline if we haven't heard from it in this long.
const OFFLINE_AFTER_SECS: i64 = 30;

/// Tracks which dongles currently hold an open athena socket, and which have a
/// sync in flight (so a connect-triggered pull can't pile up on reconnect flaps).
#[derive(Clone, Default)]
pub struct ConnectionManager {
    inner: Arc<Mutex<HashMap<String, ()>>>,
    syncing: Arc<Mutex<std::collections::HashSet<String>>>,
}

impl ConnectionManager {
    pub async fn connected(&self, dongle: &str) {
        self.inner.lock().await.insert(dongle.to_string(), ());
    }
    pub async fn disconnected(&self, dongle: &str) {
        self.inner.lock().await.remove(dongle);
    }
    pub async fn is_connected(&self, dongle: &str) -> bool {
        self.inner.lock().await.contains_key(dongle)
    }
    /// Claim the sync slot for a dongle; returns false if one is already running.
    pub async fn try_begin_sync(&self, dongle: &str) -> bool {
        self.syncing.lock().await.insert(dongle.to_string())
    }
    pub async fn end_sync(&self, dongle: &str) {
        self.syncing.lock().await.remove(dongle);
    }
}

/// GET /ws/v2/:dongle_id (and /ws/:dongle_id) — device-authenticated upgrade.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Path(dongle_id): Path<String>,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    auth: Auth,
) -> Result<Response, AppError> {
    // The athena socket is device-only: the token's identity must be this dongle.
    let device = auth
        .device
        .ok_or_else(|| AppError::Unauthorized("device token required".into()))?;
    if device.dongle_id != dongle_id || auth.claims.identity != dongle_id {
        return Err(AppError::Forbidden("token does not match dongle".into()));
    }

    // Record the address the device reached us from (Caddy sets X-Forwarded-For)
    // — its tailnet IP, used as the SSH target for device management.
    if let Some(addr) = client_addr(&headers) {
        let _ = sqlx::query("UPDATE devices SET last_addr = ? WHERE dongle_id = ?")
            .bind(&addr)
            .bind(&dongle_id)
            .execute(&state.pool)
            .await;
    }

    Ok(ws.on_upgrade(move |socket| handle_socket(socket, dongle_id, state)))
}

/// First IP from X-Forwarded-For (set by the reverse proxy).
fn client_addr(headers: &axum::http::HeaderMap) -> Option<String> {
    let xff = headers.get("x-forwarded-for")?.to_str().ok()?;
    let ip = xff.split(',').next()?.trim();
    if ip.is_empty() {
        None
    } else {
        Some(ip.to_string())
    }
}

async fn handle_socket(socket: WebSocket, dongle_id: String, state: AppState) {
    tracing::info!(dongle = %dongle_id, "athena connected");
    state.athena.connected(&dongle_id).await;
    mark_online(&state, &dongle_id, true).await;

    // The device just came online (e.g. drove home and rejoined wifi) — pull any
    // new drives now. Spawned so the ws keeps up its ping loop; `trigger`
    // self-gates on the runtime sync toggle and a per-dongle in-flight guard.
    {
        let st = state.clone();
        let dg = dongle_id.clone();
        tokio::spawn(async move { crate::devsync::trigger(&st, &dg).await });
    }

    let (mut sender, mut receiver) = {
        use futures_util::StreamExt;
        socket.split()
    };

    // Outbound: ping every 10s.
    let ping_state = state.clone();
    let ping_dongle = dongle_id.clone();
    let mut ping_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(PING_INTERVAL_SECS));
        loop {
            interval.tick().await;
            use futures_util::SinkExt;
            if sender.send(Message::Ping(axum::body::Bytes::new())).await.is_err() {
                break;
            }
            // A successful ping send is enough to refresh liveness; pongs also
            // refresh it below.
            touch_ping(&ping_state, &ping_dongle).await;
        }
    });

    // Inbound: any frame (pong, text, binary) refreshes liveness. Close ends it.
    let recv_state = state.clone();
    let recv_dongle = dongle_id.clone();
    let mut recv_task = tokio::spawn(async move {
        use futures_util::StreamExt;
        while let Some(msg) = receiver.next().await {
            match msg {
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => touch_ping(&recv_state, &recv_dongle).await,
            }
        }
    });

    // When either task ends, tear down the other.
    tokio::select! {
        _ = &mut ping_task => recv_task.abort(),
        _ = &mut recv_task => ping_task.abort(),
    }

    state.athena.disconnected(&dongle_id).await;
    mark_online(&state, &dongle_id, false).await;
    tracing::info!(dongle = %dongle_id, "athena disconnected");
}

async fn touch_ping(state: &AppState, dongle: &str) {
    let now = now_secs();
    let _ = sqlx::query("UPDATE devices SET last_athena_ping = ?, online = 1 WHERE dongle_id = ?")
        .bind(now)
        .bind(dongle)
        .execute(&state.pool)
        .await;
}

async fn mark_online(state: &AppState, dongle: &str, online: bool) {
    let now = now_secs();
    let _ = sqlx::query("UPDATE devices SET online = ?, last_athena_ping = ? WHERE dongle_id = ?")
        .bind(online as i64)
        .bind(now)
        .bind(dongle)
        .execute(&state.pool)
        .await;
}

/// Periodic sweep marking stale devices offline (covers ungraceful drops where
/// no Close frame arrives).
pub fn spawn_reaper(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(PING_INTERVAL_SECS));
        loop {
            interval.tick().await;
            let cutoff = now_secs() - OFFLINE_AFTER_SECS;
            let _ = sqlx::query(
                "UPDATE devices SET online = 0 WHERE online = 1 AND last_athena_ping < ?",
            )
            .bind(cutoff)
            .execute(&state.pool)
            .await;
        }
    });
}
