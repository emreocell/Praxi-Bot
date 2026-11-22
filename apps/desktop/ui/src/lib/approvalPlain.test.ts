import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
import type { i18n as I18n } from "i18next";
import { createServer, type ViteDevServer } from "vite";
import { egressDetails } from "./approvalDetails.ts";

type Plain = typeof import("./approvalPlain.ts");

let server: ViteDevServer;
let plain: Plain;
let i18n: I18n;

before(async () => {
  server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null }
  });
  ({ i18n } = await server.ssrLoadModule("/src/i18n/index.ts"));
  plain = (await server.ssrLoadModule("/src/lib/approvalPlain.ts")) as Plain;
});

after(async () => {
  await server?.close();
});

const card = {
  kind: "egress",
  reason:
    "this run read window msedge.exe, window chrome.exe, command output and now wants to send data to the shell This call also starts a program on this computer, so it asks every time, even when sending data after reads is allowed.",
  reason_code: "egress_executes",
  executes: true,
  read: ["window msedge.exe", "window chrome.exe", "command output"],
  host: "the shell",
  payload: "which chromium",
  payload_kind: "body"
};

test("the approval reason and action render as short Turkish sentences, never the raw English line", async () => {
  await i18n.changeLanguage("tr");
  const t = ((k: string, v?: Record<string, unknown>) => i18n.t(k, v)) as import("./approvalPlain.ts").T;
  const egress = egressDetails(card)!;
  const reason = plain.reasonLine(egress, t)!;
  assert.match(reason, /Bot şunu okudu: pencere msedge\.exe, pencere chrome\.exe, komut çıktısı/);
  assert.match(reason, /program başlatmak/);
  assert.match(reason, /hedef: komut satırı/, "the daemon's English target id is translated");
  assert.doesNotMatch(reason, /the shell/);
  assert.doesNotMatch(reason, /this run read|file shell_run/);
  const action = plain.actionText(
    "shell_run",
    { command: "which chromium || which google-chrome" },
    { computer: null, serverName: "Praxi host" },
    t
  )!;
  assert.equal(action, "Praxi host sunucusu üzerinde şu komutu çalıştır:");
  assert.equal(plain.actionCommand("shell_run", { command: "which chromium || which google-chrome" })?.text, "which chromium || which google-chrome");
  await i18n.changeLanguage("en");
  assert.match(plain.actionText("shell_run", { command: "ls" }, { computer: null, serverName: "Praxi host" }, t)!, /^Run this command on the Praxi host server:$/);
});

test("an older daemon without a reason code keeps its own text", async () => {
  const t = ((k: string) => k) as import("./approvalPlain.ts").T;
  assert.equal(plain.reasonLine(egressDetails({ ...card, reason_code: undefined })!, t), null);
});

test("the user's computer is named as a separate target only for a call the daemon bound to it", async () => {
  await i18n.changeLanguage("en");
  const t = ((k: string, v?: Record<string, unknown>) => i18n.t(k, v)) as import("./approvalPlain.ts").T;
  const pc = { kind: "pc", name: "Praxi desktop", nodeId: "abc" };
  assert.match(plain.actionText("desktop_launch", { executable: "notepad.exe" }, { computer: pc, serverName: "Praxi host" }, t)!, /on your computer \(Praxi desktop\)/);
  assert.match(plain.actionText("shell_run", { command: "ls" }, { computer: pc, serverName: "Praxi host" }, t)!, /the Praxi host server/);
});

test("every pc_* tool reads as a sentence about YOUR computer, in both languages", async () => {
  const w = { computer: null, serverName: "Praxi host" };
  for (const lang of ["en", "tr"]) {
    await i18n.changeLanguage(lang);
    const t = ((k: string, v?: Record<string, unknown>) => i18n.t(k, v)) as import("./approvalPlain.ts").T;
    const calls: [string, Record<string, unknown>][] = [
      ["pc_shell_run", { command: "dir" }],
      ["pc_fs_write", { path: "C:/a.txt" }],
      ["pc_fs_read", { path: "C:/a.txt" }],
      ["pc_fs_list", { path: "C:/" }],
      ["pc_browser_open", { url: "https://example.com" }],
      ["pc_browser_click", {}],
      ["pc_desktop_launch", { executable: "notepad.exe" }],
      ["pc_desktop_click", {}]
    ];
    for (const [tool, input] of calls) {
      const text = plain.actionText(tool, input, w, t)!;
      assert.ok(text && !text.includes("{{") && !text.startsWith("chat."), `${lang} ${tool}: ${text}`);
      assert.doesNotMatch(text, /Praxi host/, "never the server's name");
    }
  }
  await i18n.changeLanguage("en");
  const t = ((k: string, v?: Record<string, unknown>) => i18n.t(k, v)) as import("./approvalPlain.ts").T;
  assert.equal(plain.actionText("pc_shell_run", { command: "dir" }, w, t), "Run this command on YOUR computer:");
  assert.equal(plain.actionCommand("pc_shell_run", { command: "dir" })?.text, "dir");
});

