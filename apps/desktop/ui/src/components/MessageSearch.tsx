import { Clock, FileText, Link2, MessageSquare, RefreshCw, Search, TriangleAlert, X } from "lucide-react";
import { type KeyboardEvent, useEffect, useId, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { BotAvatar } from "ui";
import type { SearchPage } from "../api/types";
import type { SearchRequest } from "../lib/chatSearchRequest";
import { fetchHistoryPage, groupHistoryHits, historySnippetParts } from "../lib/chatHistory";
import { botView } from "../lib/botProfile";
import { chatTitleLabel } from "../lib/chatTitle";
import { isTabStop, returnFocusTarget, trapTarget } from "../lib/focusTrap";
import { type HumanError, humanError } from "../lib/humanError";
import { quickFind } from "../lib/quickFind";
import { appInert } from "../lib/inertStack";
import { layerStack } from "../lib/modalStack";
import {
  NO_FILTERS,
  type SearchFilters,
  type SearchWhen,
  filterParams,
  filtersActive,
  loadRecent,
  pushRecent,
  resultDate,
  saveRecent,
  stepCursor
} from "../lib/messageSearch";
import { useDaemon } from "../state/DaemonContext";
import styles from "./MessageSearch.module.css";

const FOCUSABLE = 'button:not([disabled]), input:not([disabled]), select:not([disabled])';
const WHEN: readonly SearchWhen[] = ["any", "day", "week", "month"];
const DEBOUNCE_MS = 250;

type Row =
  | { kind: "recent"; id: string; term: string }
  | { kind: "bot"; id: string; botId: string }
  | { kind: "chat"; id: string; chatId: string }
  | { kind: "hit"; id: string; chatId: string; messageId: string };

/**
 * Search bots and chats by name (instant, from the lists in memory), then the message history of
 * every bot, WhatsApp-style. A centred overlay (like the command
 * palette) rather than a bar in the sidebar: results need room, the rail can be one icon wide.
 */
export function MessageSearch({
  onClose,
  onOpenChat,
  onOpenBot,
  seed
}: {
  /** A phrase sent from a right-click menu: it is filled in and searched at once. */
  seed?: SearchRequest | null;
  onClose: () => void;
  onOpenChat: (chatId: string, messageId?: string) => void;
  /** Open a bot found by name (its latest chat or landing page). */
  onOpenBot?: (botId: string) => void;
}) {
  const { t, i18n } = useTranslation();
  const { client, bots, chats } = useDaemon();
  const uid = useId();
  const [query, setQuery] = useState(seed?.query ?? "");
  const [filters, setFilters] = useState<SearchFilters>(NO_FILTERS);
  const [page, setPage] = useState<SearchPage | null>(null);
  const [shown, setShown] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<HumanError | null>(null);
  const [recent, setRecent] = useState<string[]>(() => loadRecent());
  const [cursor, setCursor] = useState(0);
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const listRef = useRef<HTMLUListElement | null>(null);
  const active = useRef<AbortController | null>(null);
  const nowRef = useRef(Date.now());

  const seqRef = useRef(0);
  if (seqRef.current === 0) seqRef.current = layerStack.next();
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    const dialogNode = dialogRef.current;
    const seq = seqRef.current;
    layerStack.add(seq);
    appInert.acquire(seq);
    inputRef.current?.focus();
    return () => {
      active.current?.abort();
      layerStack.remove(seq);
      appInert.release(seq);
      const current = document.activeElement as HTMLElement | null;
      if (!current || current === document.body || (dialogNode?.contains(current) ?? false)) {
        returnFocusTarget(previous, (el) => el.isConnected)?.focus?.();
      }
    };
  }, []);

  const term = query.trim();
  const run = async (phrase: string, f: SearchFilters, offset: number) => {
    if (!client) return;
    active.current?.abort();
    const controller = new AbortController();
    active.current = controller;
    nowRef.current = Date.now();
    setBusy(true);
    setError(null);
    try {
      const params = filterParams(f, nowRef.current);
      const next = await fetchHistoryPage(
        (o) => client.searchChatHistory(phrase, o, controller.signal, params),
        offset
      );
      if (controller.signal.aborted) return;
      setShown(phrase);
      setPage((prev) => ({
        ...next,
        results: offset && prev ? [...prev.results, ...next.results] : next.results
      }));
      if (!offset) setCursor(0);
    } catch (e) {
      if (!controller.signal.aborted) setError(humanError(e));
    } finally {
      if (!controller.signal.aborted) setBusy(false);
    }
  };

  // Search as you type; a pause of a quarter second keeps typing smooth and the daemon calm.
  useEffect(() => {
    if (!term) {
      active.current?.abort();
      setPage(null);
      setBusy(false);
      setError(null);
      return;
    }
    const timer = window.setTimeout(() => void run(term, filters, 0), DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- `run` only reads stable refs and `client`
  }, [term, filters, client]);

  const hits = useMemo(() => page?.results ?? [], [page]);
  const found = useMemo(
    () => quickFind(bots.map((b) => ({ id: b.id, name: botView(b).name })), chats, term),
    [bots, chats, term]
  );
  const foundAny = found.bots.length + found.chats.length > 0;
  const groups = useMemo(() => groupHistoryHits(hits), [hits]);
  const rows: Row[] = useMemo(
    () =>
      term
        ? [
            ...found.bots.map((b): Row => ({ kind: "bot", id: `${uid}-b${b.id}`, botId: b.id })),
            ...found.chats.map((c): Row => ({ kind: "chat", id: `${uid}-c${c.id}`, chatId: c.id })),
            ...hits.map((h): Row => ({ kind: "hit", id: `${uid}-${h.message_id}`, chatId: h.chat_id, messageId: h.message_id }))
          ]
        : recent.map((r, i) => ({ kind: "recent", id: `${uid}-r${i}`, term: r })),
    [term, hits, recent, uid, found]
  );
  const at = rows.length === 0 ? -1 : Math.min(cursor, rows.length - 1);

  useEffect(() => {
    if (at >= 0) document.getElementById(rows[at].id)?.scrollIntoView?.({ block: "nearest" });
  }, [at, rows]);

  const open = (row: Row | undefined) => {
    if (!row) return;
    if (row.kind === "recent") {
      setQuery(row.term);
      return;
    }
    const next = pushRecent(recent, term);
    setRecent(next);
    saveRecent(next);
    onClose();
    if (row.kind === "bot") onOpenBot?.(row.botId);
    else if (row.kind === "chat") onOpenChat(row.chatId);
    else onOpenChat(row.chatId, row.messageId);
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.nativeEvent.isComposing) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      setCursor(stepCursor(at, rows.length, e.key === "ArrowDown" ? 1 : -1));
    } else if (e.key === "Enter" && e.target === inputRef.current) {
      e.preventDefault();
      open(rows[at]);
    } else if (e.key === "Escape" && layerStack.isTop(seqRef.current)) {
      e.preventDefault();
      e.stopPropagation();
      onClose();
    } else if (e.key === "Tab" && dialogRef.current) {
      const node = dialogRef.current;
      const items = [...node.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((el) =>
        isTabStop({ tabIndex: el.tabIndex, rects: el.getClientRects().length, inert: el.closest("[inert]") !== null })
      );
      const target = trapTarget(items, document.activeElement as HTMLElement | null, node, e.shiftKey);
      if (target === "none") {
        e.preventDefault();
        inputRef.current?.focus();
      } else if (target) {
        e.preventDefault();
        target.focus();
      }
    }
  };

  const patch = (p: Partial<SearchFilters>) => setFilters((f) => ({ ...f, ...p }));
  const botFor = (id: string) => bots.find((b) => b.id === id);
  const now = nowRef.current;
  const showEmpty = term !== "" && !busy && !error && page !== null && hits.length === 0 && !foundAny;
  const rowAt = (id: string) => rows.findIndex((r) => r.id === id);
  const more = page?.next_offset;

  return createPortal(
    <div className={styles.backdrop} onMouseDown={(e) => e.target === e.currentTarget && onClose()} data-testid="message-search">
      <div ref={dialogRef} className={styles.dialog} role="dialog" aria-modal="true" aria-label={t("messageSearch.title")} onKeyDown={onKeyDown}>
        <div className={styles.field}>
          <Search aria-hidden="true" className={styles.fieldIcon} />
          <input
            ref={inputRef}
            className={styles.input}
            type="text"
            maxLength={200}
            value={query}
            placeholder={t("messageSearch.placeholder")}
            aria-label={t("messageSearch.placeholder")}
            role="combobox"
            aria-expanded="true"
            aria-controls={`${uid}-list`}
            aria-activedescendant={at >= 0 ? rows[at].id : undefined}
            autoComplete="off"
            spellCheck={false}
            onChange={(e) => {
              setQuery(e.target.value);
              setCursor(0);
            }}
          />
          {query ? (
            <button type="button" className={styles.clear} aria-label={t("messageSearch.clear")} title={t("messageSearch.clear")} onClick={() => { setQuery(""); inputRef.current?.focus(); }}>
              <X aria-hidden="true" />
            </button>
          ) : (
            <kbd className={styles.kbd}>Esc</kbd>
          )}
        </div>

        <div className={styles.chips} role="group" aria-label={t("messageSearch.filters")}>
          <select className={styles.select} aria-label={t("messageSearch.bot")} data-active={filters.botId ? "true" : undefined} value={filters.botId ?? ""} onChange={(e) => patch({ botId: e.target.value || null })}>
            <option value="">{t("messageSearch.allBots")}</option>
            {bots.map((b) => <option key={b.id} value={b.id}>{botView(b).name}</option>)}
          </select>
          <select className={styles.select} aria-label={t("messageSearch.when")} data-active={filters.when !== "any" ? "true" : undefined} value={filters.when} onChange={(e) => patch({ when: e.target.value as SearchWhen })}>
            {WHEN.map((w) => <option key={w} value={w}>{t(`messageSearch.when_${w}`)}</option>)}
          </select>
          <button type="button" className={styles.chip} aria-pressed={filters.hasFile} onClick={() => patch({ hasFile: !filters.hasFile })}><FileText aria-hidden="true" />{t("messageSearch.hasFile")}</button>
          <button type="button" className={styles.chip} aria-pressed={filters.hasLink} onClick={() => patch({ hasLink: !filters.hasLink })}><Link2 aria-hidden="true" />{t("messageSearch.hasLink")}</button>
          {filtersActive(filters) ? <button type="button" className={styles.reset} onClick={() => setFilters(NO_FILTERS)}>{t("messageSearch.reset")}</button> : null}
        </div>

        <ul id={`${uid}-list`} ref={listRef} className={styles.list} role="listbox" aria-label={t("messageSearch.results")} aria-busy={busy}>
          {!term && recent.length > 0 ? (
            <>
              <li role="presentation" className={styles.heading}>
                <span>{t("messageSearch.recent")}</span>
                <button type="button" className={styles.reset} onClick={() => { setRecent([]); saveRecent([]); }}>{t("messageSearch.clearRecent")}</button>
              </li>
              {rows.map((r, i) => r.kind === "recent" ? (
                <li key={r.id} id={r.id} role="option" aria-selected={i === at} className={styles.row} data-active={i === at ? "true" : undefined} onMouseMove={() => setCursor(i)} onClick={() => open(r)}>
                  <Clock aria-hidden="true" className={styles.rowIcon} />
                  <span className={styles.recentTerm}>{r.term}</span>
                </li>
              ) : null)}
            </>
          ) : null}
          {term && found.bots.length > 0 ? (
            <>
              <li role="presentation" className={styles.heading}><span>{t("messageSearch.sectionBots")}</span></li>
              {found.bots.map((b) => {
                const id = `${uid}-b${b.id}`;
                const i = rowAt(id);
                const bot = botFor(b.id);
                return (
                  <li key={id} id={id} role="option" aria-selected={i === at} className={styles.row} data-active={i === at ? "true" : undefined} data-testid="search-bot-row" onMouseMove={() => setCursor(i)} onClick={() => open(rows[i])}>
                    {bot ? <BotAvatar bot={botView(bot)} size="xs" decorative /> : null}
                    <span className={styles.recentTerm}>{b.name}</span>
                  </li>
                );
              })}
            </>
          ) : null}
          {term && found.chats.length > 0 ? (
            <>
              <li role="presentation" className={styles.heading}><span>{t("messageSearch.sectionChats")}</span></li>
              {found.chats.map((c) => {
                const id = `${uid}-c${c.id}`;
                const i = rowAt(id);
                const bot = botFor(c.bot_id);
                return (
                  <li key={id} id={id} role="option" aria-selected={i === at} className={styles.row} data-active={i === at ? "true" : undefined} data-testid="search-chat-row" onMouseMove={() => setCursor(i)} onClick={() => open(rows[i])}>
                    <MessageSquare aria-hidden="true" className={styles.rowIcon} />
                    <span className={styles.recentTerm}>{chatTitleLabel(c.title, t("botPanel.untitled"))}</span>
                    {bot ? <span className={styles.date}>{botView(bot).name}</span> : null}
                  </li>
                );
              })}
            </>
          ) : null}
          {term && foundAny && groups.length > 0 ? (
            <li role="presentation" className={styles.heading}><span>{t("messageSearch.sectionMessages")}</span></li>
          ) : null}
          {term ? groups.map((group) => {
            const bot = botFor(group[0].bot_id);
            return (
              <li key={group[0].chat_id} role="presentation" className={styles.group}>
                <div className={styles.groupHead}>
                  {bot ? <BotAvatar bot={botView(bot)} size="xs" decorative /> : null}
                  <span className={styles.groupBot}>{bot ? botView(bot).name : ""}</span>
                  <span className={styles.groupChat}>{chatTitleLabel(group[0].chat_title, t("botPanel.untitled"))}</span>
                </div>
                <ul role="presentation" className={styles.groupRows}>
                  {group.map((h) => {
                    const i = rows.findIndex((r) => r.kind === "hit" && r.messageId === h.message_id);
                    const id = `${uid}-${h.message_id}`;
                    return (
                      <li key={h.message_id} id={id} role="option" aria-selected={i === at} className={styles.row} data-active={i === at ? "true" : undefined} onMouseMove={() => setCursor(i)} onClick={() => open(rows[i])}>
                        <span className={styles.snippet}>{historySnippetParts(h.snippet, shown).map((p, k) => p.match ? <mark key={k} className={styles.mark}>{p.text}</mark> : p.text)}</span>
                        <time className={styles.date} dateTime={new Date(h.created_at_ms).toISOString()}>{resultDate(h.created_at_ms, now, i18n.language)}</time>
                      </li>
                    );
                  })}
                </ul>
              </li>
            );
          }) : null}
          {term && more != null ? (
            <li role="presentation" className={styles.moreRow}>
              <button type="button" className={styles.reset} disabled={busy} onClick={() => void run(shown, filters, more)}>{t("messageSearch.more")}</button>
            </li>
          ) : null}
        </ul>

        {error ? (
          <div className={styles.errorState} role="alert" data-testid="message-search-error">
            <TriangleAlert aria-hidden="true" className={styles.errorIcon} />
            <div className={styles.errorText}>
              <strong>{error.title}</strong>
              {error.hint ? <span>{error.hint}</span> : null}
            </div>
            <button type="button" className={styles.retry} disabled={busy} onClick={() => void run(term, filters, 0)}>
              <RefreshCw aria-hidden="true" />
              {t("messageSearch.retry")}
            </button>
          </div>
        ) : null}

        <div className={styles.status} role="status" aria-live="polite">
          {busy ? t("messageSearch.searching")
            : showEmpty ? t("messageSearch.none")
            : !term && recent.length === 0 ? t("messageSearch.hint")
            : null}
        </div>
      </div>
    </div>,
    document.body
  );
}
