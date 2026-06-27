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
    /// SSH-pull sync: fetch drives off the device over SSH (the device's uploader
    /// can't be repointed at us — see CLAUDE.md). Master switch; the primary
    /// trigger is the device's athena connection (event-driven).
    pub sync_enabled: bool,
    /// Periodic sync interval (seconds) — each tick pulls from any device that's
    /// currently online (cheap: a no-op `find` when nothing's new). Complements
    /// the connect trigger; covers a continuously-connected device. 0 = disable
    /// the loop (connect trigger only).
    pub sync_interval_secs: u64,
    /// Whether the background pass also pulls full-res cameras + rlog (14G+);
    /// off by default — full-res is pulled on demand per route.
    pub sync_fullres: bool,
    /// After a file is safely pulled + stored, delete the device's copy to reclaim
    /// its space. Off by default (the device rotates its own storage); a runtime
    /// toggle seeds from this. Only ever deletes files we already hold.
    pub device_autoprune: bool,
    /// Background movie encoding (stitched per-drive MP4s): master switch + sweep
    /// interval (seconds). Both have runtime toggles in Settings that seed from these.
    pub movie_enabled: bool,
    pub movie_interval_secs: u64,
    /// Coordination/login server for the `--tailscale` option of onboard.sh
    /// (e.g. a self-hosted headscale URL). Empty = Tailscale's default coordination
    /// server. Not a secret (a public hostname); the per-device authkey is passed
    /// as a runtime flag, never baked into the script.
    pub tailnet_login_server: String,
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
            sync_enabled: env_or("HC_SYNC_ENABLED", "true").parse().unwrap_or(true),
            sync_interval_secs: env_or("HC_SYNC_INTERVAL_SECS", "60").parse().unwrap_or(60),
            sync_fullres: env_or("HC_SYNC_FULLRES", "false").parse().unwrap_or(false),
            device_autoprune: env_or("HC_DEVICE_AUTOPRUNE", "false").parse().unwrap_or(false),
            movie_enabled: env_or("HC_MOVIE_ENABLED", "true").parse().unwrap_or(true),
            movie_interval_secs: env_or("HC_MOVIE_INTERVAL_SECS", "120").parse().unwrap_or(120),
            tailnet_login_server: trim_trailing_slash(env_or("HC_TAILNET_LOGIN_SERVER", "")),
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

    /// Directory holding homeconnect's device SSH keypair.
    pub fn ssh_dir(&self) -> PathBuf {
        self.data_dir.join("ssh")
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn trim_trailing_slash(s: String) -> String {
    s.trim_end_matches('/').to_string()
}
