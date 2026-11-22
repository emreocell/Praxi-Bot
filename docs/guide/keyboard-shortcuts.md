# Keyboard shortcuts

The Praxi desktop app provides keyboard shortcuts to help you navigate chats, control bot runs, open dialogs, and compose messages quickly without reaching for the mouse.

You can also view this list directly in the app at any time by pressing **`?`** when no text field has focus, choosing **Help** from the account menu, or typing `/help` in the composer. See [Keyboard shortcuts and "What's new"](chatting-and-approvals.md#keyboard-shortcuts-and-whats-new).

> **CLI users:** Keyboard shortcuts apply to the desktop app. If you are interacting with Praxi from a terminal, see the [CLI reference](cli-reference.md) for command-line equivalents (`bot chat`, `bot approve`, `bot status`, etc.).

---

## Global shortcuts

Global shortcuts work from anywhere in the desktop window.

| Shortcut | Action |
| --- | --- |
| `Ctrl/Cmd+K` | Command palette |
| `?` | Show this list (when no text field has focus) |
| `Ctrl/Cmd+,` | Open settings |
| `Ctrl+Alt+Esc` *(default, configurable)* | Stop all bot runs |
| `Ctrl/Cmd+F` | Find in this chat |
| `Enter / Shift+Enter, Esc` | Next / previous match in the find bar, close it |
| `Esc` | Close the side panel |
| `Shift+Esc` | Leave keyboard control of the live view |

---

## Chat composer

These shortcuts are active while focused in the chat composer input area.

| Shortcut | Action |
| --- | --- |
| `Enter` *(default)* | Send message |
| `Shift+Enter` *(default)* | New line |
| `Up / Down` | Earlier messages of this chat (empty box or first line; `Esc` brings your draft back) |
| `Up / Down` | Move through the slash / mention menu |
| `Enter / Tab` | Pick the highlighted item |
| `Esc` | Close the menu |

### Configurable composer and stop shortcuts

Three shortcuts (`@send`, `@newline`, and `@stop`) depend on user settings:

- **Send and new line (`@send`, `@newline`)**: the **Send message** setting (Settings → General) chooses which key sends a chat message.
  - **Enter (default)**: `Enter` sends the message, and `Shift+Enter` inserts a new line.
  - **Ctrl+Enter**: If you prefer writing multiline prompts without holding Shift, choose `Ctrl+Enter`. Then `Ctrl/Cmd+Enter` sends the message, while `Enter` inserts a new line.
- **Stop hotkey (`@stop`)**: the **Stop All Shortcut** setting changes the key that cancels active bot runs. The default is `Ctrl+Alt+Esc`; the presets are `Ctrl+Alt+Esc`, `Ctrl+Shift+Esc` and `Alt+Shift+S`, and you can record your own. In the Windows desktop app it also works while another app has focus; on other platforms, focus Praxi first.

For more details on message composition, attachments, and run management, see [Chatting and approvals](chatting-and-approvals.md).

---

## Slash commands

In addition to key combinations, the composer includes built-in slash commands. Typing `/` at the beginning of an empty composer opens a completion menu with available commands:

- **Daemon commands**:
  - `/stop`: Stop the current active run.
  - `/new`: Start a fresh chat with the current bot.
  - `/workflow <name> [key=value ...]`: Replay a saved workflow without calling the language model (see [Workflows](workflows.md)).
- **Local app commands**:
  - `/export`: Download the open chat as a Markdown (`.md`) file (see [Copying messages and Markdown export](chatting-and-approvals.md#copying-messages-and-markdown-export)).
  - `/help`: Open the keyboard shortcuts dialog.
  - `/settings`: Open the settings screen.
  - `/models`: Jump to the AI model settings.
  - `/apps`: Open the connected applications view.
  - `/schedules`: Open the scheduled tasks view.

Use `Up` / `Down` to navigate the suggestions and `Enter` or `Tab` to select one.

---

## Dialogs

These shortcuts apply when modal dialogs, popup menus, or the command palette are open.

| Shortcut | Action |
| --- | --- |
| `Esc` | Close the dialog |
| `Tab / Shift+Tab` | Move focus inside the dialog |
| `Up / Down, Enter` | Command palette: move and run |
| `Up / Down, Home / End` | Move through popup menus |
| `Esc` | Close a popup menu |
| `Esc` | End the voice call ([Voice call mode](voice-call.md)) |
| `Space / Enter (hold)` | Hold to talk in a push-to-talk call ([Voice call mode](voice-call.md)) |

---

## Related documentation

- [Accessibility](accessibility.md) — Keyboard navigation order, skip links, screen-reader landmarks and live regions, focus rings, high-contrast themes, and reduced motion.
- [Chatting and approvals](chatting-and-approvals.md) — Learn about turns, steering, approving tool calls, and managing chats.
- [CLI reference](cli-reference.md) — Command reference for interacting with Praxi from the command line.
- [Workflows](workflows.md) — Create and trigger automated sequences with `/workflow`.
