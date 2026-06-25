-- A user can delete a drive's encoded movie. Without a marker the background
-- sweep would just rebuild it; `disabled` records that choice so it stays gone
-- until the user explicitly rebuilds (which clears the row).
ALTER TABLE movies ADD COLUMN disabled INTEGER NOT NULL DEFAULT 0;
