-- Who created a schedule: the user (UI / CLI / API) or a bot through its
-- schedule tools. Existing rows were all made by the user.
ALTER TABLE schedules ADD COLUMN origin TEXT NOT NULL DEFAULT 'user' CHECK (origin IN ('user', 'bot'));
