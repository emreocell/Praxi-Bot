/**
 * The model behind the app's right-click menus (owner P5).  Pure logic only: which menu belongs to
 * which thing under the pointer, which items are shown, how the keyboard moves through them and
 * where the menu is placed.  `components/ContextMenu.tsx` draws it and stays thin.
 *
 * Rules the builders follow:
 *  - an item exists only when its action exists today (a missing handler hides the item, it is
 *    never faked);
 *  - a destructive item (`danger`) must declare `confirms`: its `run` only opens the existing
 *    confirmation dialog and never deletes by itself;
 *  - text the user copies is the text they see (rendered / redacted), never raw event JSON.
 */
import { sanitizeUrl } from "./markdown.ts";

export type Translate = (key: string, vars?: Record<string, string | number>) => string;

/** Icon names; the component maps each to a lucide icon. */
export type MenuIcon =
  | "copy"
  | "markdown"
  | "quote"
  | "sparkles"
  | "search"
  | "searchAll"
  | "edit"
  | "refresh"
  | "speak"
  | "info"
  | "external"
  | "link"
  | "image"
  | "download"
  | "file"
  | "open"
  | "pin"
  | "pinOff"
  | "archive"
  | "unarchive"
  | "trash"
  | "bell"
  | "bellOff"
  | "star"
  | "user"
  | "settings";

export interface ActionItem {
  kind: "action";
  id: string;
  label: string;
  icon?: MenuIcon;
  /** Shortcut hint shown at the right edge (display only). */
  hint?: string;
  /** Destructive: drawn in the danger colour. */
  danger?: boolean;
  /** For danger items: `run` only opens a confirmation dialog; nothing is deleted by it. */
  confirms?: boolean;
  disabled?: boolean;
  /** Present (true or false) makes the item a checkbox item. */
  checked?: boolean;
  run: () => void;
}

export interface SeparatorItem {
  kind: "separator";
}

/** A small caption line (the host of a link, a group name). Not focusable. */
export interface HeaderItem {
  kind: "header";
  id: string;
  label: string;
  /** Full text for the tooltip. */
  title?: string;
}

export type MenuItem = ActionItem | SeparatorItem | HeaderItem;

export interface MenuSpec {
  /** Accessible name of the menu. */
  label: string;
  items: MenuItem[];
}

const SEP: SeparatorItem = { kind: "separator" };

function dropSeparators(items: readonly MenuItem[]): MenuItem[] {
  const out: MenuItem[] = [];
  for (const item of items) {
    if (item.kind === "separator" && (out.length === 0 || out[out.length - 1].kind === "separator")) continue;
    out.push(item);
  }
  while (out.length > 0 && out[out.length - 1].kind === "separator") out.pop();
  return out;
}

/** Drops falsy entries, doubled / leading / trailing separators and headers with nothing below. */
export function tidyItems(items: ReadonlyArray<MenuItem | false | null | undefined>): MenuItem[] {
  const out = dropSeparators(items.filter((i): i is MenuItem => !!i));
  // a header that is followed by no action (before the next separator) is noise
  const kept = out.filter((item, i) => {
    if (item.kind !== "header") return true;
    const rest = out.slice(i + 1);
    const next = rest.findIndex((x) => x.kind === "separator");
    return rest.slice(0, next < 0 ? rest.length : next).some((x) => x.kind === "action");
  });
  return dropSeparators(kept);
}

export function hasActions(items: readonly MenuItem[]): boolean {
  return items.some((i) => i.kind === "action");
}

/** Indexes (into `items`) the keyboard can land on: actions that are not disabled. */
export function focusableIndexes(items: readonly MenuItem[]): number[] {
  const out: number[] = [];
  items.forEach((item, i) => {
    if (item.kind === "action" && !item.disabled) out.push(i);
  });
  return out;
}

/** A danger item may only be run when it declares that it confirms first. */
export function isSafeToRun(item: ActionItem): boolean {
  return !item.disabled && (!item.danger || item.confirms === true);
}

// ------------------------------------------------------------------ keyboard

export const TYPEAHEAD_MS = 700;

export interface TypeAhead {
  buffer: string;
  at: number;
}

