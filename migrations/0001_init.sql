-- homeconnect schema (SQLite). Times are unix-millis unless noted.

CREATE TABLE IF NOT EXISTS users (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    identity      TEXT NOT NULL UNIQUE,           -- uuid v4
    username      TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,                  -- argon2
    email         TEXT,
    is_admin      INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS devices (
    dongle_id        TEXT PRIMARY KEY,
    serial           TEXT NOT NULL DEFAULT '',
    imei             TEXT NOT NULL DEFAULT '',
    imei2            TEXT NOT NULL DEFAULT '',
    public_key       TEXT NOT NULL,               -- PEM (RSA or EC)
    owner_id         INTEGER REFERENCES users(id),
    online           INTEGER NOT NULL DEFAULT 0,
    last_athena_ping INTEGER NOT NULL DEFAULT 0,  -- unix seconds (comma convention)
    uploads_allowed  INTEGER NOT NULL DEFAULT 1,
    alias            TEXT NOT NULL DEFAULT '',
    device_type      TEXT NOT NULL DEFAULT '',
    server_storage   INTEGER NOT NULL DEFAULT 0,  -- bytes
    last_gps_lat     REAL NOT NULL DEFAULT 0,
    last_gps_lng     REAL NOT NULL DEFAULT 0,
    last_gps_time    INTEGER NOT NULL DEFAULT 0,
    created_at       INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS routes (
    fullname              TEXT PRIMARY KEY,        -- '{dongle}|{timestamp}'
    device_dongle_id      TEXT NOT NULL REFERENCES devices(dongle_id),
    start_time_utc_millis INTEGER NOT NULL DEFAULT 0,
    end_time_utc_millis   INTEGER NOT NULL DEFAULT 0,
    start_lat             REAL NOT NULL DEFAULT 0,
    start_lng             REAL NOT NULL DEFAULT 0,
    end_lat               REAL NOT NULL DEFAULT 0,
    end_lng               REAL NOT NULL DEFAULT 0,
    length                REAL NOT NULL DEFAULT 0, -- miles
    segment_numbers       TEXT NOT NULL DEFAULT '[]',
    segment_start_times   TEXT NOT NULL DEFAULT '[]',
    segment_end_times     TEXT NOT NULL DEFAULT '[]',
    maxcamera             INTEGER NOT NULL DEFAULT -1,
    maxdcamera            INTEGER NOT NULL DEFAULT -1,
    maxecamera            INTEGER NOT NULL DEFAULT -1,
    maxqcamera            INTEGER NOT NULL DEFAULT -1,
    maxlog                INTEGER NOT NULL DEFAULT -1,
    maxqlog               INTEGER NOT NULL DEFAULT -1,
    platform              TEXT NOT NULL DEFAULT '',
    git_remote            TEXT NOT NULL DEFAULT '',
    git_branch            TEXT NOT NULL DEFAULT '',
    git_commit            TEXT NOT NULL DEFAULT '',
    is_public             INTEGER NOT NULL DEFAULT 0,
    created_at            INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_routes_device_start
    ON routes(device_dongle_id, start_time_utc_millis DESC);

CREATE TABLE IF NOT EXISTS segments (
    canonical_name        TEXT PRIMARY KEY,        -- '{dongle}|{ts}--{n}'
    canonical_route_name  TEXT NOT NULL REFERENCES routes(fullname),
    number                INTEGER NOT NULL,
    qcam_url              TEXT NOT NULL DEFAULT '',
    fcam_url              TEXT NOT NULL DEFAULT '',
    dcam_url              TEXT NOT NULL DEFAULT '',
    ecam_url              TEXT NOT NULL DEFAULT '',
    qlog_url              TEXT NOT NULL DEFAULT '',
    rlog_url              TEXT NOT NULL DEFAULT '',
    qcam_duration         REAL NOT NULL DEFAULT 0,
    start_lat             REAL NOT NULL DEFAULT 0,
    start_lng             REAL NOT NULL DEFAULT 0,
    end_lat               REAL NOT NULL DEFAULT 0,
    end_lng               REAL NOT NULL DEFAULT 0,
    miles                 REAL NOT NULL DEFAULT 0,
    start_time_utc_millis INTEGER NOT NULL DEFAULT 0,
    end_time_utc_millis   INTEGER NOT NULL DEFAULT 0,
    created_at            INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_segments_route ON segments(canonical_route_name);

CREATE TABLE IF NOT EXISTS authorized_users (
    user_id          INTEGER NOT NULL REFERENCES users(id),
    device_dongle_id TEXT NOT NULL REFERENCES devices(dongle_id),
    access_level     TEXT NOT NULL DEFAULT 'read',
    created_at       INTEGER NOT NULL,
    PRIMARY KEY (user_id, device_dongle_id)
);
