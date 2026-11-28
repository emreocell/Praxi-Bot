-- Indexes for per-run / per-call hot paths that used to scan whole tables:
--  * messages by run (`run_user_message_sources`, asked on every permission decision),
--  * audit rows by run and action (the end-of-run usage summary),
--  * audit rows by user, action and time (spend-limit checks before every model call).
CREATE INDEX idx_messages_run ON messages(run_id, role, created_at_ms);
CREATE INDEX idx_audit_run ON audit_log(run_id, action, user_id);
CREATE INDEX idx_audit_user_action ON audit_log(user_id, action, ts_ms);
