-- Runs a standing order started per local day: the daily cost cap. One row per schedule and
-- day; rows of earlier days are dropped when a later day is first counted.
CREATE TABLE standing_order_days (
    schedule_id TEXT NOT NULL REFERENCES schedules(id) ON DELETE CASCADE,
    day TEXT NOT NULL,
    runs INTEGER NOT NULL CHECK (runs >= 0),
    PRIMARY KEY (schedule_id, day)
);
