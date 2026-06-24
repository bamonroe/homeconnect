use std::path::PathBuf;

/// Runtime configuration, loaded from env (HC_*) with home-friendly defaults.
#[derive(Clone, Debug)]
pub struct Config {
    /// Address to bind the HTTP server to.
    pub bind: String,
    /// Base data directory (DB + blobs live under here).
    pub data_dir: PathBuf,
    /// Base64-encoded HMAC secret for signing user (HS512) JWTs.
    pub jwt_secret_b64: String,
    /// Public-facing base URL the device + browser use (no trailing slash).
    /// Used to build upload + connectdata URLs.
    pub public_url: String,
    /// Directory of the built SPA (served at `/`).
    pub web_dir: PathBuf,
    /// Default retention policy (overridable at runtime via the settings table).
    /// 0 = unlimited.
    pub retain_days: i64,
    pub retain_max_drives: i64,
    pub retain_gb: f64,
    /// VAAPI DRM render node for GPU transcoding (e.g. `/dev/dri/renderD129`).
    /// `None` → CPU (libx264). GPU failures fall back to CPU per-transcode.
    pub vaapi_device: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        let data_dir = PathBuf::from(env_or("HC_DATA_DIR", "./data"));
        Config {
            bind: env_or("HC_BIND", "0.0.0.0:8099"),
            data_dir,
            // Dev default; override with HC_JWT_SECRET (base64). Not for production as-is.
            jwt_secret_b64: env_or("HC_JWT_SECRET", "ZGV2LXNlY3JldC1jaGFuZ2UtbWUtaW4tcHJvZHVjdGlvbg=="),
            public_url: trim_trailing_slash(env_or("HC_PUBLIC_URL", "http://localhost:8099")),
            web_dir: PathBuf::from(env_or("HC_WEB_DIR", "./web/dist")),
            retain_days: env_or("HC_RETAIN_DAYS", "30").parse().unwrap_or(30),
            retain_max_drives: env_or("HC_RETAIN_DRIVES", "30").parse().unwrap_or(30),
            retain_gb: env_or("HC_RETAIN_GB", "100").parse().unwrap_or(100.0),
            vaapi_device: match env_or("HC_VAAPI_DEVICE", "") {
                s if s.is_empty() => None,
                s => Some(s),
            },
        }
    }

    pub fn db_url(&self) -> String {
        let path = self.data_dir.join("homeconnect.db");
        format!("sqlite://{}?mode=rwc", path.display())
    }

    pub fn blobs_dir(&self) -> PathBuf {
        self.data_dir.join("blobs")
    }

    pub fn transcode_dir(&self) -> PathBuf {
        self.data_dir.join("transcode")
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn trim_trailing_slash(s: String) -> String {
    s.trim_end_matches('/').to_string()
}
