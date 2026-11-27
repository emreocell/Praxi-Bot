-- Memories a bot saved itself in an unattended run (schedule, mission, sub-agent).
--
-- `origin_json` is NULL for everything the owner said (quoted, extracted from a chat or typed in
-- the panel). Otherwise it holds the provenance: the run, the kind of run, the schedule or mission
-- id and whether the run held untrusted content. Such a memory is recalled in its own section,
-- with that provenance, and never as something the owner said.

ALTER TABLE memories ADD COLUMN origin_json TEXT;
