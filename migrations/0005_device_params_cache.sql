-- Local cache of device (openpilot) param values, so the Device page is instant
-- and edits work offline. `value` is the desired/last-known value shown in the UI;
-- `pending = 1` means the user changed it and it hasn't been written to the device
-- yet (it's flushed over SSH when the device is next online).
CREATE TABLE IF NOT EXISTS device_params (
  dongle_id  TEXT    NOT NULL,
  key        TEXT    NOT NULL,
  value      TEXT    NOT NULL,
  pending    INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (dongle_id, key)
);
