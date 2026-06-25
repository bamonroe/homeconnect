-- Per-drive "movie" artifacts: one stitched, audio-muxed H.264 MP4 per (route,
-- camera), built in the background. This table tracks freshness so the sweep
-- rebuilds only when a drive's segment coverage for that camera changes. The MP4
-- itself is a route-level blob (`{dongle}_{ts}--movie--{cam}.mp4`).
CREATE TABLE IF NOT EXISTS movies (
  fullname   TEXT    NOT NULL,          -- "{dongle}|{ts}"
  cam        TEXT    NOT NULL,          -- qcamera | fcamera | dcamera | ecamera
  seg_count  INTEGER NOT NULL,          -- segments covered when built (freshness)
  bytes      INTEGER NOT NULL DEFAULT 0,
  duration   REAL    NOT NULL DEFAULT 0,
  built_at   INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (fullname, cam)
);
