ALTER TABLE runs ADD COLUMN computer_node TEXT;
ALTER TABLE runs ADD COLUMN computer_node_name TEXT;
ALTER TABLE runs ADD COLUMN paused_at_ms INTEGER;
UPDATE runs SET paused_at_ms = updated_at_ms WHERE state = 'paused';
