-- Device's last-seen network address (tailnet IP from the athena connection),
-- used as the SSH target for device management.
ALTER TABLE devices ADD COLUMN last_addr TEXT NOT NULL DEFAULT '';
