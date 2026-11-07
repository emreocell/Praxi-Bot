import { type KeyboardEvent, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { rank } from "../lib/fuzzy";
import styles from "./CommandPalette.module.css";

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
  const inputRef = useRef<HTMLInputElement | null>(null);
  const listRef = useRef<HTMLUListElement | null>(null);

  useEffect(() => {
    inputRef.current?.focus();
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
    } else if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    }
  };

  return (
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
    </div>
  );
}
