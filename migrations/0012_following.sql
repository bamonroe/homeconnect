-- Following distance per drive, from the qlog's radarState (lead car) and
-- carState speed (see parse.rs `telemetry_stats`). Only counted while moving at
-- FOLLOW_MIN_MS or faster with a lead tracked. The gap is the time headway in
-- seconds (distance / own speed). `*_op_*` = while openpilot's longitudinal
-- control was on (it sets the following distance). `close_*` = time under
-- CLOSE_GAP_S. `min_gap` = closest headway, -1 when never following.
-- Run `reparse` to backfill.
ALTER TABLE segments ADD COLUMN follow_seconds    REAL NOT NULL DEFAULT 0;
ALTER TABLE segments ADD COLUMN follow_gap_sum    REAL NOT NULL DEFAULT 0;
ALTER TABLE segments ADD COLUMN close_seconds     REAL NOT NULL DEFAULT 0;
ALTER TABLE segments ADD COLUMN follow_op_seconds REAL NOT NULL DEFAULT 0;
ALTER TABLE segments ADD COLUMN follow_op_gap_sum REAL NOT NULL DEFAULT 0;
ALTER TABLE segments ADD COLUMN close_op_seconds  REAL NOT NULL DEFAULT 0;
ALTER TABLE segments ADD COLUMN min_gap           REAL NOT NULL DEFAULT -1;

ALTER TABLE routes ADD COLUMN follow_seconds    REAL NOT NULL DEFAULT 0;
ALTER TABLE routes ADD COLUMN follow_gap_sum    REAL NOT NULL DEFAULT 0;
ALTER TABLE routes ADD COLUMN close_seconds     REAL NOT NULL DEFAULT 0;
ALTER TABLE routes ADD COLUMN follow_op_seconds REAL NOT NULL DEFAULT 0;
ALTER TABLE routes ADD COLUMN follow_op_gap_sum REAL NOT NULL DEFAULT 0;
ALTER TABLE routes ADD COLUMN close_op_seconds  REAL NOT NULL DEFAULT 0;
ALTER TABLE routes ADD COLUMN min_gap           REAL NOT NULL DEFAULT -1;
