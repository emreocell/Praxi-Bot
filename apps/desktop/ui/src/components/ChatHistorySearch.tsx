import { Search } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { SearchPage } from "../api/types";
import { fetchHistoryPage, groupHistoryHits, historySnippetParts } from "../lib/chatHistory";
import { humanLine } from "../lib/humanError";
import { useDaemon } from "../state/DaemonContext";
import styles from "./ChatHistorySearch.module.css";

/** Explicit searches across all stored chats, including archived chats and other bots. */
export function ChatHistorySearch({ onOpenChat }: { onOpenChat: (chatId: string, messageId?: string) => void }) {
  const { t, i18n } = useTranslation();
  const { client } = useDaemon();
  const [query, setQuery] = useState("");
  const [searched, setSearched] = useState("");
  const [page, setPage] = useState<SearchPage | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const active = useRef<AbortController | null>(null);
  useEffect(() => () => active.current?.abort(), []);
  const search = async (offset = 0) => {
    if (!client || !query.trim()) return;
    active.current?.abort();
    const controller = new AbortController();
    active.current = controller;
    const phrase = offset ? searched : query.trim();
    setBusy(true);
    setError(null);
    if (!offset) { setPage(null); setSearched(phrase); }
    try {
      const next = await fetchHistoryPage((o) => client.searchChatHistory(phrase, o, controller.signal), offset);
      if (!controller.signal.aborted) setPage((prev) => ({ ...next, results: offset ? [...(prev?.results ?? []), ...next.results] : next.results }));
    } catch (e) {
      if (!controller.signal.aborted) setError(humanLine(e));
    } finally {
      if (!controller.signal.aborted) setBusy(false);
    }
  };
  return <div className={styles.root}>
    <form className={styles.form} onSubmit={(e) => { e.preventDefault(); void search(); }}>
      <input type="search" maxLength={200} value={query} placeholder={t("chatHistory.search")}
        aria-label={t("chatHistory.search")} onChange={(e) => {
          active.current?.abort(); setBusy(false); setQuery(e.target.value); setPage(null); setError(null);
        }} />
      <button type="submit" disabled={busy || !query.trim()} aria-label={t("chatHistory.submit")} title={t("chatHistory.submit")}><Search aria-hidden="true" /></button>
    </form>
    {busy ? <p role="status">{t("chatHistory.searching")}</p> : null}
    {error ? <p role="alert">{error}</p> : null}
    {page ? <div aria-label={t("chatHistory.results")}>
      <p role="status">{page.results.length === 0 && page.next_offset != null ? t("chatHistory.noneYet") : t("chatHistory.count", { count: page.results.length })}</p>
      {groupHistoryHits(page.results).map((group) => <section key={group[0].chat_id}>
        <h4>{group[0].chat_title || t("botPanel.untitled")}</h4>
        {group.map((hit) => <button type="button" className={styles.hit} key={hit.message_id} onClick={() => onOpenChat(hit.chat_id, hit.message_id)}>
          <time dateTime={new Date(hit.created_at_ms).toISOString()}>{new Date(hit.created_at_ms).toLocaleString(i18n.language)}</time>
          <span>{historySnippetParts(hit.snippet, searched).map((part, i) => part.match ? <mark key={i}>{part.text}</mark> : part.text)}</span>
        </button>)}
      </section>)}
      {page.next_offset !== null && page.next_offset !== undefined ? <button type="button" disabled={busy} onClick={() => void search(page.next_offset ?? 0)}>{t("chatHistory.more")}</button> : null}
    </div> : null}
  </div>;
}
