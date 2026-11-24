# Permissions UX: autonomy modes and approval memory

Status: implemented in three steps (engine modes, approval memory, UI). This note records the
current model, why a "Full" bot could still prompt a lot, and the model that replaces it.

## The model before this change

Every tool call goes through `Shared::decide_with_reason` (`crates/agent-core/src/approval.rs`):

1. Hard denies: locked policy, daemon-protected paths, external agents spawning sub-agents.
2. `PermissionResolver::resolve` (`crates/permission-engine`): per-category level
   (`deny` / `ask` / `allow`) from the bot's policy, falling back to the global policy, then the
   floors: **Money** is never below ask, a **never-list** command is never below ask.
3. Always-ask tools (`Tool::always_ask`, `always_ask_for`): a secret fill, every Composio app
   action, the knowledge brain tool. No preset and no grant waives these.
4. The **taint gate** (`ContentExposure`, `egress.rs`): once a run read untrusted content, a call
   that can send data out (any fetch, a navigation to another site, a page action with text, a
   send) asks, and so does any call that starts a program (shell, script, desktop launch). Only the
   per-bot `allow_egress_after_reads` flag skips the egress half; nothing but a chat-scoped
   "gate grant" skips the executor half.
5. Waivers of a *plain policy-level ask*: a "this chat" or "always" grant, a run-scoped control
   grant, and the read shortcut (`read_tools_skip_ask` for `Effect::Read`).

A bot's stored permission is a preset (`read-only`, `approve-destructive`, `read-and-draft`,
`full`) or custom per-category levels. The UI shows three "autonomy" pills: Ask, Balanced, Auto,
mapped to `read-and-draft`, `approve-destructive`, `full`.

## Why a Full bot still prompted

* **The taint gate is the main source.** A bot that browses reads untrusted content on its first
  page, so afterwards every navigation to a new site, every fetch, and every shell command asks,
  on every call. Grants did not help: a gate card only remembered "this chat" for a tool class and
  one origin, and the shell class asked again in the next chat.
* **Always-ask tools.** Each Composio action asks every time, whatever the preset or grants.
* **Ask and Balanced were the same thing.** `read-and-draft` and `approve-destructive` both set
  `ask` on every category, so the "Balanced" pill asked before every file edit and every page
  click.
* **"Always allow" was too wide.** A persisted grant was a whole tool (every shell command) or a
  whole category. Because that is risky, the card offered it rarely, so people kept clicking once.
* The same approval could be pending twice (parallel identical calls), and a run needing several
  approvals showed them as separate cards.

## The model after this change

### Modes (per bot)

| Mode | Meaning | Stored as |
|---|---|---|
| Ask | Confirm anything with a side effect | preset `read-and-draft` (and unknown/custom) |
| Balanced | Reads, browsing, workspace edits and work on the bot's own computer run freely; outward-facing, destructive, financial and credential actions ask | preset `approve-destructive` |
| Full | No prompts inside the grants; only hard stops remain | preset `full` |

The mode is derived from the preset (`AutonomyMode::from_preset`), so profiles and the API do not
change. Balanced is a lane (`permission_engine::modes::balanced_lane`) over a *plain policy-level
ask*, evaluated after every deny, floor, always-ask check, the taint gate and unevaluated limits.
It can only turn such an ask into an allow, never override any of them. It covers:

* file edits (`Effect::Write` in the `fs` category; the file tools keep enforcing their roots),
* browsing and page work in an isolated browser profile (not a named, possibly signed-in one),
* shell, script and desktop calls when the daemon is an always-on host and the run resolved to the
  bot's own computer (never the user's PC).

Everything else asks: file deletes, messaging, accounts, money, system, remote, anything on the
user's PC. Only the user's own runs (typed or scheduled) get the lane. Each lane allow writes an
`approval.auto_allowed` audit entry with a reason code (`mode.balanced.workspace_edit`,
`mode.balanced.browse`, `mode.balanced.own_computer`).

### Hard stops (all modes, never remembered)

Money, never-list commands, secret fills, untrusted-content-then-execute, the egress gate when the
run read untrusted content (unless the user remembered that exact target, below), and anything an
external agent starts. These keep their current behaviour and tests.

### Approval memory (step 2)

Approving offers *Allow once*, *Allow for this chat* and *Always allow this for this bot*. A
remembered rule is scoped by tool and target, never wider than what the card showed:

| Tool family | Target | Match |
|---|---|---|
| web navigation | site origin (`scheme://host:port`) | exact origin |
| file tools | folder | the path is inside the folder |
| shell | command prefix | whole-word prefix, only for commands without shell operators |

A call whose target cannot be derived (for example a shell command with `;` or `|`) offers *once*
only. A rule never covers a hard-stop class and never another target. Rules are persisted per bot
in the audit log (like existing grants), listed in the bot's Permissions page, and revocable.

Standing choices (this chat, always) need the owner's authority, as gate grants already do. A
remembered site grant ("allow this site for this chat", exact scheme + host + port) also satisfies
the egress gate for a `browser_open` to any path of that origin, because the owner granted the
whole site by clicking it. Without that grant, a plain navigation (one argument, `url`, no query,
fragment, userinfo or explicit port) is silent after untrusted content only when its path is
exactly `/` or empty AND the origin is the current page's origin, an origin the owner named in
this chat (a full http(s) URL, or a bare domain under a real public TLD that is not a file
extension, so `README.md` is not a site) or one approved once. A root URL carries nothing but the
host, so no value copied from a page can ride in it. Any non-empty path asks (no verbatim-word or
shape rule: those leak), and a one-time approval never widens to other paths. Residual risk: the
host name itself (owner-named) is the only data channel. A redirect to another origin is not
remembered: opening that origin later asks. A remembered command never satisfies the
"untrusted content, then run a program" gate. Risky programs (shells, interpreters, `rm`, `curl`,
`ssh`, `sudo`, ...) and commands with arguments other than flags and one sub-command word are
never remembered. The old tool-wide and category-wide rules keep working for other tools.

### Batching and quiet notifications (step 3)

Identical pending approvals of one run are shown once; several different approvals of one run are
shown in one card with a toggle per item. What ran under auto-approval appears as compact tool
chips in the chat, not as prompts.

## Not covered

* Composio actions stay always-ask (their approval token is created by the card; remembering them
  needs a tool-side change).
* CLI engines in native-tools mode keep their own approval levels (`cli-provider` `levels.rs`).
