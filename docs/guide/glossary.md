# Glossary

Alphabetical reference of core terms used across Praxi, the user guide, and the codebase.

- **Approval** — A confirmation card shown when a bot wants to perform an action that requires permission. The user approves or denies each one. See [Chatting and approvals](chatting-and-approvals.md). Code: `crates/permission-engine/`.
- **Bot** — A named assistant with its own persona, model, tool set and permission rules. Each bot has its own chats and grants. See [Bots and profiles](bots-and-profiles.md).
- **Daemon** — The background process (`bot serve`) that runs the agent loop, stores data in SQLite, enforces permissions and fires schedules. It listens only on `127.0.0.1`. See [CLI reference](cli-reference.md).
- **Egress gate** — The rule that requires approval before a run that has read untrusted content can send data out of the machine. See [Security model](security.md). Code: `crates/permission-engine/src/egress.rs`.
- **Host** — An always-on Linux server running the daemon in host mode, so bots keep working when the desktop is off. See [Running on a remote host](remote-host.md).
- **Never-list** — A hard-coded list of catastrophic shell commands (formatting disks, deleting system directories) that always require approval, regardless of policy. See [Security model](security.md). Code: `crates/permission-engine/src/never_list.rs`.
- **Node-agent** — An experimental helper (`bot-node`) that lets a bot control another machine. See [FAQ](faq.md). Code: `apps/node-agent/`.
- **Permission preset** — A named set of default permission rules for a bot: `read-only`, `read-and-draft`, `approve-destructive` (the default) and `full`. See [Chatting and approvals](chatting-and-approvals.md). Code: `apps/cli/src/presets.rs`, `crates/permission-engine/src/modes.rs`.
- **Run** — A single turn of work: the bot receives a message, thinks, calls tools and streams its answer. Also called a turn. See [Chatting and approvals](chatting-and-approvals.md).
- **Schedule** — A timed trigger that makes a bot start a run on its own (e.g. every weekday at 9). See [Scheduled tasks](scheduled-tasks.md).
- **Skill** — A short Markdown playbook that teaches a bot how to handle a tricky task, page or app. Not code — instructions the bot reads when relevant. See [Skills](skills.md).
- **Workflow** — A saved, parameterised sequence of tool calls that replays without model calls. See [Workflows](workflows.md).