test("a multi-line command is shown whole and pc_* page and screen actions are specific", async () => {
  await i18n.changeLanguage("en");
  const t = ((k: string, v?: Record<string, unknown>) => i18n.t(k, v)) as import("./approvalPlain.ts").T;
  const w = { computer: null, serverName: "Praxi host" };
  const shell = plain.actionCommand("pc_shell_run", { command: "dir\nRemove-Item C:/work -Recurse" })!.text;
  assert.equal(shell, "dir\nRemove-Item C:/work -Recurse", "every line stays visible");
  const long = plain.actionCommand("pc_shell_run", { command: "x".repeat(2500) })!;
  assert.ok(long.text.endsWith("…"), long.text.slice(-5));
  assert.equal(long.shortened, true);
  assert.equal(plain.actionCommand("pc_shell_run", { command: "x".repeat(2000) })!.shortened, false);
  assert.equal(plain.actionCommand("browser_open", { command: "x" }), null);
  assert.match(plain.actionText("pc_browser_click", { ref: "e12" }, w, t)!, /Click element e12/);
  assert.match(plain.actionText("pc_browser_fill", { ref: "e3", text: "hello" }, w, t)!, /e3.*hello/);
  assert.match(plain.actionText("pc_browser_tabs", { switch_to: 2 }, w, t)!, /tab 2/);
  assert.match(plain.actionText("pc_desktop_type", { text: "secret plan" }, w, t)!, /secret plan/);
  assert.match(plain.actionText("pc_desktop_key", { keys: ["ctrl", "s"] }, w, t)!, /ctrl\+s/);
  assert.match(plain.actionText("pc_desktop_launch", { executable: "cmd.exe", args: ["/c", "del x"] }, w, t)!, /cmd\.exe \/c del x/);
});

test("known internal target ids read as words in every language, a site stays as it is", async () => {
  for (const lang of ["en", "tr", "de"]) {
    await i18n.changeLanguage(lang);
    const t = ((k: string, v?: Record<string, unknown>) => i18n.t(k, v)) as import("./approvalPlain.ts").T;
    for (const id of ["the shell", "a script", "the browser", "the desktop", "the mailbox", "mailbox work", "this bot's scheduler"]) {
      const text = plain.hostText(id, t);
      assert.ok(text && !text.startsWith("chat.") && !text.includes("{{"), `${lang} ${id}: ${text}`);
      if (lang !== "en") assert.notEqual(text, id);
    }
    assert.equal(plain.hostText("example.com", t), "example.com");
  }
  await i18n.changeLanguage("en");
});

test("a browser batch card lists every step as a numbered readable line with invisible characters escaped", async () => {
  const details = {
    kind: "browser_steps",
    steps: [
      { n: 1, action: "click", ref: "e1", target: 'button "No thanks"' },
      { n: 2, action: "fill", ref: "e2", target: 'textbox "Email"', text: "a\u202Eb@x.io", text_chars: 7 },
      { n: 3, action: "press", key: "Enter" }
    ]
  };
  for (const [lng, first, third] of [["en", /Click button "No thanks"/, /Press the Enter key/], ["tr", /öğesine tıkla/, /tuşuna bas/], ["de", /anklicken/, /drücken/]] as const) {
    await i18n.changeLanguage(lng);
    const t = ((k: string, v?: Record<string, unknown>) => i18n.t(k, v)) as import("./approvalPlain.ts").T;
    const lines = plain.browserStepLines(details, t)!;
    assert.equal(lines.length, 3);
    assert.match(lines[0], first);
    assert.match(lines[2], third);
    assert.ok(lines[1].includes("a<U+202E>b@x.io"), lines[1]);
    assert.ok(!lines[1].includes("\u202E"));
  }
  await i18n.changeLanguage("en");
  assert.equal(plain.browserStepLines({ kind: "other" }, ((k: string) => k) as import("./approvalPlain.ts").T), null);
});
