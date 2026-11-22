import assert from "node:assert/strict";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { resolveKeys, SHORTCUT_GROUPS, SHORTCUTS } from "./shortcuts.ts";

const srcDir = fileURLToPath(new URL("..", import.meta.url));
const locale = (name: string) =>
  JSON.parse(readFileSync(join(srcDir, "i18n/locales", `${name}.json`), "utf8")) as Record<string, unknown>;
const lookup = (dict: Record<string, unknown>, key: string) =>
  key.split(".").reduce<unknown>((o, k) => (o as Record<string, unknown> | undefined)?.[k], dict);

/**
 * Files that read keyboard events but are NOT app-wide shortcuts: they are the local keyboard
 * behaviour of one control (roving tabs, resize grips, Enter-to-submit fields, ...). Add a new
 * handler file either to SHORTCUTS (lib/shortcuts.ts) or here with a reason.
 */
const LOCAL_ONLY: Record<string, string> = {
  "api/mockLiveView.ts": "mock backend, not a UI shortcut",
  "components/AccountMenu.tsx": "arrow keys inside the account menu (covered by popup menu rows)",
  "components/BotEditorFields.tsx": "Enter inside a single field",
  "components/BotPanel.tsx": "arrow keys on the panel tabs",
  "components/CharacterPicker.tsx": "arrow keys inside the bot shape and color pickers",
  "components/ChatList.tsx": "Enter/Escape in inline rename and arrow keys on chat menu trigger",
  "components/ComputerPicker.tsx": "arrow keys between the run-location radio rows",
  "components/ModelPicker.tsx": "arrow keys inside the model picker list",
  "components/ResendTurn.tsx": "Escape and Ctrl/Cmd+Enter inside the inline message editor",
  "components/ResizeGrip.tsx": "arrow keys on a resize handle",
  "components/SecretRequestCard.tsx": "Escape cancels the credential form it is typed in",
  "components/ServerSignInModal.tsx": "Enter inside a single field",
  "components/SettingsUi.tsx": "arrow keys on a segmented radio group",
  "components/useBotReorder.ts": "keyboard reordering of the bot list",
  "lib/liveViewInput.ts": "forwards keys to the remote screen, not app shortcuts",
  "lib/liveViewLayers.ts": "layer routing for the live view",
  "overlay/events.ts": "stop hotkey formatting (documented as the stop entry)",
  "views/BotEditModal.tsx": "arrow keys on editor section tabs",
  "views/SchedulesView.tsx": "arrow, Home, and End navigation inside the schedule calendar grid",
  "views/SettingsModal.tsx": "arrow, Home, and End navigation across settings section tabs",
  "views/firstrun/BrainStep.tsx": "Enter inside a single field",
  "views/firstrun/FreedomStep.tsx": "arrow keys on a segmented control",
  "views/settings/ComputerCards.tsx": "arrow keys on computer target radio cards"
};

const KEY_HANDLER = /\b(?:e|ev|evt|event)\.key\b|\.key\s*(?:===|!==)\s*"/;

function walk(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((d) =>
    d.isDirectory() ? walk(join(dir, d.name)) : /\.tsx?$/.test(d.name) && !/\.test\.tsx?$/.test(d.name) ? [join(dir, d.name)] : []
  );
}

test("every shortcut entry points at an existing source file and has en + tr text", () => {
  const ids = new Set<string>();
  for (const s of SHORTCUTS) {
    assert.ok(!ids.has(s.id), `duplicate id ${s.id}`);
    ids.add(s.id);
    assert.ok(SHORTCUT_GROUPS.includes(s.group), s.id);
    assert.ok(existsSync(join(srcDir, s.source)), `${s.id}: missing ${s.source}`);
    for (const name of ["en", "tr", "de"]) {
      assert.equal(typeof lookup(locale(name), s.labelKey), "string", `${name}: ${s.labelKey}`);
    }
  }
  for (const g of SHORTCUT_GROUPS) {
    for (const name of ["en", "tr", "de"]) {
      assert.equal(typeof lookup(locale(name), `help.group.${g}`), "string", `${name}: group ${g}`);
    }
    assert.ok(g === "slash" || SHORTCUTS.some((s) => s.group === g), `empty group ${g}`);
  }
});

test("a source file with key handlers must be in the help list or exempted with a reason", () => {
  const documented = new Set(SHORTCUTS.map((s) => s.source));
  const unlisted = walk(srcDir)
    .map((f) => relative(srcDir, f).split("\\").join("/"))
    .filter((f) => KEY_HANDLER.test(readFileSync(join(srcDir, f), "utf8")))
    .filter((f) => !documented.has(f) && !(f in LOCAL_ONLY));
  assert.deepEqual(unlisted, [], "add a help entry in lib/shortcuts.ts (and help.* in en/tr) or list in LOCAL_ONLY");
  for (const f of Object.keys(LOCAL_ONLY)) assert.ok(existsSync(join(srcDir, f)), `stale exemption ${f}`);
});

test("send and newline follow the send setting", () => {
  const ctx = (sendWith: "enter" | "ctrl-enter") => ({ stopHotkey: "F8", sendWith });
  assert.equal(resolveKeys("@send", ctx("enter")), "Enter");
  assert.equal(resolveKeys("@newline", ctx("enter")), "Shift+Enter");
  assert.equal(resolveKeys("@send", ctx("ctrl-enter")), "Ctrl/Cmd+Enter");
  assert.equal(resolveKeys("@newline", ctx("ctrl-enter")), "Enter");
  assert.equal(resolveKeys("@stop", ctx("enter")), "F8");
});

test("slash group is read from the composer command lists, and ? only fires outside text fields", async () => {
  const { slashShortcuts, isHelpKey } = await import("./shortcuts.ts");
  const { APP_COMMANDS, LOCAL_COMMANDS, SLASH_COMMANDS } = await import("./slash.ts");
  assert.equal(slashShortcuts().length, APP_COMMANDS.length + LOCAL_COMMANDS.length + SLASH_COMMANDS.length);
  for (const s of slashShortcuts()) {
    for (const name of ["en", "tr", "de"]) assert.equal(typeof lookup(locale(name), s.labelKey), "string", `${name}: ${s.labelKey}`);
  }
  const ev = (key: string, tag: string, extra = {}) => ({ key, ctrlKey: false, metaKey: false, altKey: false, target: { tagName: tag, isContentEditable: false }, ...extra }) as never;
  assert.equal(isHelpKey(ev("?", "DIV")), true);
  assert.equal(isHelpKey(ev("?", "TEXTAREA")), false);
  assert.equal(isHelpKey(ev("?", "DIV", { ctrlKey: true })), false);
  assert.equal(isHelpKey(ev("a", "DIV")), false);
});
