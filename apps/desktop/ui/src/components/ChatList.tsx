import {
  Archive,
  ArchiveRestore,
  Check,
  Download,
  ChevronRight,
  LoaderCircle,
  MessageSquare,
  MoreHorizontal,
  Pencil,
  Pin,
  PinOff,
  Trash2,
  X
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { UpdateChatInput } from "../api/types";
import type { ChatActivity } from "../lib/botLanding";
import { CHAT_TITLE_MAX_LEN, chatHasActiveRun, partitionChats, validateChatTitle } from "../lib/chatManage";
import { saveChatExport, type ExportFormat } from "../lib/chatHistory";
import { humanLine } from "../lib/humanError";
import { useDaemon } from "../state/DaemonContext";
import { useToast } from "./Toast";
import { EmptyState } from "./EmptyState";
import { chatTitleLabel } from "../lib/chatTitle";
import { Callout, ConfirmDialog } from "./DialogParts";
import { Popover } from "./Popover";
import styles from "./ChatList.module.css";

export interface ChatListItem {
  id: string;
  title: string;
  createdAtMs: number;
  pinned?: boolean;
  archived?: boolean;
  dateLabel?: string;
  activity?: ChatActivity;
}

export interface ChatListProps {
  chats: readonly ChatListItem[];
  activeChatId?: string;
  runs: ReadonlyArray<{ chat_id?: string | null; state: string }>;
  pendingIds: ReadonlySet<string>;
  variant: "rail" | "panel";
  loading?: boolean;
  searchActive?: boolean;
  onOpenChat: (chatId: string) => void;
  onPatchChat: (chatId: string, patch: UpdateChatInput) => Promise<void>;
  onDeleteChat: (chatId: string) => Promise<void>;
}

/**
 * Shared chat/thread list for both the left rail and the bot panel: pinned chats float to top with
 * a pin glyph, archived chats tuck behind a collapsible toggle, and each row offers an accessible
 * overflow ("...") menu for inline rename, pin/unpin, archive/unarchive, and confirmed deletion.
 */
export function ChatList({
  chats,
  activeChatId,
  runs,
  pendingIds,
  variant,
  loading = false,
  searchActive = false,
  onOpenChat,
  onPatchChat,
  onDeleteChat
}: ChatListProps) {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const toast = useToast();
  const [exporting, setExporting] = useState(false);
  const exportChat = async (chatId: string, format: ExportFormat) => {
    if (!client || exporting) return;
    setExporting(true);
    closeMenu();
    try {
      await saveChatExport(client, chatId, format);
      toast.show(t("chat.export.done"), "success");
    } catch (e) { toast.show(humanLine(e), "danger"); }
    finally { setExporting(false); }
  };
  const [menuChatId, setMenuChatId] = useState<string | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [renameError, setRenameError] = useState<string | null>(null);
  const [archivedOpen, setArchivedOpen] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<ChatListItem | null>(null);

  const triggerRefs = useRef(new Map<string, HTMLButtonElement | null>());
  const anchorRef = useRef<HTMLButtonElement | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);

  const { active, archived } = useMemo(() => partitionChats(chats), [chats]);
  const showArchived = archivedOpen || (searchActive && archived.length > 0);
  const menuChat = menuChatId ? chats.find((c) => c.id === menuChatId) ?? null : null;

  useEffect(() => {
    if (menuChatId && !menuChat) setMenuChatId(null);
  }, [menuChatId, menuChat]);

  useEffect(() => {
    if (editingId && inputRef.current) {
      inputRef.current.focus();
      inputRef.current.select();
    }
  }, [editingId]);

  const closeMenu = useCallback(() => setMenuChatId(null), []);
  const openMenu = (chatId: string) => {
    anchorRef.current = triggerRefs.current.get(chatId) ?? null;
    setMenuChatId((prev) => (prev === chatId ? null : chatId));
  };

  const beginRename = (chat: ChatListItem) => {
    setMenuChatId(null);
    setEditingId(chat.id);
    setDraft(chatTitleLabel(chat.title, t("botPanel.untitled")));
    setRenameError(null);
  };

  const cancelRename = (restoreFocusFor?: string) => {
    setEditingId(null);
    setRenameError(null);
    if (restoreFocusFor) {
      requestAnimationFrame(() => triggerRefs.current.get(restoreFocusFor)?.focus());
    }
  };

  const submitRename = (chat: ChatListItem) => {
    const check = validateChatTitle(draft);
    if (!check.ok) {
      setRenameError(
        check.reason === "empty"
          ? t("chatManage.renameEmpty")
          : t("chatManage.renameTooLong", { max: CHAT_TITLE_MAX_LEN })
      );
      inputRef.current?.focus();
      return;
    }
    const currentDisplay = chatTitleLabel(chat.title, t("botPanel.untitled"));
    setEditingId(null);
    setRenameError(null);
    if (check.title === chat.title.trim() || (check.title === currentDisplay && chat.title.trim() !== "")) {
      return;
    }
    void onPatchChat(chat.id, { title: check.title });
  };

  const listClass = [styles.list, variant === "panel" ? styles.listPanel : ""].filter(Boolean).join(" ");

  const renderRow = (c: ChatListItem) => {
    const isActive = c.id === activeChatId;
    const busy = pendingIds.has(c.id);
    const displayTitle = chatTitleLabel(c.title, t("botPanel.untitled"));
    const isMenuOpen = menuChatId === c.id;
    const rowClass = [styles.row, variant === "panel" ? styles.rowPanel : styles.rowRail].join(" ");

    if (editingId === c.id) {
      return (
        <li key={c.id} className={rowClass} data-testid="chat-row-editing">
          <div className={styles.renameWrap}>
            <div className={styles.renameRow}>
              <input
                ref={inputRef}
                type="text"
                className={styles.renameInput}
                value={draft}
                maxLength={CHAT_TITLE_MAX_LEN}
                aria-label={t("chatManage.renameInputLabel")}
                aria-invalid={renameError ? "true" : undefined}
                data-testid="chat-rename-input"
                onChange={(e) => {
                  setDraft(e.target.value);
                  if (renameError) setRenameError(null);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    e.stopPropagation();
                    submitRename(c);
                  } else if (e.key === "Escape") {
                    e.preventDefault();
                    e.stopPropagation();
                    cancelRename(c.id);
                  }
                }}
                onBlur={() => cancelRename()}
              />
              <button type="button" className={styles.renameBtn} title={t("common.save")} aria-label={t("common.save")} data-testid="chat-rename-save" onMouseDown={(e) => e.preventDefault()} onClick={() => submitRename(c)}>
                <Check aria-hidden="true" />
              </button>
              <button type="button" className={styles.renameBtn} title={t("common.cancel")} aria-label={t("common.cancel")} data-testid="chat-rename-cancel" onMouseDown={(e) => e.preventDefault()} onClick={() => cancelRename(c.id)}>
                <X aria-hidden="true" />
              </button>
            </div>
            {renameError ? <p className={styles.renameError} role="alert">{renameError}</p> : null}
          </div>
        </li>
      );
    }

    return (
      <li
        key={c.id}
        className={rowClass}
        data-active={isActive ? "true" : undefined}
        data-pinned={c.pinned ? "true" : undefined}
        data-archived={c.archived ? "true" : undefined}
        data-busy={busy ? "true" : undefined}
        data-testid={`chat-row-${variant}`}
      >
        <button
          type="button"
          className={styles.main}
          aria-current={isActive ? "true" : undefined}
          title={c.pinned ? `${displayTitle} · ${t("chatManage.pinnedBadge")}` : displayTitle}
          onClick={() => onOpenChat(c.id)}
        >
          {c.pinned ? (
            <Pin aria-hidden="true" className={`${styles.icon} ${styles.pinIcon}`} data-testid="chat-pin-glyph" />
          ) : (
            <MessageSquare aria-hidden="true" className={styles.icon} />
          )}
          <span className={styles.text}>
            <span className={styles.title}>{displayTitle}</span>
            {variant === "panel" && c.dateLabel ? (
              <span className={styles.date} data-testid="bot-panel-chat-date">{c.dateLabel}</span>
            ) : null}
          </span>
          {c.activity ? (
            <span className={styles.state} data-state={c.activity}>{t(`bots.chatState.${c.activity}`)}</span>
          ) : null}
        </button>
        <button
          ref={(el) => {
            if (el) triggerRefs.current.set(c.id, el);
            else triggerRefs.current.delete(c.id);
          }}
          type="button"
          className={styles.more}
          disabled={busy}
          aria-haspopup="menu"
          aria-expanded={isMenuOpen}
          aria-label={t("chatManage.menuLabel", { title: displayTitle })}
          title={t("chatManage.menuTooltip")}
          data-busy={busy ? "true" : undefined}
          data-testid="chat-menu-trigger"
          onClick={(e) => {
            e.stopPropagation();
            openMenu(c.id);
          }}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown" || e.key === "ArrowUp") {
              e.preventDefault();
              openMenu(c.id);
            }
          }}
        >
          {busy ? <LoaderCircle className={styles.spin} aria-hidden="true" /> : <MoreHorizontal aria-hidden="true" />}
        </button>
      </li>
    );
  };

  if (loading && chats.length === 0) {
    return <p className={styles.empty} role="status" data-testid="chat-list-loading">{t("chatManage.loading")}</p>;
  }

  const deleteActiveRun = deleteTarget ? chatHasActiveRun(deleteTarget.id, runs) : false;
  const deleteTitle = deleteTarget ? chatTitleLabel(deleteTarget.title, t("botPanel.untitled")) : "";

  return (
    <div className={styles.wrap}>
      {chats.length === 0 ? (
        !searchActive ? <EmptyState icon={MessageSquare} title={t("chatManage.emptyTitle")} testId="chat-list-empty">{t("chatManage.empty")}</EmptyState> : null
      ) : active.length === 0 && !searchActive ? (
        <p className={styles.empty} data-testid="chat-list-all-archived">{t("chatManage.allArchived")}</p>
      ) : active.length > 0 ? (
        <ul className={listClass}>{active.map(renderRow)}</ul>
      ) : null}

      {archived.length > 0 ? (
        <div className={styles.archivedBlock}>
          <button type="button" className={styles.archivedToggle} aria-expanded={showArchived} data-testid="chat-archived-toggle" onClick={() => setArchivedOpen((o) => !o)}>
            <ChevronRight className={styles.chevron} aria-hidden="true" />
            <Archive aria-hidden="true" />
            <span>{t("chatManage.archivedToggle", { count: archived.length })}</span>
          </button>
          {showArchived ? <ul className={listClass} data-testid="chat-archived-list">{archived.map(renderRow)}</ul> : null}
        </div>
      ) : null}

      <Popover
        open={menuChat !== null}
        anchor={anchorRef}
        onClose={closeMenu}
        placement="bottom-end"
        role="menu"
        label={menuChat ? t("chatManage.menuLabel", { title: chatTitleLabel(menuChat.title, t("botPanel.untitled")) }) : t("chatManage.menuTooltip")}
        width={196}
        className={styles.menu}
      >
        {menuChat ? (
          <>
            <button type="button" role="menuitem" className={styles.menuItem} data-testid="chat-action-rename" onClick={() => beginRename(menuChat)}>
              <Pencil aria-hidden="true" /><span>{t("chatManage.rename")}</span>
            </button>
            <button type="button" role="menuitem" className={styles.menuItem} data-testid="chat-action-pin" onClick={() => { closeMenu(); void onPatchChat(menuChat.id, { pinned: !menuChat.pinned }); }}>
              {menuChat.pinned ? <PinOff aria-hidden="true" /> : <Pin aria-hidden="true" />}
              <span>{menuChat.pinned ? t("chatManage.unpin") : t("chatManage.pin")}</span>
            </button>
            <button type="button" role="menuitem" className={styles.menuItem} data-testid="chat-action-archive" onClick={() => { closeMenu(); void onPatchChat(menuChat.id, { archived: !menuChat.archived }); }}>
              {menuChat.archived ? <ArchiveRestore aria-hidden="true" /> : <Archive aria-hidden="true" />}
              <span>{menuChat.archived ? t("chatManage.unarchive") : t("chatManage.archive")}</span>
            </button>
            <button type="button" role="menuitem" className={styles.menuItem} disabled={exporting || !client} onClick={() => void exportChat(menuChat.id, "markdown")}><Download aria-hidden="true" /><span>{t("chatHistory.exportMarkdown")}</span></button>
            <button type="button" role="menuitem" className={styles.menuItem} disabled={exporting || !client} onClick={() => void exportChat(menuChat.id, "json")}><Download aria-hidden="true" /><span>{t("chatHistory.exportJson")}</span></button>
            <button type="button" role="menuitem" className={`${styles.menuItem} ${styles.menuDanger}`} data-testid="chat-action-delete" onClick={() => { closeMenu(); setDeleteTarget(menuChat); }}>
              <Trash2 aria-hidden="true" /><span>{t("chatManage.delete")}</span>
            </button>
          </>
        ) : null}
      </Popover>

      <ConfirmDialog
        open={deleteTarget !== null}
        title={t("chatManage.deleteTitle")}
        tone="danger"
        confirmLabel={t("chatManage.deleteConfirm")}
        confirmDisabled={deleteActiveRun}
        body={
          <div className={styles.confirmStack}>
            <p>{t("chatManage.deleteBody", { title: deleteTitle })}</p>
            {deleteActiveRun ? <Callout tone="warning" role="alert">{t("chatManage.deleteActiveRun")}</Callout> : null}
          </div>
        }
        onCancel={() => setDeleteTarget(null)}
        onConfirm={() => {
          if (!deleteTarget || deleteActiveRun) return;
          const id = deleteTarget.id;
          setDeleteTarget(null);
          void onDeleteChat(id);
        }}
      />
    </div>
  );
}
