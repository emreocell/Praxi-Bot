# FAQ

### Is Praxi ready to use?
It is in early development. The README says there is no stable API and no
releases, and things will break. It is meant for trying out and building on.

### Does anything leave my machine?
Prompts and tool results go to the model provider you configured, and Telegram
messages go through Telegram if you enable it. There is no telemetry. With a
local model (Ollama, LM Studio) and no Telegram, nothing leaves your machine.

### Does it cost money?
Praxi itself is free and open source (Apache-2.0). You pay your model provider,
or nothing on a free tier or with local models. See
[Providers and models](providers-and-models.md). The API reports token usage
and, where the catalog has prices, estimated cost (`/v1/usage`; the desktop app
has a usage screen).

### Which operating systems work?
The CLI and daemon build on Windows, Linux and macOS. Desktop control (clicking
and typing in other apps) is implemented for Windows only; the Linux and macOS
backends are not usable yet. The desktop app and installer are Windows. Browser
tools drive a Chromium-based browser through the DevTools protocol and work
where such a browser is available.

### Can I use it without a terminal?
Yes on Windows: install the desktop app, follow the wizard, chat, approve and
schedule from the interface.

### Can the bot do anything without asking me?
Only what the policy allows. By default reading is free and changes ask. You
can widen this per bot and per category. Money and catastrophic shell commands
always ask. See [Chatting and approvals](chatting-and-approvals.md).

### How do I stop a bot right now?
Send `/stop` in the chat or Telegram, press the global hotkey (`Ctrl+Alt+Esc` by
default, desktop app), take control in the desktop app, or stop the daemon
(Ctrl-C), which pauses live runs. `POST /v1/runs/{id}/cancel` cancels one run.

### Can I run several bots?
Yes. Each has its own profile, chats and grants; see
[Bots and profiles](bots-and-profiles.md).

### Where is my data, and how do I back it up?
In the data directory (`--data-dir` / `BOT_DATA_DIR`, otherwise the OS per-user
app-data folder for `praxi-bot`): a SQLite database plus event logs,
`providers.toml`, skills, workflows and the API token. Stop the daemon and copy
the folder. API keys are in your OS keychain (or an encrypted file in the data
directory) and are not part of a plain copy when the keychain is used.

### How do I make a bot faster and cheaper?
Use a small `classifier` model, save [workflows](workflows.md) for repeated jobs,
press **Remember this** after a run that went well, and write
[skills](skills.md) for sites and apps the bot struggles with.

### Can I record a workflow by demonstrating it?
Not yet (**Planned**). Today workflows are TOML files you write, and "Remember"
caches the route of a successful run. See [Workflows](workflows.md).

### Does it have long-term memory?
There is a memory library (recall before a turn, extraction after) in the
repository, but the daemon does not enable it yet (**Planned**). Chats keep
their own history.

### Can I add MCP servers or plugins?
MCP support exists as a library and is not wired into the daemon yet
(**Planned**); see [MCP servers](mcp-servers.md). The design calls for plugins
for tools, providers and channels; there is no plugin API for end users yet.

### Can one bot control another computer?
Not yet in the product. The repository contains a node-agent (`bot-node`) with
pairing (`join`, `run`, `devices`) and the peer network code, intended so that a
machine you own can be controlled by your core without inbound ports. The core
side is not connected to the daemon yet, so treat it as **experimental**.

### Which other messaging apps are supported?
Telegram only. Others, such as email or chat services, are in the design
documents (**Planned**).

### Is there voice, image or video generation?
No. These are on the design roadmap only (**Planned**).

### Can I use Turkish?
Yes. The wizard offers English and Turkish, the desktop app has both, and the
schedule parser understands Turkish phrases such as `her gün 09:00`. The model
answers in whatever language you write to it, and a profile's `language` can set
a preference.

### How do I update?
Rebuild from source or install a newer installer; your data directory is kept.
Because the project is changing quickly, back up the data directory first.

### How do I uninstall?
Windows: use the uninstaller (it keeps your data unless you tick **Delete
application data**). CLI: delete the binary, and if you want, the data directory
and the `provider.*` and Telegram entries in your keychain (`bot secrets delete`).

### Where do I report bugs or contribute?
See [CONTRIBUTING.md](../../CONTRIBUTING.md).
