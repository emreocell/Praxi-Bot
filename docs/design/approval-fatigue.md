# Approval fatigue: why one job asks so often, and the narrow fix

Status: first slice implemented (host-status commands). The rest is a proposal with open policy
questions. This note extends [permissions-ux.md](permissions-ux.md); read that first for the modes
(Ask / Balanced / Full), grants and the Balanced lane.

Security-sensitive: every change here sits next to the taint gate. Nothing below weakens a deny,
a floor, a redaction or the never-list.

## Symptom

Owner feedback (alpha 1, item D, and again on 2026-11-20): "one job asks for permission a million
times, even at the Full level". Two concrete cards:

1. `uptime` on the bot's own Linux host asked because "this run read: command output".
2. "This call also starts a program on this computer, so it asks every time, even when sending data
   after reads is allowed."

## Where every ask comes from

`Shared::decide_with_reason` (`crates/agent-core/src/approval.rs`) is the single decision pipeline.
In order:

| # | Rule | Source | Asks repeatedly for the same kind of action in one run? |
|---|------|--------|---------|
| 1 | Hard denies (locked policy, protected paths, external agent spawning, `pc_*` from a non-owner run) | `approval.rs` | Not an ask (deny) |
| 2 | Category level `ask` / `allow` / `deny` from the bot policy, global fallback | `resolver.rs` | Yes in Ask mode (every call); no in Balanced/Full except below |
| 3 | `AllowWithLimits` (limits not enforceable yet, fails closed) | `resolver.rs` | Yes, every call; no grant waives it |
| 4 | Money floor | `resolver.rs` | Yes, by design |
| 5 | Never-list floor (`rm -rf`, CLI sign-in stores, ...) | `never_list.rs` | Yes, by design |
| 6 | Always-ask tools (`Tool::always_ask`, `always_ask_for`: secret fills, Composio money actions, knowledge brain, bot/schedule creation) | tools | Yes, every call; no preset or grant waives it |
| 7 | **Egress gate** after untrusted (or untrusted+private) content: browse to another site, fetch, page text, send | `egress.rs`, `taint.rs` | Yes, each call, unless a "this chat" gate grant for the class and origin exists |
| 8 | **Executor gate**: any call that starts a program (`shell_run`, `script_run`, `desktop_launch`, desktop typing) after untrusted content | `egress.rs::executes_must_ask` | Yes, each call; only a chat-scoped gate grant (shell class) or a typed URL helps; not the egress waiver. The one Full-level exception is a `shell_run` of an owner-started run whose WHOLE command line is on the strict allow-list in `shell_allow.rs` (read-only tools and local build tools, paths inside the workspace; a script or project file written after an untrusted read is never waived), see `docs/guide/security.md` |
| 9 | User's-PC read, once per run and family | `approval.rs` (`pc_read_pending`) | No (once per run) |
| 10 | Policy-level ask in Balanced mode outside its lane (deletes, sends, accounts, the user's PC) | `modes.rs` | Yes, by design |

### The multiplier: shell output counts as untrusted

`builtin-tools/src/util.rs::classify` gives `shell_run` and `script_run` the class "all": the
result may ingest untrusted content, read private data, send data out and start a program. That is
right for `curl`, `cat downloads/x.html` or an arbitrary script, but it is also applied to
`uptime`. So in a run that started clean:

1. `uptime` runs (clean run, Full level, no card), and its result marks the run *untrusted* with
   the source label "command output";
2. from then on rule 8 asks before **every** shell call, and rule 7 asks before every browse to a
   new site, although nothing a stranger wrote has entered the context.

One harmless first command therefore turns the whole rest of the job into a card per call. This is
the card in `owner-1120-approval.webp`. The second quoted sentence is the executor gate (rule 8)
being shown on the egress card of such a call.

## Counted example: "check the server, browse three pages, run three status commands"

Full level, bot homed on its Linux host, one run:

| Step | Before | After this change |
|---|---|---|
| `shell_run uptime` | allowed, but taints the run ("command output") | allowed, run stays clean |
| `browser_open` page A | asks (egress after untrusted) | asks once; this is a real egress after reading, unchanged |
| `browser_open` page B, page C | one card each (a "this chat" grant per origin exists) | unchanged |
| `shell_run df -h` | asks (executor gate) | allowed |
| `shell_run free -m` | asks (executor gate) | allowed |
| `shell_run uptime` again | asks (executor gate) | allowed |

The regression tests in `crates/agent-core/src/mode_tests.rs` replay the shell half and count
cards: three status commands after a page was read produce **3 cards before, 0 after**; the run
`uptime` then `ls` produces 1 card before (the `ls`), 0 after. Cards for everything that is not a
plain status command are unchanged (see "Hard limits" below).

## What this change does (slice 1)

`permission_engine::is_host_status_command` recognises one narrow shape: exactly one of
`uptime whoami hostname uname date df free nproc id`, with only short flags from a per-command
letter list (`uptime -p`, `df -h`, `free -m`, `uname -a`). Characters other than letters, digits,
space and `-` disqualify the command, so there is no path, quote, variable, substitution, pipe,
redirect or second command. Flags that take a value (`date -s`, `free -s`, `df -t`, `hostname -F`)
are not in the lists.

`Shared::is_host_status_call` applies it only when:

* the tool is `shell_run`, and its only fields are `command`, `timeout` and an `sh` / `bash`
  `shell` (no `cwd`, no PowerShell, nothing else);
* the daemon is an always-on host and the run resolved to the bot's own computer (the host or its
  isolated browser), never the user's PC.

