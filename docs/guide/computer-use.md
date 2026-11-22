# Computer use on your own PC

Bots can work in apps on your Windows PC when the chat uses **This computer** and
the owner switch *Let my bots use this computer* is on. If it is off, the chat shows
a **Turn on computer use** card with one button.

## What a bot can do

| Tool | Purpose |
| --- | --- |
| `desktop_list_apps` | List visible windows (id, title, process). |
| `desktop_launch` | Start a program, document or URI. Packaged apps (for example WhatsApp) use `appx:<AppUserModelID>`. |
| `desktop_focus` | Bring an open window to the front and read it. |
| `desktop_get_state` | Read a window's accessibility (UI Automation) tree: names, roles, screen bounds. Optional window-scoped screenshot. |
| `desktop_wait` | Wait up to 10 s for a window title or an element name/role. |
| `desktop_click`, `desktop_type`, `desktop_key`, `desktop_scroll` | Act by element index (with `gen`) or, when the tree lacks the target, by screen x/y inside the observed window. |

The model is told to read the accessibility tree first and to take a screenshot only
when the tree does not expose the target.

## Safety

- Every focus, click, type, key and scroll goes through the permission engine: it asks by default, and an approval can be limited to the current task.
- Approved input is re-checked against the live window before it is sent; if a different window took focus the action is refused.
- Password fields are never typed into.
- Nothing runs while you are using the PC yourself, or while Stop / pause is active. The *Praxi is controlling your PC* indicator and the Stop hotkey stay in force.

## Not verified

- Behaviour with a real WhatsApp (packaged app) install was not tested on a live machine.
- A Notepad/Calculator integration test against the real desktop was not added; coverage is a fake backend plus unit tests.
- Linux/macOS peers: focus and wait fall back to "unsupported".
