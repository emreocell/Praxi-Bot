import { ChevronDown, Search, type LucideIcon } from "lucide-react";
import { type ReactNode, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { needsSearch, rowWindow } from "../lib/panelRows";
import styles from "./PanelRows.module.css";

/** A titled group of cards in a right-panel tab ("Browser", "What it knows"). */
export function PanelGroup({ title, action, testId, children }: { title: string; action?: ReactNode; testId?: string; children: ReactNode }) {
  const id = useId();
  return (
    <section className={styles.group} aria-labelledby={id} data-testid={testId}>
      <div className={styles.groupHead}>
        <h3 className={styles.groupTitle} id={id}>
          {title}
        </h3>
        {action}
      </div>
      {children}
    </section>
  );
}

export type StatusTone = "ok" | "warn" | "danger" | "muted";

/** A short state next to a name: a dot in the state's colour plus words (colour never carries it alone). */
export function StatusChip({ tone, children, testId }: { tone: StatusTone; children: ReactNode; testId?: string }) {
  return (
    <span className={styles.status} data-tone={tone} data-testid={testId}>
      <span className={styles.dot} aria-hidden="true" />
      {children}
    </span>
  );
}

export interface PanelRow {
  id: string;
  name: string;
  icon?: LucideIcon;
  /** Second, quieter column ("42 files", "Saved page"). */
  meta?: string;
  /** A state that is not the normal one ("Off"); the normal state is left unsaid. */
  status?: { tone: StatusTone; label: string };
  /** The row opens something. */
  onOpen?: () => void;
  /** One icon button at the end of the row (forget, remove). */
  action?: { label: string; icon: LucideIcon; onClick: () => void; disabled?: boolean };
}

/**
 * One line per item. Shows the first `limit` rows and a "Show all" toggle; from `searchFrom` rows
 * on, a search field filters the whole list (and shows every match). The count is announced when
 * the list is filtered.
 */
export function PanelRows({
  rows,
  label,
  searchLabel,
  limit = 4,
  searchFrom = 8,
  footer,
  testId
}: {
  rows: readonly PanelRow[];
  /** Names the list for assistive technology. */
  label: string;
  searchLabel: string;
  limit?: number;
  searchFrom?: number;
  /** Sits on the line of the "Show all" toggle, at its end ("Manage skills"). */
  footer?: ReactNode;
  testId?: string;
}) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const [all, setAll] = useState(false);
  const searching = query.trim().length > 0;
  const { shown, hidden, found } = rowWindow(rows, { query, expanded: all, limit });

  return (
    <div className={styles.list} data-testid={testId}>
      {needsSearch(rows.length, searchFrom) ? (
        <label className={styles.search}>
          <Search aria-hidden="true" />
          <input
            type="search"
            value={query}
            placeholder={searchLabel}
            aria-label={searchLabel}
            data-testid={testId ? `${testId}-search` : undefined}
            onChange={(e) => setQuery(e.target.value)}
          />
        </label>
      ) : null}
      {shown.length === 0 ? (
        <p className={styles.none} role="status">
          {t("panelTabs.list.noMatches")}
        </p>
      ) : (
        <ul className={styles.rows} aria-label={label}>
          {shown.map((row) => {
            const Icon = row.icon;
            const body = (
              <>
                {Icon ? <Icon aria-hidden="true" /> : null}
                <span className={styles.name} title={row.name}>
                  {row.name}
                </span>
                {row.meta ? <span className={styles.meta}>{row.meta}</span> : null}
                {row.status ? <StatusChip tone={row.status.tone}>{row.status.label}</StatusChip> : null}
              </>
            );
            const ActionIcon = row.action?.icon;
            return (
              <li key={row.id} className={styles.row}>
                {row.onOpen ? (
                  <button type="button" className={styles.main} onClick={row.onOpen}>
                    {body}
                  </button>
                ) : (
                  <div className={styles.main}>{body}</div>
                )}
                {row.action && ActionIcon ? (
                  <button
                    type="button"
                    className={styles.action}
                    title={row.action.label}
                    aria-label={row.action.label}
                    disabled={row.action.disabled}
                    onClick={row.action.onClick}
                  >
                    <ActionIcon aria-hidden="true" />
                  </button>
                ) : null}
              </li>
            );
          })}
        </ul>
      )}
      {searching ? (
        <span className={styles.srOnly} role="status">
          {t("panelTabs.list.found", { count: found })}
        </span>
      ) : null}
      {(!searching && (hidden > 0 || (all && rows.length > limit))) || footer ? (
        <div className={styles.foot}>
          {!searching && (hidden > 0 || (all && rows.length > limit)) ? (
            <button type="button" className={styles.toggle} aria-expanded={all} onClick={() => setAll((a) => !a)}>
              {all ? t("panelTabs.list.showFewer") : t("panelTabs.list.showAll", { count: rows.length })}
              <ChevronDown aria-hidden="true" data-open={all ? "true" : undefined} />
            </button>
          ) : null}
          {footer ? <span className={styles.footEnd}>{footer}</span> : null}
        </div>
      ) : null}
    </div>
  );
}