For such a call the content class is "none": it does not count as an executor, does not egress,
and its output does not mark the run untrusted or private (`ExecFact` is recorded from the same
decision, never from the output). The decision is made from the input alone.

What it does **not** change: the category level (Ask mode still asks for `uptime`, Balanced and Full
still allow it), the never-list, money, always-ask tools, PC scoping, every other shell command.

## Hard limits (kept, with tests)

A command is never in the class, and keeps the executor and egress gates, when it: names a path or
URL, chains or pipes, redirects, uses substitution, uses an unlisted flag, sets a value, runs from
a chosen `cwd`, uses another shell, or runs on the user's PC. Tests: `uptime; curl ...`,
`uptime | nc ...`, `df -h /etc`, `cat ~/.ssh/id_rsa`, `ls`, a `cwd`, `shell: powershell`, and
`uptime` on a non-host daemon still ask in a tainted Full run.

## Options considered

| Option | Verdict |
|---|---|
| Treat all output of shell on the host as clean | Rejected. `cat downloads/page.html`, `curl`, `git log` from a cloned repo all carry outsider text. Command text cannot tell them apart in general. |
| Read-only command allow-class on the host (`ls`, `cat`, `ps`, `git status`) | Rejected for now. These read files or process lists that can hold attacker-written text (file names, command lines). Only fixed status commands with no path argument are safe to call clean. |
| Status commands only (this slice) | **Implemented.** Fixed output shape, no free arguments, nothing to steer. |
| Target-aware policy: everything on the bot's own host allowed after untrusted content | Rejected. The host is disposable, but it holds the bot's API keys, vault access and network; "execute what the page asked" is the prompt-injection case the executor gate exists for. |
| "Grant for this chat" per tool + target + argument class | Largely exists (gate grants for web origin, page origin, shell class; scoped command-prefix grants). Gap: the shell class grant is all shell, not a command family. Proposal below, not implemented. |
| Ask once per run when the first approval already showed the plan | Rejected as a general rule: a later call can differ from what the card showed. Fine only for exact-repeat calls, which already collapse. |
| Make Full skip the gates | Rejected: Full's contract is "no prompts inside the grants; only hard stops remain", and the taint gate is a hard stop. Changing that is a policy question for the owner. |

## Open policy questions for the owner

1. After untrusted content, should a *command-family* chat grant exist for read-only shell commands
   on the bot's own host (`ls`, `cat` inside the workspace, `git status`)? It would collapse the
   remaining shell cards of a coding job, at the cost of output that may carry outsider text
   becoming allowed without a card.
2. Should Full on a host let a tainted run run any shell command once the owner said "yes, for this
   chat" to the shell class (today possible), or should that grant be narrowed to the command the
   card showed plus its exact repeats?
3. Is a clean-run first `browser_open` that reads a page worth a card in Full (today it does not;
   the second site does)? Showing the list of sites a job plans to visit up front would turn N cards
   into 1 but needs a plan step the agent loop does not have.
4. Should `desktop_launch` on the bot's own host (a browser start) be offered a chat grant? Today it
   is once-only by design.
