import assert from "node:assert/strict";
import { test } from "node:test";
import {
  type ActionItem,
  type MenuItem,
  type MenuSpec,
  askDraft,
  botRowMenu,
  chatRowMenu,
  draftWithQuote,
  fileMenu,
  focusableIndexes,
  hasActions,
  imageMenu,
  isMenuKey,
  isSafeToRun,
  keepNativeMenu,
  linkInfo,
  linkMenu,
  messageMenu,
  navigate,
  placeMenu,
  quoteLines,
  selectionMenu,
  tidyItems
} from "./contextMenu.ts";

const t = (key: string, vars?: Record<string, string | number>) => (vars ? `${key}:${Object.values(vars).join(",")}` : key);
const noop = () => undefined;
const ids = (spec: MenuSpec | null) => (spec?.items ?? []).flatMap((i) => (i.kind === "separator" ? ["|"] : [i.id]));
const action = (label: string, extra: Partial<ActionItem> = {}): ActionItem => ({ kind: "action", id: label, label, run: noop, ...extra });

test("selection: copy always, quote / ask / find only when their action exists, nothing for empty text", () => {
  const all = selectionMenu({ t, mod: "Ctrl", text: "hello", onCopy: noop, onQuote: noop, onAsk: noop, onFindHere: noop, onFindAll: noop });
  assert.deepEqual(ids(all), ["copy", "|", "quote", "ask", "|", "findHere", "findAll"]);
  const bare = selectionMenu({ t, mod: "Ctrl", text: "hello", onCopy: noop });
  assert.deepEqual(ids(bare), ["copy"]);
  assert.equal(selectionMenu({ t, mod: "Ctrl", text: "  \n ", onCopy: noop }), null);
});

test("message: edit and regenerate appear only with a handler; pin and delete do not exist yet", () => {
  const last = messageMenu({ t, side: "user", onCopyText: noop, onQuote: noop, onEdit: noop, onInfo: noop });
  assert.deepEqual(ids(last), ["copyText", "quote", "|", "edit", "|", "info"]);
  const older = messageMenu({ t, side: "user", onCopyText: noop, onQuote: noop, onInfo: noop });
  assert.deepEqual(ids(older), ["copyText", "quote", "|", "info"]);
  const answer = messageMenu({ t, side: "assistant", onCopyText: noop, onCopyMarkdown: noop, onQuote: noop, onRegenerate: noop, onSpeak: noop, onInfo: noop });
  assert.deepEqual(ids(answer), ["copyText", "copyMarkdown", "quote", "|", "regenerate", "speak", "|", "info"]);
  const all = (answer?.items ?? []).filter((i): i is ActionItem => i.kind === "action");
  assert.ok(!all.some((i) => /pin|delete/i.test(i.id)), "no message-level pin / delete without daemon routes");
  assert.equal(messageMenu({ t, side: "user" })?.items.length, 0);
});

test("chat row: same actions as the ... menu, delete is danger and only asks for confirmation", () => {
  const ran: string[] = [];
  const spec = chatRowMenu({
    t, title: "Plan", pinned: true, archived: false, busy: false, canExport: true,
    onOpen: () => ran.push("open"), onRename: noop, onPin: noop, onExport: noop, onArchive: noop,
    onRequestDelete: () => ran.push("confirm-dialog")
  });
  assert.deepEqual(ids(spec), ["open", "|", "rename", "pin", "export", "archive", "|", "delete"]);
  const find = (id: string) => spec.items.find((i) => i.kind === "action" && i.id === id) as ActionItem;
  assert.equal(find("pin").label, "chatManage.unpin");
  assert.equal(find("delete").danger, true);
  assert.equal(find("delete").confirms, true);
  assert.equal(isSafeToRun(find("delete")), true);
  find("delete").run();
  assert.deepEqual(ran, ["confirm-dialog"], "choosing delete opens the confirmation, nothing more");
  // a danger item that does not confirm must never run, whatever the caller built
  assert.equal(isSafeToRun(action("bomb", { danger: true })), false);
  assert.equal(isSafeToRun(action("off", { disabled: true })), false);
});

