//! Row structs mapped from SQLite (booleans are stored as INTEGER 0/1).

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct User {
    pub id: i64,
    pub identity: String,
    pub username: String,
    pub password_hash: String,
    pub email: Option<String>,
    pub is_admin: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Device {
    pub dongle_id: String,
    pub serial: String,
    pub imei: String,
    pub imei2: String,
    pub public_key: String,
    pub owner_id: Option<i64>,
    pub online: i64,
    pub last_athena_ping: i64,
    pub uploads_allowed: i64,
    pub alias: String,
    pub device_type: String,
    pub server_storage: i64,
    pub last_gps_lat: f64,
    pub last_gps_lng: f64,
    pub last_gps_time: i64,
    pub created_at: i64,
    pub last_addr: String,
}
