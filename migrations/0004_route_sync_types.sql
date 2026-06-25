-- Per-route override of which data types sync. NULL = inherit the global default
-- (settings `sync_types`); a value (comma-separated, possibly empty for "qlog
-- only") is an explicit per-drive choice, e.g. set when the user deletes data so
-- it isn't pulled again.
ALTER TABLE routes ADD COLUMN sync_types TEXT;
