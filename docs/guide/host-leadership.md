# Host leadership limits

Demotion closes automatic admission and stops the scheduler and channels. Runs already admitted (scheduled, webhook, Telegram or chat) continue and can still execute approved tools. This card does not pause or transfer them. Before a planned handover, quiesce those runs; task 057 must implement the durable pause/transfer/resume protocol. Heartbeat demotion has the same in-flight limitation. Permission checks remain in force.

A persisted active owner with paired desktop/host devices waits up to ten seconds for heartbeat metadata before starting components. During verification, automatic admission is closed. Higher epochs demote it and equal active epochs stop both. If peers remain unreachable at the deadline, the persisted owner starts: a partition longer than ten seconds can still cause concurrent owners. There is no automatic takeover or distributed lease.

Peer epochs more than 4096 above the local epoch, or above 1,000,000,000, are ignored. Explicit forced activation can recover legacy or ceiling epochs by saturating at that ceiling. Two active owners at the ceiling still stop on equal-epoch evidence; operator coordination is required.

Ownership, invite, revoke and update actions record the actor and old/new ownership in SQLite before acting. Rows describe requested actions, including requests whose later I/O fails; they do not imply successful completion. Pairing tickets and codes are never audited.

Forced activation uses the last local data. Scheduler startup applies misfire policy; run_once/run_all may repeat effects the other owner already completed. The CLI warns about both missing data and duplicate actions. Non-forced activation of an already active owner is idempotent.

The API currently returns no install_command: install-linux.sh is Bash and does not yet implement --join. Create a host invite, install separately and use bot host join until packaging supports an end-to-end command. Telegram channel status remains conflict after bounded retries and is exposed through the channels API until an explicit restart.