export const FRESH_TYPEAHEAD: TypeAhead = { buffer: "", at: 0 };

export interface NavResult {
  /** Index into `items` that gets focus, or null when the key means nothing here. */
  index: number | null;
  typeahead: TypeAhead;
}

function step(order: readonly number[], current: number, dir: 1 | -1): number | null {
  if (order.length === 0) return null;
  const at = order.indexOf(current);
  if (at < 0) return dir === 1 ? order[0] : order[order.length - 1];
  return order[(at + dir + order.length) % order.length];
}

/**
 * The keyboard reducer: roving focus over the enabled actions with wrap-around, Home / End and
 * type-ahead (typing a letter jumps to the next label that starts with it; typing quickly builds a
 * longer prefix).  `current` is the focused item index, or -1 when none has focus yet.
 */
export function navigate(
  items: readonly MenuItem[],
  current: number,
  key: string,
  typeahead: TypeAhead = FRESH_TYPEAHEAD,
  now = 0
): NavResult {
  const order = focusableIndexes(items);
  if (key === "ArrowDown") return { index: step(order, current, 1), typeahead };
  if (key === "ArrowUp") return { index: step(order, current, -1), typeahead };
  if (key === "Home") return { index: order[0] ?? null, typeahead };
  if (key === "End") return { index: order[order.length - 1] ?? null, typeahead };
  if (key.length !== 1 || key === " ") return { index: null, typeahead };
  const fresh = now - typeahead.at > TYPEAHEAD_MS;
  const buffer = (fresh ? "" : typeahead.buffer) + key.toLowerCase();
  const next: TypeAhead = { buffer, at: now };
  // the same letter pressed again cycles through the labels that start with it
  const cycling = buffer.length > 1 && [...buffer].every((c) => c === buffer[0]);
  const prefix = cycling ? buffer[0] : buffer;
  const startAt = order.indexOf(current);
  // one letter (or the same letter again) moves past the focused item; a growing prefix may stay on it
  const from = startAt < 0 ? 0 : startAt + (buffer.length === 1 || cycling ? 1 : 0);
  const rotated = [...order.slice(from), ...order.slice(0, from)];
  const hit = rotated.find((i) => {
    const item = items[i];
    return item.kind === "action" && item.label.toLowerCase().startsWith(prefix);
  });
  return { index: hit ?? null, typeahead: next };
}

/** Shift+F10 or the Menu key: open the menu of the focused element. */
export function isMenuKey(e: Pick<KeyboardEvent, "key" | "shiftKey" | "ctrlKey" | "altKey" | "metaKey">): boolean {
  if (e.key === "ContextMenu") return !e.ctrlKey && !e.altKey && !e.metaKey;
  return e.key === "F10" && e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey;
}

// ------------------------------------------------------------------ pointer rules

export interface PointerInfo {
  shiftKey: boolean;
  /** Tag of the event target, upper case. */
  tagName: string;
  isContentEditable: boolean;
  /** The target sits inside an element marked `data-native-menu`. */
  nativeZone: boolean;
}

/**
 * True when the browser's own menu must stay: text fields and editable areas (spell check, paste,
 * dictation keep working), and Shift+right-click (power users, dev tools in dev builds).  On macOS
 * Ctrl+click already arrives as a `contextmenu` event, so it needs no rule of its own.
 */
export function keepNativeMenu(info: PointerInfo): boolean {
  if (info.shiftKey) return true;
  if (info.nativeZone || info.isContentEditable) return true;
  return info.tagName === "INPUT" || info.tagName === "TEXTAREA" || info.tagName === "SELECT";
}

export const LONG_PRESS_MS = 550;
export const LONG_PRESS_SLOP_PX = 8;

/** A finger that moved more than a few pixels is scrolling, not long-pressing. */
export function pressMoved(dx: number, dy: number): boolean {
  return Math.hypot(dx, dy) > LONG_PRESS_SLOP_PX;
}

// ------------------------------------------------------------------ placement

export const MENU_EDGE = 8;

export interface PlaceInput {
  x: number;
  y: number;
  width: number;
  height: number;
  viewportWidth: number;
  viewportHeight: number;
  edge?: number;
}

