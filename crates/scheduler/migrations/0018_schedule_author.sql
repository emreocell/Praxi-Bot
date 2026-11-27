-- Who wrote the definition, as far as the daemon can prove: the owner (the local app or the
-- owner's administrator desktop), a bot (its schedule tools), or unknown. Existing rows are
-- NOT assumed to be the owner's: `origin = 'user'` only says the row did not come from a bot's
-- tools, and a paired phone or an older build could have written it. The owner confirms an
-- unknown row with one click. Only an owner-authored schedule of a Full bot runs in the Full
-- lane without cards.
ALTER TABLE schedules ADD COLUMN author TEXT NOT NULL DEFAULT 'unknown' CHECK (author IN ('owner', 'bot', 'unknown'));
UPDATE schedules SET author = 'bot' WHERE origin = 'bot';