test("chat row: busy chats pause changes, archived chats offer unarchive, no daemon means no export", () => {
  const spec = chatRowMenu({
    t, title: "x", pinned: false, archived: true, busy: true, canExport: false,
    onOpen: noop, onRename: noop, onPin: noop, onExport: noop, onArchive: noop, onRequestDelete: noop
  });
  const get = (id: string) => spec.items.find((i) => i.kind === "action" && i.id === id) as ActionItem;
  assert.equal(get("archive").label, "chatManage.unarchive");
  for (const id of ["rename", "pin", "archive", "delete", "export"]) assert.equal(get(id).disabled, true, id);
  assert.equal(get("open").disabled, undefined);
});

test("bot row: mute is a checkbox that follows the state; main bot and single-bot setups lose Make main", () => {
  const spec = botRowMenu({ t, name: "Ada", onOpen: noop, muted: true, onToggleMute: noop, onProfile: noop, onSettings: noop });
  assert.deepEqual(ids(spec), ["open", "|", "mute", "|", "profile", "settings"]);
  const mute = spec.items.find((i) => i.kind === "action" && i.id === "mute") as ActionItem;
  assert.equal(mute.checked, true);
  const other = botRowMenu({ t, name: "Ada", onOpen: noop, muted: false, onToggleMute: noop, onMakeMain: noop, onProfile: noop, onSettings: noop });
  assert.ok(ids(other).includes("makeMain"));
  assert.equal((other.items.find((i) => i.kind === "action" && i.id === "mute") as ActionItem).checked, false);
});

test("link: the header shows the real host, unsafe schemes get no menu, opening uses the checked url", () => {
  const opened: string[] = [];
  const spec = linkMenu({ t, href: "https://evil.example.net/login?x=1", onOpen: (u) => opened.push(u), onCopy: noop });
  assert.ok(spec);
  const header = spec.items[0];
  assert.equal(header.kind, "header");
  assert.equal((header as { label: string }).label, "evil.example.net");
  (spec.items[1] as ActionItem).run();
  assert.deepEqual(opened, ["https://evil.example.net/login?x=1"]);
  assert.equal(linkMenu({ t, href: "javascript:alert(1)", onOpen: noop, onCopy: noop }), null);
  assert.equal(linkMenu({ t, href: "java\tscript:alert(1)", onOpen: noop, onCopy: noop }), null);
  assert.deepEqual(linkInfo("mailto:ada@example.org"), { url: "mailto:ada@example.org", host: "example.org" });
  // a look-alike host is shown as the browser resolves it (punycode), not as the pretty text
  assert.match(linkInfo("https://аpple.com/")?.host ?? "", /^xn--/);
});

test("image and file menus", () => {
  assert.deepEqual(ids(imageMenu({ t, name: "a.png", onSave: noop })), ["saveImage"]);
  assert.deepEqual(ids(imageMenu({ t, name: "a.png", onSave: noop, onCopyImage: noop })), ["copyImage", "saveImage"]);
  assert.deepEqual(ids(fileMenu({ t, name: "a.pdf", onCopyName: noop })), ["copyName"]);
  assert.deepEqual(ids(fileMenu({ t, name: "a.pdf", onCopyName: noop, onDownload: noop })), ["downloadFile", "copyName"]);
});

test("tidyItems removes leading, trailing and doubled separators and empty headers", () => {
  const sep: MenuItem = { kind: "separator" };
  const a = action("a");
  const b = action("b");
  assert.deepEqual(tidyItems([sep, a, sep, sep, b, sep]).map((i) => i.kind), ["action", "separator", "action"]);
  assert.deepEqual(tidyItems([{ kind: "header", id: "h", label: "h" }, sep, a]).map((i) => i.kind), ["action"]);
  assert.equal(hasActions(tidyItems([sep, false, null])), false);
});

test("keyboard: wrap-around, Home / End, disabled items are skipped", () => {
  const items: MenuItem[] = [action("Alpha"), action("Beta", { disabled: true }), { kind: "separator" }, action("Gamma"), action("Delta")];
  assert.deepEqual(focusableIndexes(items), [0, 3, 4]);
  assert.equal(navigate(items, -1, "ArrowDown").index, 0);
  assert.equal(navigate(items, -1, "ArrowUp").index, 4);
  assert.equal(navigate(items, 0, "ArrowDown").index, 3, "skips the disabled item and the separator");
  assert.equal(navigate(items, 4, "ArrowDown").index, 0, "wraps to the top");
  assert.equal(navigate(items, 0, "ArrowUp").index, 4, "wraps to the bottom");
  assert.equal(navigate(items, 3, "Home").index, 0);
  assert.equal(navigate(items, 0, "End").index, 4);
  assert.equal(navigate([action("x", { disabled: true })], -1, "ArrowDown").index, null);
});