export interface Placement {
  left: number;
  top: number;
  /** Which corner of the menu sits on the pointer: drives the animation origin. */
  originX: "left" | "right";
  originY: "top" | "bottom";
  /** Room the menu may use before it scrolls. */
  maxHeight: number;
}

/** Opens right and down from the point; flips to the other side at a window edge; never leaves the window. */
export function placeMenu({ x, y, width, height, viewportWidth, viewportHeight, edge = MENU_EDGE }: PlaceInput): Placement {
  const fitsRight = x + width <= viewportWidth - edge;
  const fitsBelow = y + height <= viewportHeight - edge;
  const flipX = !fitsRight && x - width >= edge;
  const flipY = !fitsBelow && y - height >= edge;
  const rawLeft = flipX ? x - width : x;
  const rawTop = flipY ? y - height : y;
  const left = Math.max(edge, Math.min(rawLeft, viewportWidth - width - edge));
  const top = Math.max(edge, Math.min(rawTop, viewportHeight - height - edge));
  return {
    left,
    top,
    originX: flipX ? "right" : "left",
    originY: flipY ? "bottom" : "top",
    maxHeight: Math.max(0, viewportHeight - edge * 2)
  };
}

// ------------------------------------------------------------------ text helpers

export const QUOTE_MAX_CHARS = 2000;

/** "> " in front of every line (blank lines included), long text cut with an ellipsis. */
export function quoteLines(text: string): string {
  const clean = text.replace(/\r\n?/g, "\n").trim();
  const cut = clean.length > QUOTE_MAX_CHARS ? `${clean.slice(0, QUOTE_MAX_CHARS).trimEnd()}…` : clean;
  return cut
    .split("\n")
    .map((l) => (l.trim() === "" ? ">" : `> ${l}`))
    .join("\n");
}

/** The draft after adding a quote: the old draft stays, the caret lands on a fresh line below the quote. */
export function draftWithQuote(draft: string, quote: string): string {
  const head = draft.trim() === "" ? "" : `${draft.trimEnd()}\n\n`;
  return `${head}${quote}\n\n`;
}

/** The draft for "ask the bot about this": a short question line over the quoted text. */
export function askDraft(draft: string, intro: string, text: string): string {
  return draftWithQuote(draft, `${intro}\n\n${quoteLines(text)}`).replace(/\n\n$/, "\n");
}

export interface LinkInfo {
  /** The real address that will open (after the same checks Markdown links use). */
  url: string;
  /** The host a click would reach, shown in the header so misleading link text cannot hide it. */
  host: string;
}

/** The safe target of a link, or null when it would not open (so the menu has nothing to offer). */
export function linkInfo(href: string): LinkInfo | null {
  const url = sanitizeUrl(href);
  if (!url) return null;
  try {
    const u = new URL(url);
    if (u.protocol === "mailto:") return { url, host: decodeURIComponent(u.pathname).split("@").pop() ?? u.pathname };
    return { url, host: u.hostname };
  } catch {
    return null;
  }
}

// ------------------------------------------------------------------ builders

export interface SelectionCtx {
  t: Translate;
  /** Platform modifier label for the hints: "Ctrl" or "⌘". */
  mod: string;
  text: string;
  onCopy: () => void;
  onQuote?: () => void;
  onAsk?: () => void;
  onFindHere?: () => void;
  onFindAll?: () => void;
}

/** Selected text inside a chat message. */
export function selectionMenu(c: SelectionCtx): MenuSpec | null {
  if (c.text.trim() === "") return null;
  const items = tidyItems([
    { kind: "action", id: "copy", label: c.t("contextMenu.copy"), icon: "copy", hint: `${c.mod}+C`, run: c.onCopy },
    SEP,
    c.onQuote && { kind: "action", id: "quote", label: c.t("contextMenu.quote"), icon: "quote", run: c.onQuote },
    c.onAsk && { kind: "action", id: "ask", label: c.t("contextMenu.ask"), icon: "sparkles", run: c.onAsk },
    SEP,
    c.onFindHere && { kind: "action", id: "findHere", label: c.t("contextMenu.findHere"), icon: "search", hint: `${c.mod}+F`, run: c.onFindHere },
    c.onFindAll && { kind: "action", id: "findAll", label: c.t("contextMenu.findAll"), icon: "searchAll", run: c.onFindAll }
  ]);
  return { label: c.t("contextMenu.selectionLabel"), items };
}

