-- Device health captured from DeviceState during a drive: peak temperature,
-- lowest free disk %, peak memory %. Per segment, aggregated onto the route.
-- `min_free` is -1 when unknown (no DeviceState health seen). Run `reparse`.
ALTER TABLE segments ADD COLUMN max_temp REAL    NOT NULL DEFAULT 0;
ALTER TABLE segments ADD COLUMN min_free REAL    NOT NULL DEFAULT -1;
ALTER TABLE segments ADD COLUMN max_mem  INTEGER NOT NULL DEFAULT 0;

ALTER TABLE routes ADD COLUMN max_temp REAL    NOT NULL DEFAULT 0;
ALTER TABLE routes ADD COLUMN min_free REAL    NOT NULL DEFAULT -1;
ALTER TABLE routes ADD COLUMN max_mem  INTEGER NOT NULL DEFAULT 0;
