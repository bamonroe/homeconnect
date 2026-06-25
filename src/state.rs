use crate::athena::ConnectionManager;
use crate::config::Config;
use crate::movie::MovieQueue;
use crate::storage::BlobStore;
use crate::sync_queue::SyncQueue;
use sqlx::SqlitePool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: SqlitePool,
    pub blobs: BlobStore,
    pub athena: ConnectionManager,
    pub sync_queue: SyncQueue,
    pub movie_queue: MovieQueue,
}
