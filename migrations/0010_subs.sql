-- Per-drive subtitle artifacts: one WebVTT transcript per route, produced by
-- sending the drive's stitched qcamera audio to a whisper.cpp server. Freshness
-- is tracked the same way movies are (segment coverage when built), and the VTT
-- itself is a route-level blob (`{dongle}_{ts}--subs--en.vtt`).
CREATE TABLE IF NOT EXISTS subs (
  fullname   TEXT    NOT NULL PRIMARY KEY, -- "{dongle}|{ts}"
  seg_count  INTEGER NOT NULL,             -- audio segments covered when built
  cues       INTEGER NOT NULL DEFAULT 0,   -- number of subtitle cues found
  bytes      INTEGER NOT NULL DEFAULT 0,   -- 0 also marks a failed/empty attempt
  built_at   INTEGER NOT NULL DEFAULT 0,
  disabled   INTEGER NOT NULL DEFAULT 0    -- user-deleted: never auto-rebuild
);
