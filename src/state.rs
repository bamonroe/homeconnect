use crate::athena::ConnectionManager;
use crate::config::Config;
use crate::storage::BlobStore;
use sqlx::SqlitePool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: SqlitePool,
    pub blobs: BlobStore,
    pub athena: ConnectionManager,
}
