# Import from other assistants

Praxi can read setups you already have in Claude, Cursor, VS Code, Codex,
OpenClaw, Hermes, or a folder you choose, and copy what is safe into Praxi
(nothing is sent to the network during the scan).

## Open the import screen

- **Settings → General → Start-up and shortcuts → Import from other
  assistants** (card with **Look for setups**).
- First-run wizard: link at the bottom of an early step.
- **New bot → Import from file** imports a single exported profile instead; see
  [Bots and profiles](bots-and-profiles.md).

The import screen explains what it reads, shows which apps it found, and lets you
tick what to bring over. **Import** applies the choices through the local daemon;
`bot import detect` and `bot import run` do the same from the terminal (see
[`bot import`](cli-reference.md#bot-import)).

Praxi never overwrites bots or keys without asking. Secrets can stay in the other
app's store unless you choose to move them.