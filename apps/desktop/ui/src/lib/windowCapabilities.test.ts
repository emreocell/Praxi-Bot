import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { test } from "node:test";
import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { getCurrentWindow, type Window } from "@tauri-apps/api/window";

interface Capability {
  identifier: string;
  windows?: string[];
  webviews?: string[];
  local?: boolean;
  remote?: unknown;
  permissions: (string | { identifier: string })[];
}

const config = JSON.parse(readFileSync(new URL("../../../tauri.conf.json", import.meta.url), "utf8")) as {
  app: { security: { capabilities: string[] } };
};
const directory = new URL("../../../capabilities/", import.meta.url);
const capabilities = readdirSync(directory)
  .filter((name) => name.endsWith(".json"))
  .map((name) => JSON.parse(readFileSync(new URL(name, directory), "utf8")) as Capability)
  .filter((capability) => config.app.security.capabilities.includes(capability.identifier));
const identifiers = (capability: Capability) => capability.permissions.map(
  (permission) => typeof permission === "string" ? permission : permission.identifier
);

// These methods explicitly require individual grants in @tauri-apps/api/window;
// core:window:default only grants read operations and internal-toggle-maximize.
const actions: [string, (window: Window) => Promise<void>][] = [
  ["set-decorations", (window) => window.setDecorations(false)],
  ["is-maximized", async (window) => { await window.isMaximized(); }],
  ["minimize", (window) => window.minimize()],
  ["toggle-maximize", (window) => window.toggleMaximize()],
  ["close", (window) => window.close()]
];

for (const [action, perform] of actions) {
  test(`the main window can invoke the ${action} command used by its chrome`, async () => {
    const previous = Object.getOwnPropertyDescriptor(globalThis, "window");
    Object.defineProperty(globalThis, "window", { value: {}, configurable: true });
    try {
      mockWindows("main");
      mockIPC((command, payload) => {
        assert.equal(command, `plugin:window|${action.replaceAll("-", "_")}`);
        assert.equal((payload as { label: string }).label, "main");
        assert.ok(capabilities.some((capability) =>
          capability.windows?.includes("main") &&
          identifiers(capability).includes(`core:window:allow-${action}`)
        ), `${command} is not enabled by the shipped capabilities`);
      });
      await perform(getCurrentWindow());
    } finally {
      clearMocks();
      if (previous) Object.defineProperty(globalThis, "window", previous);
      else Reflect.deleteProperty(globalThis, "window");
    }
  });
}

test("window chrome grants stay local to the main window", () => {
  const grants = new Set(actions.map(([action]) => `core:window:allow-${action}`));
  for (const capability of capabilities.filter((c) => identifiers(c).some((p) => grants.has(p)))) {
    assert.deepEqual(capability.windows, ["main"]);
    assert.equal(capability.webviews, undefined);
    assert.equal(capability.remote, undefined);
    assert.notEqual(capability.local, false);
  }
});

// ── Custom app commands ───────────────────────────────────────────
// build.rs lists every #[tauri::command] in the app manifest, so each one needs
// an `allow-<command>` permission in a capability that names the calling window.

const desktopDir = new URL("../../../", import.meta.url);
const read = (relative: string, base: URL = desktopDir) => readFileSync(new URL(relative, base), "utf8");

const manifestCommands = [...read("src/command_names.rs").matchAll(/^\s+"([a-z_]+)",$/gm)].map((m) => m[1]);

const toCommand = (permission: string) => permission.replace(/^allow-/, "").replaceAll("-", "_");
const matchesLabel = (pattern: string, label: string) =>
  pattern.endsWith("*") ? label.startsWith(pattern.slice(0, -1)) : pattern === label;

function commandsFor(label: string): string[] {
  const allowed = new Set<string>();
  for (const capability of capabilities) {
    if (!capability.windows?.some((pattern) => matchesLabel(pattern, label))) continue;
    for (const id of identifiers(capability)) {
      if (id.startsWith("allow-")) allowed.add(toCommand(id));
    }
  }
  return [...allowed].sort();
}

/** In the manifest, but no UI code calls them, so no window is granted them. */
const UNGRANTED = [
  "autostart_supported", "get_monitors_geometry", "is_human_driving", "overlay_clear", "overlay_show_target",
  "quick_ask_connect", "quick_ask_hide", "quick_ask_open_in_app"
];
/** The only commands the Quick ask window may call (and only it). */
const quickAskCommands = ["quick_ask_connect", "quick_ask_hide", "quick_ask_open_in_app"];

const widgetCommands = [
  "dismiss_stop_widget",
  "get_control_state",
  "give_back_control",
  "pause_bot_run",
  "resume_bot_run",
  "stop_bot_run",
  "take_the_wheel"
];