test("keyboard: type-ahead jumps to the next label, builds a prefix, and forgets after a pause", () => {
  const items: MenuItem[] = [action("Copy"), action("Copy as Markdown"), action("Quote"), action("Search")];
  let r = navigate(items, -1, "q", undefined, 1000);
  assert.equal(r.index, 2);
  r = navigate(items, 2, "c", r.typeahead, 5000);
  assert.equal(r.index, 0, "a long pause starts a new word");
  r = navigate(items, 0, "c", r.typeahead, 5100);
  assert.equal(r.index, 1, "the same letter again moves to the next label starting with it");
  const fast = navigate(items, -1, "c", undefined, 10_000);
  const fast2 = navigate(items, fast.index ?? -1, "o", fast.typeahead, 10_100);
  assert.equal(fast2.index, 0, "co... stays on the first Copy");
  assert.equal(navigate(items, 0, "z", undefined, 0).index, null);
  assert.equal(navigate(items, 0, "Enter").index, null);
  assert.equal(navigate(items, 0, " ").index, null);
});

test("opening keys: Shift+F10 and the Menu key, nothing else", () => {
  const key = (k: string, extra = {}) => ({ key: k, shiftKey: false, ctrlKey: false, altKey: false, metaKey: false, ...extra });
  assert.equal(isMenuKey(key("F10", { shiftKey: true })), true);
  assert.equal(isMenuKey(key("ContextMenu")), true);
  assert.equal(isMenuKey(key("F10")), false);
  assert.equal(isMenuKey(key("F10", { shiftKey: true, ctrlKey: true })), false);
});

test("native menu stays for text fields, editable areas and Shift+right-click", () => {
  const base = { shiftKey: false, tagName: "P", isContentEditable: false, nativeZone: false };
  assert.equal(keepNativeMenu(base), false);
  assert.equal(keepNativeMenu({ ...base, tagName: "TEXTAREA" }), true);
  assert.equal(keepNativeMenu({ ...base, tagName: "INPUT" }), true);
  assert.equal(keepNativeMenu({ ...base, isContentEditable: true }), true);
  assert.equal(keepNativeMenu({ ...base, nativeZone: true }), true);
  assert.equal(keepNativeMenu({ ...base, shiftKey: true }), true);
});

test("placement: opens right and down, flips at each window edge, never leaves the window", () => {
  const view = { width: 180, height: 200, viewportWidth: 1000, viewportHeight: 800 };
  assert.deepEqual(placeMenu({ ...view, x: 100, y: 100 }), { left: 100, top: 100, originX: "left", originY: "top", maxHeight: 784 });
  const right = placeMenu({ ...view, x: 950, y: 100 });
  assert.equal(right.left, 770);
  assert.equal(right.originX, "right");
  const bottom = placeMenu({ ...view, x: 100, y: 750 });
  assert.equal(bottom.top, 550);
  assert.equal(bottom.originY, "bottom");
  const corner = placeMenu({ ...view, x: 999, y: 799 });
  assert.deepEqual([corner.left, corner.top], [812, 592]);
  // too small to flip: clamped inside the edge margin
  const tiny = placeMenu({ x: 50, y: 50, width: 180, height: 200, viewportWidth: 200, viewportHeight: 220 });
  assert.deepEqual([tiny.left, tiny.top], [12, 12]);
  const left = placeMenu({ ...view, x: -20, y: -20 });
  assert.deepEqual([left.left, left.top], [8, 8]);
});

test("quote and ask drafts: lines get '> ', long text is cut, nothing is ever sent", () => {
  assert.equal(quoteLines("one\ntwo\n\nthree"), "> one\n> two\n>\n> three");
  assert.ok(quoteLines("x".repeat(5000)).endsWith("…"));
  assert.equal(draftWithQuote("", "> hi"), "> hi\n\n");
  assert.equal(draftWithQuote("my note  ", "> hi"), "my note\n\n> hi\n\n");
  assert.equal(askDraft("", "Please explain:", "a\nb"), "Please explain:\n\n> a\n> b\n");
});
