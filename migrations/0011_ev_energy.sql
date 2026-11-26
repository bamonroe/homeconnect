-- EV energy per drive, from the rlog's undecoded bus-1 CAN (see parse.rs,
-- "EV battery / motor telemetry"; provisional scales). Per segment, summed onto
-- the route. `ev_seconds` = seconds of power data integrated, `ev_meters` =
-- distance over those segments from carState.vEgo. Run `reparse-rlog` to backfill.
ALTER TABLE segments ADD COLUMN ev_kwh_out   REAL NOT NULL DEFAULT 0;
ALTER TABLE segments ADD COLUMN ev_kwh_regen REAL NOT NULL DEFAULT 0;
ALTER TABLE segments ADD COLUMN ev_seconds   REAL NOT NULL DEFAULT 0;
ALTER TABLE segments ADD COLUMN ev_meters    REAL NOT NULL DEFAULT 0;

ALTER TABLE routes ADD COLUMN ev_kwh_out   REAL NOT NULL DEFAULT 0;
ALTER TABLE routes ADD COLUMN ev_kwh_regen REAL NOT NULL DEFAULT 0;
ALTER TABLE routes ADD COLUMN ev_seconds   REAL NOT NULL DEFAULT 0;
ALTER TABLE routes ADD COLUMN ev_meters    REAL NOT NULL DEFAULT 0;
