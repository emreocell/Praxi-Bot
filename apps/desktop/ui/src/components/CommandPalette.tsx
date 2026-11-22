import { type KeyboardEvent, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { isTabStop, returnFocusTarget, trapTarget } from "../lib/focusTrap";
import { rank } from "../lib/fuzzy";
import { appInert } from "../lib/inertStack";
import { layerStack } from "../lib/modalStack";
import styles from "./CommandPalette.module.css";

const FOCUSABLE =
  'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([disabled])';

export interface PaletteCommand {
  id: string;
  title: string;
  /** Group label shown at the right of the row. */
  group: string;
  keywords?: readonly string[];
  run: () => void;
}

function Highlighted({ text, indices }: { text: string; indices: number[] }) {
  if (indices.length === 0) {
    return <>{text}</>;
  }
  const hit = new Set(indices);
  return (
    <>
      {Array.from(text).map((ch, i) =>
        hit.has(i) ? (
          <mark key={i} className={styles.mark}>
            {ch}
          </mark>
        ) : (
          ch
        )
      )}
    </>
  );
}

/** Ctrl+K palette: type to fuzzy-filter, arrows to move, Enter to run. */
export function CommandPalette({
  commands,
  onClose
}: {
  commands: readonly PaletteCommand[];
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const listRef = useRef<HTMLUListElement | null>(null);

  // A layer like Modal/Popover: whatever sits below (the live viewer) stops
  // owning Tab/Escape while the palette is open.
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
      layerStack.remove(seq);
      appInert.release(seq);
      const active = document.activeElement as HTMLElement | null;
      const stillInPalette =
        !active ||
        active === document.body ||
        (dialogNode?.contains(active) ?? false);
      if (stillInPalette) {
        returnFocusTarget(previous, (el) => el.isConnected)?.focus?.();
      }
    };
  }, []);

  const results = useMemo(
    () =>
      rank(commands, query, (c) => ({
        text: c.title,
        keywords: [c.group, ...(c.keywords ?? [])]
      })),
    [commands, query]
  );
  const active = Math.min(cursor, Math.max(results.length - 1, 0));

  useEffect(() => {
    const el = listRef.current?.children[active] as HTMLElement | undefined;
    el?.scrollIntoView?.({ block: "nearest" });
  }, [active]);

  const choose = (i: number) => {
    const hit = results[i];
    if (hit) {
      onClose();
      hit.item.run();
    }
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.nativeEvent.isComposing) {
      return;
    }
    const n = Math.max(results.length, 1);
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setCursor((active + 1) % n);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setCursor((active - 1 + n) % n);
    } else if (e.key === "Enter") {
      e.preventDefault();
      choose(active);
    } else if (e.key === "Escape" && layerStack.isTop(seqRef.current)) {
      e.preventDefault();
      e.stopPropagation();
      onClose();
    } else if (e.key === "Tab" && dialogRef.current) {
      const node = dialogRef.current;
      const items = [...node.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((el) =>
        isTabStop({
          tabIndex: el.tabIndex,
          rects: el.getClientRects().length,
          inert: el.closest("[inert]") !== null
        })
      );
      const current = document.activeElement as HTMLElement | null;
      const target = trapTarget(items, current, node, e.shiftKey);
      if (target === "none") {
        e.preventDefault();
        inputRef.current?.focus();
      } else if (target) {
        e.preventDefault();
        target.focus();
      }
    }
  };

  // Portaled to <body>: a Modal makes #root inert, and a palette inside it
  // would paint above the modal (z-index 60) yet could not take focus or keys.
  return createPortal(
    <div
      className={styles.backdrop}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) {
          onClose();
        }
      }}
      data-testid="command-palette"
    >
      <div
        ref={dialogRef}
        className={styles.dialog}
        role="dialog"
        aria-modal="true"
        aria-label={t("palette.title")}
        onKeyDown={onKeyDown}
      >
        <input
          ref={inputRef}
          className={styles.input}
          value={query}
          placeholder={t("palette.placeholder")}
          aria-label={t("palette.placeholder")}
          role="combobox"
          aria-expanded="true"
          aria-controls="palette-list"
          aria-activedescendant={
            results[active] ? `palette-${results[active].item.id}` : undefined
          }
          onChange={(e) => {
            setQuery(e.target.value);
            setCursor(0);
          }}
        />
        <ul
          id="palette-list"
          className={styles.list}
          role="listbox"
          ref={listRef}
        >
          {results.map((r, i) => (
            <li
              key={r.item.id}
              id={`palette-${r.item.id}`}
              role="option"
              aria-selected={i === active}
              className={[styles.row, i === active ? styles.rowActive : ""]
                .filter(Boolean)
                .join(" ")}
              onMouseMove={() => setCursor(i)}
              onClick={() => choose(i)}
            >
              <span className={styles.rowTitle}>
                <Highlighted text={r.item.title} indices={r.indices} />
              </span>
              <span className={styles.rowGroup}>{r.item.group}</span>
            </li>
          ))}
          {results.length === 0 ? (
            <li className={styles.empty} role="presentation">
              {t("palette.empty")}
            </li>
          ) : null}
        </ul>
        <div className={styles.footer}>{t("palette.hint")}</div>
      </div>
    </div>,
    document.body
  );
}