export interface MessageCtx {
  t: Translate;
  side: "user" | "assistant";
  onCopyText?: () => void;
  /** Only for assistant answers: user messages are plain text, so there is nothing to convert. */
  onCopyMarkdown?: () => void;
  onQuote?: () => void;
  /** Edit and resend: only the last user message, only when `lib/resendTurn` says it is editable. */
  onEdit?: () => void;
  /** Regenerate: only the last answer of a resendable turn. */
  onRegenerate?: () => void;
  onSpeak?: () => void;
  speaking?: boolean;
  onInfo?: () => void;
  // Message-level pin and delete are deliberately absent: the daemon has no such routes yet
  // (only chat-level pin / archive / delete).  Add `onPin` / `onDelete` here when they exist.
}

/** A message without a text selection. */
export function messageMenu(c: MessageCtx): MenuSpec | null {
  const items = tidyItems([
    c.onCopyText && { kind: "action", id: "copyText", label: c.t("contextMenu.copyText"), icon: "copy", run: c.onCopyText },
    c.onCopyMarkdown && { kind: "action", id: "copyMarkdown", label: c.t("contextMenu.copyMarkdown"), icon: "markdown", run: c.onCopyMarkdown },
    c.onQuote && { kind: "action", id: "quote", label: c.t("contextMenu.quote"), icon: "quote", run: c.onQuote },
    SEP,
    c.onEdit && { kind: "action", id: "edit", label: c.t("contextMenu.edit"), icon: "edit", run: c.onEdit },
    c.onRegenerate && { kind: "action", id: "regenerate", label: c.t("contextMenu.regenerate"), icon: "refresh", run: c.onRegenerate },
    c.onSpeak && { kind: "action", id: "speak", label: c.t(c.speaking ? "contextMenu.stopSpeaking" : "contextMenu.speak"), icon: "speak", run: c.onSpeak },
    SEP,
    c.onInfo && { kind: "action", id: "info", label: c.t("contextMenu.info"), icon: "info", run: c.onInfo }
  ]);
  return { label: c.t(c.side === "user" ? "contextMenu.userMessageLabel" : "contextMenu.botMessageLabel"), items };
}

export interface ChatRowCtx {
  t: Translate;
  title: string;
  pinned: boolean;
  archived: boolean;
  /** A change is already on its way for this chat: changes are paused, like the "..." button. */
  busy: boolean;
  /** The export needs the daemon connection. */
  canExport: boolean;
  onOpen: () => void;
  onRename: () => void;
  onPin: () => void;
  onExport: () => void;
  onArchive: () => void;
  /** Opens the existing delete confirmation; never deletes by itself. */
  onRequestDelete: () => void;
}

/** A chat in the rail or the bot panel: the same actions as its "..." menu. */
export function chatRowMenu(c: ChatRowCtx): MenuSpec {
  const items = tidyItems([
    { kind: "action", id: "open", label: c.t("contextMenu.open"), icon: "open", run: c.onOpen },
    SEP,
    { kind: "action", id: "rename", label: c.t("chatManage.rename"), icon: "edit", disabled: c.busy, run: c.onRename },
    { kind: "action", id: "pin", label: c.t(c.pinned ? "chatManage.unpin" : "chatManage.pin"), icon: c.pinned ? "pinOff" : "pin", disabled: c.busy, run: c.onPin },
    { kind: "action", id: "export", label: c.t("chatExport.menu"), icon: "download", disabled: !c.canExport, run: c.onExport },
    { kind: "action", id: "archive", label: c.t(c.archived ? "chatManage.unarchive" : "chatManage.archive"), icon: c.archived ? "unarchive" : "archive", disabled: c.busy, run: c.onArchive },
    SEP,
    { kind: "action", id: "delete", label: c.t("chatManage.delete"), icon: "trash", danger: true, confirms: true, disabled: c.busy, run: c.onRequestDelete }
  ]);
  return { label: c.t("chatManage.menuLabel", { title: c.title }), items };
}

