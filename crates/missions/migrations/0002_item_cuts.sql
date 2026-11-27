-- Missions: sessions the step guard cut while the item was still being worked on.
-- A cut is not a failed attempt (the item may simply need more than one session), so it is
-- counted apart from `attempts` and capped on its own (`MAX_ITEM_CUTS`). The owner's answer
-- resets it.
ALTER TABLE mission_items ADD COLUMN cuts INTEGER NOT NULL DEFAULT 0;