test("the app manifest lists the commands the shell registers", () => {
  assert.ok(manifestCommands.length >= 26, "command_names.rs could not be parsed");
  assert.ok(manifestCommands.includes("finish_bot_run"));
  assert.ok(manifestCommands.includes("ack_stop"));
  assert.ok(manifestCommands.includes("stop_bot_run"));
});

test("per-window custom command table", () => {
  // Commands with no UI caller are in the manifest but granted to no window,
  // except finish_bot_run / ack_stop which are reserved to be called from main later.
  assert.deepEqual(commandsFor("main"), manifestCommands.filter((c) => !UNGRANTED.includes(c)).sort());
  assert.ok(commandsFor("main").includes("finish_bot_run") && commandsFor("main").includes("ack_stop"));
  assert.deepEqual(commandsFor("widget"), widgetCommands);
  assert.deepEqual(commandsFor("quick-ask"), quickAskCommands);
  assert.deepEqual(commandsFor("overlay-0"), ["get_control_state"]);
  assert.deepEqual(commandsFor("overlay-7"), ["get_control_state"]);
  assert.deepEqual(commandsFor("unknown-window"), []);
});

test("widget and overlays cannot call the run, daemon, provider or OS commands", () => {
  for (const label of ["widget", "overlay-0", "quick-ask"]) {
    const allowed = commandsFor(label);
    for (const command of [
      "quick_ask_configure", "owner_key", "providers_cli",
      "start_bot_run", "finish_bot_run", "ack_stop", "begin_action", "providers_cli", "get_autostart", "set_autostart",
      "daemon_connect", "daemon_restart", "simulate_bot_run", "set_stop_hotkey", "notify_show", "notify_take_actions",
      "set_needs_count", "set_tray_labels", "set_keep_in_tray"
    ]) {
      assert.ok(!allowed.includes(command), `${label} must not be granted ${command}`);
    }
  }
});

test("custom command grants are local to named windows", () => {
  for (const capability of capabilities.filter((c) => identifiers(c).some((id) => id.startsWith("allow-")))) {
    assert.ok((capability.windows ?? []).length > 0, `${capability.identifier} names no window`);
    assert.equal(capability.webviews, undefined);
    assert.equal(capability.remote, undefined);
    assert.notEqual(capability.local, false);
  }
});

/** invoke() command names reachable from the wrappers in overlay/events.ts, by exported function. */
function commandsByWrapper(): Map<string, string> {
  const source = read("src/overlay/events.ts", new URL("ui/", desktopDir));
  const wrappers = new Map<string, string>();
  for (const part of source.split(/^export (?:async )?function /m).slice(1)) {
    const name = /^(\w+)/.exec(part)?.[1];
    const command = /invoke(?:<[^>]*>)?\(\s*"(\w+)"/.exec(part.split(/^}/m)[0] ?? "")?.[1];
    if (name && command) wrappers.set(name, command);
  }
  return wrappers;
}

function commandsUsedBy(file: string): string[] {
  const source = read(`src/overlay/${file}`, new URL("ui/", desktopDir));
  assert.ok(!/invoke(?:<[^>]*>)?\(/.test(source), `${file} must call commands through overlay/events.ts`);
  const imported = /import\s*\{([^}]*)\}\s*from\s*"\.\/events"/.exec(source)?.[1] ?? "";
  const wrappers = commandsByWrapper();
  const used = new Set<string>();
  for (const name of imported.split(",").map((n) => n.replace(/^\s*type\s+/, "").trim())) {
    const command = wrappers.get(name);
    if (command) used.add(command);
  }
  return [...used].sort();
}

test("the widget UI only calls commands its capability grants", () => {
  const used = commandsUsedBy("WidgetWindow.tsx");
  assert.deepEqual(used, widgetCommands);
  for (const command of used) assert.ok(commandsFor("widget").includes(command), command);
});

test("the overlay UI only calls commands its capability grants", () => {
  const used = commandsUsedBy("OverlayWindow.tsx");
  assert.deepEqual(used, ["get_control_state"]);
  for (const command of used) assert.ok(commandsFor("overlay-0").includes(command), command);
});

/** Whether `source` imports `name` from the overlay/events module. */
function importsFromEvents(source: string, name: string): boolean {
  for (const m of source.matchAll(/import\s*\{([^}]*)}\s*from\s*"([^"]*)"/g)) {
    if (/(^|\/)events$/.test(m[2] ?? "") && (m[1] ?? "").split(",").some((n) => n.replace(/^\s*type\s+/, "").trim() === name)) return true;
  }
  return false;
}

/** Every non-test .ts/.tsx file under ui/src, recursively. */
function uiSources(directory: URL = new URL("ui/src/", desktopDir)): URL[] {
  const files: URL[] = [];
  for (const name of readdirSync(directory)) {
    const child = new URL(name, directory);
    if (statSync(child).isDirectory()) files.push(...uiSources(new URL(`${name}/`, directory)));
    else if (/\.tsx?$/.test(name) && !/\.test\.tsx?$/.test(name) && !name.endsWith(".d.ts")) files.push(child);
  }
  return files;
}

