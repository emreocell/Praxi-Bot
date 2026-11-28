import { type RefObject } from "react";
import { useTranslation } from "react-i18next";
import type { ApiClient } from "../api/client";
import type { Message } from "../api/types";
import { canCopyImage, copyImageBlob, saveBlob } from "../lib/attachmentActions";
import { searchPhrase, requestSearchAllChats } from "../lib/chatSearchRequest";
import { type MenuSpec, fileMenu, imageMenu, linkMenu, messageMenu, selectionMenu } from "../lib/contextMenu";
import { copyText, visibleMessageText } from "../lib/copyText";
import { openExternal } from "../lib/externalLink";
import { currentSpeakingId, stopSpeaking } from "../lib/speaker";
import { modKeyLabel, useContextMenu, useMenuTranslate } from "./ContextMenu";
import { replySpeechSupported, useReplySpeaker } from "./SpeakButton";
import { useToast } from "./Toast";

/** The parts of a timeline entry the menus need. */
interface EntryLike {
  kind: string;
  key: string;
  msgId?: string;
  text?: string;
  attachments?: Message["attachments"];
}

export interface ChatMenuArgs {
  /** The timeline element: only things inside it get a designed menu. */
  rootRef: RefObject<HTMLElement | null>;
  entries: readonly EntryLike[];
  chatId: string;
  client: ApiClient | null;
  botId?: string | null;
  /** The user message that may be edited and resent right now (`lib/resendTurn`), if any. */
  editableMsgId: string | null;
  /** The answer that may be regenerated right now, if any. */
  regenerateKey: string | null;
  onQuote: (text: string) => void;
  onAsk: (text: string) => void;
  onFind: (text: string) => void;
  onEdit: (msgId: string) => void;
  onRegenerate: () => void;
}

/**
 * Right-click menus of the chat timeline: selected text, a message, a link, an image or a file.
 * Which one opens depends on what is under the pointer; everything else (the composer, the
 * search boxes, tool cards) keeps the browser's own menu.  Item rules live in `lib/contextMenu`.
 */
export function useChatContextMenu(a: ChatMenuArgs) {
  const { t } = useTranslation();
  const speakReply = useReplySpeaker();
  const menuT = useMenuTranslate();
  const toast = useToast();
  const cm = useContextMenu();
  const mod = modKeyLabel();

  const copy = (text: string) =>
    void copyText(text).then((ok) => toast.show(t(ok ? "contextMenu.copied" : "contextMenu.copyFailed"), ok ? "success" : "danger"));
  const failed = () => toast.show(t("contextMenu.fileFailed"), "danger");

  const resolve = (target: HTMLElement): MenuSpec | null => {
    const root = a.rootRef.current;
    const row = target.closest<HTMLElement>("[data-message-row]");
    const side = row?.dataset.messageRow;
    if (!root || !row || !root.contains(row) || (side !== "user" && side !== "assistant")) return null;
    const entry = a.entries.find((x) => x.key === row.id && x.kind === side);
    if (!entry) return null;
    const raw = visibleMessageText(entry.text ?? "");
    const rendered = side === "assistant" ? (row.querySelector<HTMLElement>("[data-message-body]")?.innerText ?? raw) : raw;

    // 1. selected text
    const sel = window.getSelection();
    const selected = sel && !sel.isCollapsed ? sel.toString() : "";
    if (selected.trim() && sel && root.contains(sel.anchorNode) && root.contains(sel.focusNode) && sel.containsNode(target, true)) {
      return selectionMenu({
        t: menuT,
        mod,
        text: selected,
        onCopy: () => copy(selected),
        onQuote: () => a.onQuote(selected),
        onAsk: () => a.onAsk(selected),
        onFindHere: () => a.onFind(searchPhrase(selected).slice(0, 120)),
        onFindAll: () => requestSearchAllChats(selected)
      });
    }

    // 2. a link: the header names the real host
    const link = target.closest<HTMLAnchorElement>("a[href]");
    if (link && row.contains(link)) {
      const spec = linkMenu({
        t: menuT,
        href: link.getAttribute("href") ?? "",
        onOpen: (url) => void openExternal(url),
        onCopy: (url) => copy(url)
      });
      if (spec) return spec;
    }

    // 3. an image or a file of a message
    const attId = target.closest<HTMLElement>("[data-attachment-id]")?.dataset.attachmentId;
    const att = attId ? entry.attachments?.find((x) => x.id === attId) : undefined;
    if (att && a.client) {
      const client = a.client;
      const fetchBlob = () => client.attachmentBlob(a.chatId, att.id);
      if (att.kind === "image") {
        return imageMenu({
          t: menuT,
          name: att.name,
          onCopyImage: canCopyImage()
            ? () => void fetchBlob().then(copyImageBlob).then((ok) => toast.show(t(ok ? "contextMenu.imageCopied" : "contextMenu.imageCopyFailed"), ok ? "success" : "danger"), failed)
            : undefined,
          onSave: () => void fetchBlob().then((blob) => saveBlob(blob, att.name), failed)
        });
      }
      return fileMenu({
        t: menuT,
        name: att.name,
        onDownload: () => void fetchBlob().then((blob) => saveBlob(blob, att.name), failed),
        onCopyName: () => copy(att.name)
      });
    }

    // 4. the message itself
    const infoButton = row.querySelector<HTMLButtonElement>('[data-testid="message-info"]');
    const speakable = side === "assistant" && replySpeechSupported() && rendered.trim() !== "";
    return messageMenu({
      t: menuT,
      side,
      onCopyText: rendered ? () => copy(rendered) : undefined,
      onCopyMarkdown: side === "assistant" && raw && raw !== rendered ? () => copy(raw) : undefined,
      onQuote: rendered ? () => a.onQuote(rendered) : undefined,
      onEdit: side === "user" && entry.msgId && entry.msgId === a.editableMsgId ? () => a.onEdit(entry.msgId as string) : undefined,
      onRegenerate: side === "assistant" && entry.key === a.regenerateKey ? a.onRegenerate : undefined,
      onSpeak: speakable
        ? () => (currentSpeakingId() === row.id ? stopSpeaking() : speakReply(row.id, entry.text ?? "", a.botId))
        : undefined,
      speaking: currentSpeakingId() === row.id,
      onInfo: infoButton ? () => infoButton.click() : undefined
    });
  };

  return { bind: cm.bind((target) => resolve(target)), menu: cm.menu };
}