export interface BotRowCtx {
  t: Translate;
  name: string;
  onOpen: () => void;
  muted: boolean;
  /** Mute / unmute notifications. */
  onToggleMute?: () => void;
  /** Hidden for the main bot and when there is no other bot to choose from. */
  onMakeMain?: () => void;
  onProfile?: () => void;
  onSettings?: () => void;
}

/** A bot in the rail. */
export function botRowMenu(c: BotRowCtx): MenuSpec {
  const items = tidyItems([
    { kind: "action", id: "open", label: c.t("contextMenu.open"), icon: "open", run: c.onOpen },
    SEP,
    c.onToggleMute && { kind: "action", id: "mute", label: c.t("botPanel.muteNotify"), icon: c.muted ? "bellOff" : "bell", checked: c.muted, run: c.onToggleMute },
    c.onMakeMain && { kind: "action", id: "makeMain", label: c.t("botPanel.makeMain"), icon: "star", run: c.onMakeMain },
    SEP,
    c.onProfile && { kind: "action", id: "profile", label: c.t("contextMenu.botProfile"), icon: "user", run: c.onProfile },
    c.onSettings && { kind: "action", id: "settings", label: c.t("contextMenu.botSettings"), icon: "settings", run: c.onSettings }
  ]);
  return { label: c.t("contextMenu.botLabel", { name: c.name }), items };
}

export interface LinkCtx {
  t: Translate;
  href: string;
  onOpen: (url: string) => void;
  onCopy: (url: string) => void;
}

/** A link in a message: the header names the real host; opening goes through `openExternal` only. */
export function linkMenu(c: LinkCtx): MenuSpec | null {
  const info = linkInfo(c.href);
  if (!info) return null;
  const items = tidyItems([
    { kind: "header", id: "host", label: info.host, title: info.url },
    { kind: "action", id: "open", label: c.t("contextMenu.openLink"), icon: "external", run: () => c.onOpen(info.url) },
    { kind: "action", id: "copyLink", label: c.t("contextMenu.copyLink"), icon: "link", run: () => c.onCopy(info.url) }
  ]);
  return { label: c.t("contextMenu.linkLabel", { host: info.host }), items };
}

export interface ImageCtx {
  t: Translate;
  name: string;
  /** Only when the browser can put an image on the clipboard. */
  onCopyImage?: () => void;
  onSave: () => void;
}

/** An image in a message. */
export function imageMenu(c: ImageCtx): MenuSpec {
  const items = tidyItems([
    c.onCopyImage && { kind: "action", id: "copyImage", label: c.t("contextMenu.copyImage"), icon: "image", run: c.onCopyImage },
    { kind: "action", id: "saveImage", label: c.t("contextMenu.saveImage"), icon: "download", run: c.onSave }
  ]);
  return { label: c.t("contextMenu.imageLabel", { name: c.name }), items };
}

export interface FileCtx {
  t: Translate;
  name: string;
  /** Hidden when the app cannot hand the file over (no safe way to open it from the page). */
  onOpen?: () => void;
  onDownload?: () => void;
  onCopyName: () => void;
}

/**
 * Menu items for a file: attachments of a message today, the bot's file library when it arrives
 * (c752 attaches `fileMenu` to its rows with one line).
 */
export function fileMenu(c: FileCtx): MenuSpec {
  const items = tidyItems([
    c.onOpen && { kind: "action", id: "openFile", label: c.t("contextMenu.openFile"), icon: "open", run: c.onOpen },
    c.onDownload && { kind: "action", id: "downloadFile", label: c.t("contextMenu.downloadFile"), icon: "download", run: c.onDownload },
    { kind: "action", id: "copyName", label: c.t("contextMenu.copyName"), icon: "file", run: c.onCopyName }
  ]);
  return { label: c.t("contextMenu.fileLabel", { name: c.name }), items };
}

/** Shorthand for the library: just the items. */
export function fileMenuItems(c: FileCtx): MenuItem[] {
  return fileMenu(c).items;
}
