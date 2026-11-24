import { ChevronDown, ChevronUp, X } from "lucide-react";
import { type RefObject, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  clearHighlights,
  collectTextNodes,
  findMatches,
  matchRanges,
  paintHighlights,
  stepMatch
} from "../lib/findInChat";
import styles from "./ChatFindBar.module.css";

/**
 * Ctrl/Cmd+F bar for the open chat: highlights every match inside `rootRef`, steps with
 * Enter / Shift+Enter, closes with Esc. `contentKey` changes whenever the timeline re-renders so
 * matches follow new messages.
 */
export function ChatFindBar({
  rootRef,
  contentKey,
  initialQuery = "",
  onClose
}: {
  rootRef: RefObject<HTMLElement | null>;
  contentKey: unknown;
  /** Text to look for right away (a selection sent here from the right-click menu). */
  initialQuery?: string;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const inputRef = useRef<HTMLInputElement | null>(null);
  const [query, setQuery] = useState(initialQuery);
  const [current, setCurrent] = useState(-1);
  const [ranges, setRanges] = useState<Range[]>([]);
  const total = ranges.length;

  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
    return clearHighlights;
  }, []);

  // re-scan on a new query or new timeline content
  useLayoutEffect(() => {
    const root = rootRef.current;
    if (!root) {
      return;
    }
    const scan = () => {
      const nodes = collectTextNodes(root);
      const matches = findMatches(nodes.map((n) => n.nodeValue ?? ""), query);
      const next = matchRanges(nodes, matches);
      setRanges(next);
      setCurrent((c) => (next.length === 0 ? -1 : c >= 0 && c < next.length ? c : 0));
    };
    scan();
    // Lazy highlighting replaces plain code with token nodes after scrolling.
    // Rebuild ranges even if the query and number of matches are unchanged.
    let frame: number | undefined;
    const observer = new MutationObserver(() => {
      if (frame !== undefined) return;
      frame = requestAnimationFrame(() => { frame = undefined; scan(); });
    });
    observer.observe(root, { childList: true, characterData: true, subtree: true });
    return () => {
      observer.disconnect();
      if (frame !== undefined) cancelAnimationFrame(frame);
    };
  }, [query, contentKey, rootRef]);

  // paint + keep the current match in view
  useEffect(() => {
    paintHighlights(ranges, current);
    const r = ranges[current];
    const el = r?.startContainer.parentElement;
    el?.scrollIntoView?.({ block: "center" });
  }, [current, ranges]);

  const step = (dir: 1 | -1) => setCurrent((c) => stepMatch(c, ranges.length, dir));

  return (
    <div className={styles.bar} role="search" data-find-skip data-testid="chat-find">
      <input
        ref={inputRef}
        className={styles.input}
        type="text"
        value={query}
        placeholder={t("chat.find.placeholder")}
        aria-label={t("chat.find.label")}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            onClose();
          } else if (e.key === "Enter") {
            e.preventDefault();
            step(e.shiftKey ? -1 : 1);
          }
        }}
      />
      <span className={styles.count} role="status" aria-live="polite">
        {query.trim() ? (total === 0 ? t("chat.find.none") : t("chat.find.count", { current: current + 1, total })) : ""}
      </span>
      <button type="button" className={styles.btn} aria-label={t("chat.find.prev")} title={t("chat.find.prev")} disabled={total === 0} onClick={() => step(-1)}>
        <ChevronUp aria-hidden="true" />
      </button>
      <button type="button" className={styles.btn} aria-label={t("chat.find.next")} title={t("chat.find.next")} disabled={total === 0} onClick={() => step(1)}>
        <ChevronDown aria-hidden="true" />
      </button>
      <button type="button" className={styles.btn} aria-label={t("common.close")} title={t("common.close")} onClick={onClose}>
        <X aria-hidden="true" />
      </button>
    </div>
  );
}