test("every command any UI file invokes is granted to the main window (the widget and overlay files are checked above)", () => {
  const invoked = new Map<string, string[]>();
  for (const file of uiSources()) {
    const source = readFileSync(file, "utf8");
    for (const call of source.matchAll(/\binvoke(?:<[^>]*>)?\(([^)]*)\)/g)) {
      const literals = [...(call[1] ?? "").matchAll(/"([a-z_]+)"/g)].map((m) => m[1] ?? "");
      assert.ok(literals.length > 0, `${file.pathname}: invoke() with a non-literal command name cannot be checked`);
      for (const command of literals) invoked.set(command, [...(invoked.get(command) ?? []), file.pathname]);
    }
  }
  assert.ok(invoked.size >= 15, "invoke() call sites could not be found");
  for (const [command, files] of invoked) {
    assert.ok(manifestCommands.includes(command), `${command} (${files.join(", ")}) is not in the app manifest`);
    if (quickAskCommands.includes(command)) {
      // Granted to the Quick ask window only, and only invoked from its own bridge file.
      assert.ok(files.every((f) => f.endsWith("/quickask/bridge.ts")), `${command} must be invoked from quickask/bridge.ts only`);
      assert.ok(commandsFor("quick-ask").includes(command) && !commandsFor("main").includes(command), command);
      continue;
    }
    if (UNGRANTED.includes(command)) {
      // A wrapper exists (overlay/events.ts) but nothing may call it: otherwise the grant is missing.
      const wrapper = [...commandsByWrapper()].find(([, c]) => c === command)?.[0];
      assert.ok(wrapper, `${command} is invoked outside overlay/events.ts but granted to no window`);
      for (const file of uiSources().filter((f) => !f.pathname.endsWith("/overlay/events.ts"))) {
        assert.ok(!importsFromEvents(readFileSync(file, "utf8"), wrapper), `${file.pathname} calls ${wrapper}, whose command ${command} has no grant`);
      }
      continue;
    }
    assert.ok(commandsFor("main").includes(command), `main lacks ${command}`);
    if (!commandsFor("widget").includes(command)) {
      assert.ok(!files.some((f) => /\/overlay\/(Widget|Overlay)Window\.tsx$/.test(f)), `${command} is invoked from a widget/overlay file`);
    }
  }
});

test("the CSP is a real policy that keeps scripts local and allows only what the app uses", () => {
  const security = (JSON.parse(read("tauri.conf.json")) as { app: { security: { csp: unknown; devCsp: unknown } } }).app.security;
  assert.equal(typeof security.csp, "string", "csp must not be null");
  const csp = security.csp as string;
  const directives = new Map(csp.split("; ").map((d) => {
    const [name, ...sources] = d.split(" ");
    return [name, sources] as const;
  }));
  assert.deepEqual(directives.get("default-src"), ["'self'"]);
  assert.deepEqual(directives.get("script-src"), ["'self'"]);
  assert.deepEqual(directives.get("img-src"), ["'self'", "blob:", "data:"]);
  assert.deepEqual(directives.get("connect-src"), [
    "'self'", "ipc:", "http://ipc.localhost", "http://127.0.0.1:*", "http://localhost:*", "http://[::1]:*"
  ]);
  assert.deepEqual(directives.get("object-src"), ["'none'"]);
  assert.deepEqual(directives.get("frame-src"), ["'none'"]);
  assert.ok(!csp.includes("unsafe-eval"));
  assert.ok(!/https?:\/\/(?!127\.0\.0\.1|localhost|ipc\.localhost|\[::1\])/.test(csp), "no remote origin");
  assert.ok(!/(^|\s)\*(\s|;|$)/.test(csp), "no bare wildcard source");
  const dev = security.devCsp;
  assert.equal(typeof dev, "string");
  for (const origin of (dev as string).split(/[ ;]+/).filter((t) => /^(https?|wss?):\/\//.test(t))) {
    assert.ok(/^(https?|wss?):\/\/(127\.0\.0\.1|localhost|ipc\.localhost|\[::1\])(:|$)/.test(origin), `devCsp origin ${origin} is not loopback`);
  }
  assert.ok(!(dev as string).includes("unsafe-eval"));
});

test("windows keep the default http IPC scheme the CSP assumes (useHttpsScheme unset or false)", () => {
  // The CSP allows http://ipc.localhost. useHttpsScheme: true moves Windows IPC to
  // https://ipc.localhost, which the CSP would block; change both together.
  const conf = JSON.parse(read("tauri.conf.json")) as { app: { windows: { useHttpsScheme?: boolean }[] } };
  for (const window of conf.app.windows) assert.notEqual(window.useHttpsScheme, true);
});
