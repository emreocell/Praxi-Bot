# Security policy

Praxi-Bot can read and change files, run commands, drive a browser and operate desktop apps on
your behalf, so security reports are taken seriously.

## Reporting a vulnerability

**Please do not open a public issue, pull request or discussion for a vulnerability.**

Report it privately through GitHub: open the repository's **Security** tab and choose
**Report a vulnerability** (a private security advisory). Include:

- what you found and which component it is in (daemon, desktop app, host, a crate);
- the version or commit, your operating system, and how Praxi was started;
- steps to reproduce, ideally a minimal proof of concept;
- what an attacker gains (for example: a bot gets a permission it should not have).

Please give us a reasonable time to fix the problem before you publish details. We will confirm
receipt, tell you whether we agree it is a vulnerability, keep you informed, and credit you in the
advisory if you want.

## Supported versions

Praxi-Bot is **alpha**. Only the **latest pre-release** (and `main`) receive security fixes.
Older pre-releases are not patched; update instead.

## In scope

- Bypassing the permission system: running a tool without the approval its policy requires, or a
  subagent/script/workflow doing something its parent could not.
- Defeating the egress gate, the never-list, protected paths, or secret redaction in a way that
  leaks data or credentials.
- Reaching owner-only or host-administration routes without the owner key or the paired desktop's
  administration right (for example from a bot, a plain API token, a phone, or another device).
- Reading or forging the daemon's API token, the owner key, pairing keys or stored secrets.
- Prompt-injection paths that cause an action to run **without** the approval the design promises.
- Remote code execution, path traversal, SSRF and similar bugs in the daemon, the desktop app, the
  host installer or the update path.

## Out of scope

- A bot the owner has deliberately given an unrestricted shell as the same operating-system user
  can do what that user can do; the never-list and approval prompts there are damping, not a
  sandbox (this is documented).
- Findings that need an attacker who already controls your account, your machine or your API keys.
- Missing hardening that is documented as planned, denial of service by a user against their own
  machine, and reports from automated scanners without a demonstrated impact.
- Vulnerabilities in third-party services or in the model providers you configure.

## The security model in ten lines

1. Permissions are decided by code, never by the model; every tool call - also from scripts and
   workflows - passes the same gate, and subagents can never be more permissive than their parent.
2. By default anything that changes something (files, desktop, commands, messages) asks first.
3. Money actions and a parsed never-list of catastrophic shell commands always ask, at every level.
4. After a run has read untrusted or private content, anything that can send data off the machine
   asks (the egress gate), even with a standing grant.
5. The daemon listens on `127.0.0.1` only, checks the `Host` and `Origin` headers, and needs a bearer
   token (256 random bits, readable only by your user).
6. Routes that change authority, secrets, providers or keys also need a separate owner key; bots and
   plain API tokens cannot use them.
7. Secrets live in the OS keychain or an encrypted file, never in config files, command lines, logs
   or audit rows; output is passed through a redactor.
8. Devices pair with end-to-end encrypted, key-based links; a paired desktop administers a host only
   with an explicit, revocable right.
9. File tools refuse the data directory, credential files and the CLIs' sign-in stores; file changes
   are checkpointed so they can be undone; the audit log is append-only.
10. There is no telemetry: data goes to the model provider you configured and nowhere else.

The longer description is in the [user guide](docs/guide/security.md).
