# Accessibility

Praxi is designed to be usable with a keyboard alone, with assistive technologies such as screen readers, across light, dark, and high-contrast themes, and with reduced motion or reduced transparency enabled.

For the quick shortcut reference table, see [Keyboard shortcuts](keyboard-shortcuts.md). You can also open the in-app shortcut sheet at any time by pressing **`?`** outside text fields, running **Keyboard shortcuts** from the command palette (`Ctrl/Cmd+K`), or typing `/help` in the chat composer.

---

## Keyboard navigation

### Skip link and landmarks

- **Skip to main content**: Pressing `Tab` from the top of the window reveals a **Skip to main content** pill at the top-left of the sidebar. Activating it (`Enter`) jumps focus past the left sidebar directly to the primary interactive control in the main pane (such as the chat composer or the current view's heading).
- **Semantic landmarks**:
  - The left bot rail is a `<aside>` landmark with `<nav>` and live reorder announcements.
  - The center pane is a `<main>` landmark (`#main-content`).
  - The right-hand bot panel (`BotPanel`) is a labeled complementary `<aside>` landmark (`id="bot-panel"`) that closes on `Esc`.
  - Modal dialogs (`Settings`, `Help`, `New Bot`, `Edit Bot`, `Bot Profile`, `Skills`, `Add Skill`, `Connected Apps`, `Command Palette`) render with `role="dialog"` and `aria-modal="true"`, make the background shell `inert`, trap `Tab` / `Shift+Tab` inside the active layer, and return focus to the trigger control when dismissed.

### First-run setup wizard

All three steps of the first-run wizard (**Connect a brain**, **Set freedom**, **Meet your bot**) support keyboard-only operation:

1. **Connect a brain (`brain`)**:
   - Language pills (`TR`, `EN`, `DE`) and **Skip for now** sit at the top of the frame in natural `Tab` order.
   - Detected CLI engines, cloud/local provider tiles, compatibility chips, and the API key input (`Enter` to connect) are reachable with `Tab` and `Shift+Tab`.
   - When moving between steps (**Next** / **Back**), focus moves to the step's `<h1>` heading so screen readers announce the new step title and step progress (`Step X of 3`).
2. **Set freedom (`freedom`)**:
   - The autonomy level cards (**Always ask**, **Balanced**, **Full autonomy**) form a `role="radiogroup"` with roving `tabIndex`.
   - Use `Left` / `Up` and `Right` / `Down` arrow keys to move between autonomy levels, then `Tab` to reach **Back** and **Next**.
3. **Meet your bot (`bot`)**:
   - The bot name input receives initial focus.
   - The 16 character silhouettes and the preset color swatches use roving `tabIndex`: press `Tab` to enter the shape grid or color row, and use `Left` / `Right` / `Up` / `Down` arrow keys to pick a shape or color swatch. One additional `Tab` reaches the custom color picker (`<input type="color">`), followed by **Back** and **Start chatting**.

### Chat, step list, and approval cards

- **Composer**:
  - `Enter` sends a message and `Shift+Enter` inserts a new line by default (configurable to `Ctrl/Cmd+Enter` in **Settings → General → Send message**).
  - `Up` / `Down` recalls earlier user messages when the composer is empty or on its first line (`Esc` restores the draft).
  - Typing `/` or `@` opens the slash-command or bot-mention completion listbox (`Up` / `Down` to navigate, `Enter` or `Tab` to pick, `Esc` to dismiss).
- **Message list and step list**:
  - The message scroller (`role="log"`, `aria-live="polite"`, `aria-relevant="additions"`) is keyboard-focusable (`tabIndex={0}`) for `PageUp` / `PageDown` and arrow scrolling.
  - Focusing any message row reveals its action bar (**Copy**, **Speak**, **Edit & resend**, **Message info**) with 24×24 px minimum hit targets.
  - Thinking chains and tool step groups use focusable toggle buttons (`aria-expanded`) so every intermediate step and code output can be inspected with `Enter` or `Space`.
- **Approval, question, and secret request cards**:
  - When a run pauses for an approval, clarifying question, or credential, the run status line above the composer offers a **Show card** button that scrolls the pending card into view (respecting `prefers-reduced-motion: reduce`).
  - Approval cards group their action buttons (**Approve**, **Allow for this session**, **Always allow**, **Deny**) in logical `Tab` order, and secret request forms cancel on `Esc`.

### Right panel (`BotPanel`) and resizable rails

- **Resizable sidebars**: Both the left bot rail and the right bot panel include keyboard-focusable resize grips (`role="separator"`, `aria-orientation="vertical"`). Focus a grip and press `Left` / `Right` arrow keys to resize the column in steps.
- **Bot list reordering**: Each bot row in the expanded left rail has a keyboard reorder grip button. Focus the grip and press `Up` / `Down` (or `Home` / `End`) to move the bot; a polite live region announces the new position.
- **Bot panel tabs**: The **Now** and **Library** tabs in the right panel implement `role="tablist"` with roving `tabIndex` and `Left` / `Right` / `Home` / `End` keys. Pressing `Esc` anywhere inside the right panel closes it and returns focus to the bot chip in the title bar.

### Settings (`SettingsModal`)

- **Opening**: Press `Ctrl/Cmd+,`, choose **Settings** from the profile menu, or click the gear button in the title bar. Focus starts in the **Search settings** input (`data-autofocus`).
- **Section navigation**: Move into the left section list with `Tab`, then use `Up` / `Down` / `Home` / `End` arrow keys (or `Tab` and `Enter` / `Space`) to switch between **General**, **Appearance**, **AI & Models**, **Computer**, **Mail & Calendar**, **Usage**, **Diagnostics**, and **Updates**.
- **Controls**:
  - Segmented pill controls (`Segmented`) and computer target cards (`ComputerTargetBlock`) use `role="radiogroup"` with roving `tabIndex` and `Left` / `Right` / `Up` / `Down` arrow keys.
  - On/off switches use `role="switch"` with `aria-checked` and toggle on `Space` or `Enter`.

---

## Screen-reader announcements and live regions

To keep screen-reader output informative without chatter:

- **Streaming run status**: The status sentence in `RunStatusLine` uses `role="status"` with `aria-live="polite"` and `aria-atomic="true"` on the status sentence alone. The 1-second elapsed timer (`aria-hidden="true"`) and action buttons sit outside the live text element so ticking seconds never trigger repeated announcements.
- **Approval requests and stop warnings**: When a run enters a waiting-for-approval state or a stop request is unconfirmed/failed, the status sentence switches to `role="alert"` (`aria-live="assertive"`) so the user is notified immediately.
- **Approval and secret countdown timers**: Per-second expiration countdowns on approval cards and secret request cards use `role="timer"` with `aria-live="off"` so they do not interrupt or spam screen readers every second.
- **Toast notifications**: Transient notifications render inside a polite status region (`role="status"`, `aria-live="polite"`).

---

## Focus indication, themes, and contrast

- **Visible focus rings**: Interactive controls use `:focus-visible` outlines and `--shadow-focus` (`0 0 0 2px var(--color-background), 0 0 0 4px var(--color-focus)`) so keyboard focus has a two-tone halo that remains visible over any surface or backdrop.
- **Theme and contrast verification**: All 13 built-in themes — including the dedicated high-contrast themes **High Contrast Dark** (`high-contrast`) and **High Contrast Light** (`high-contrast-light`) — are checked in CI by `pnpm --dir apps/desktop/ui check:themes` (enforcing WCAG text, muted text, border, and 3:1 focus ring contrast) and `pnpm --dir apps/desktop/ui check:a11y` (Playwright + `axe-core` WCAG 2.1 A/AA checks across screens and themes).

---

## Reduced motion and reduced transparency

- **Reduced motion (`prefers-reduced-motion: reduce`)**:
  - CSS keyframe animations (spinners, status pulses, message entrance transitions, popover rise animations, first-run transitions) and hover transforms are disabled when the operating system requests reduced motion.
  - Programmatic scrolling (`scrollToEnd`, **Show card** jump to pending approval, expanding advanced provider details) switches from `"smooth"` to `"instant"` when `prefers-reduced-motion: reduce` is active.
  - Procedural bot avatars disable idle breathing, blinking, and pointer eye-tracking when reduced motion is enabled.
- **Surface style and transparency**:
  - If frosted glass or backdrops make text harder to read, open **Settings → Appearance** and set **Panel style** to **Solid** or **Crisp borders (`line`)**, **Chat area** to **Solid**, and **Backdrop** to **None**, or pick **High Contrast Dark** / **High Contrast Light**.
