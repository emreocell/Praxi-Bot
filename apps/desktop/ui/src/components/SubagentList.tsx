import { Square } from "lucide-react";
import { type CSSProperties, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { brandIcon } from "../lib/brandIcons";
import { formatElapsed, type SubagentRow } from "../lib/activityPanel";
import { describeTool, stepPhase } from "../lib/toolLabels";
import styles from "./SubagentList.module.css";

const brandName = (name: string): string | null => {
  const b = brandIcon(name);
  return b.known ? b.name : null;
};

/** One human sentence for what a sub-agent is doing right now. */
export function stepSentence(t: TFunction, row: SubagentRow): string {
  if (!row.step) {
    return row.state === "running" ? t("botPanel.subagents.starting") : "";
  }
  const label = describeTool(row.step.name, row.step.input, brandName);
  const text = t(`${label.key}.${stepPhase(row.step.status)}`, label.values);
  return row.state === "waiting" ? t("botPanel.subagents.waitingFor", { step: text }) : text;
}

/** Re-render every second while something runs (elapsed times). */
export function useNow(active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) {
      return undefined;
    }
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [active]);
  return now;
}

export function taskTitle(t: TFunction, task: string, max = 56): string {
  if (!task) {
    return t("botPanel.subagents.noTask");
  }
  return task.length > max ? `${task.slice(0, max - 1)}…` : task;
}

/** The compact live list of sub-agents. The caller renders it only with rows. */
export function SubagentList({
  rows,
  onOpen,
  onStop
}: {
  rows: readonly SubagentRow[];
  onOpen: (id: string) => void;
  onStop: (id: string) => void;
}) {
  const { t } = useTranslation();
  const now = useNow(rows.some((r) => r.canStop));
  return (
    <section className={styles.block} aria-labelledby="bot-panel-subagents" data-testid="bot-panel-subagents">
      <h3 className={styles.label} id="bot-panel-subagents">
        {t("botPanel.subagents.title")}
      </h3>
      <ul className={styles.card}>
        {rows.map((row) => {
          const title = taskTitle(t, row.task);
          const sentence = stepSentence(t, row);
          return (
            <li
              key={row.id}
              className={styles.row}
              style={{ "--depth": row.depth } as CSSProperties}
              data-state={row.state}
              data-testid="subagent-row"
            >
              <button
                type="button"
                className={styles.open}
                aria-label={t("botPanel.subagents.open", { task: title })}
                onClick={() => onOpen(row.id)}
              >
                <i className={styles.dot} data-state={row.state} aria-hidden="true" />
                <span className={styles.text}>
                  <span className={styles.title}>{title}</span>
                  <span className={styles.sub}>
                    <span className={styles.state} data-state={row.state}>
                      {t(`botPanel.subagents.state.${row.state}`)}
                    </span>
                    {sentence ? <span> · {sentence}</span> : null}
                  </span>
                </span>
                <span className={styles.time}>{formatElapsed((row.endedMs ?? now) - row.startedMs)}</span>
              </button>
              {row.canStop ? (
                <button
                  type="button"
                  className={styles.stop}
                  title={t("botPanel.subagents.stop")}
                  aria-label={t("botPanel.subagents.stopLabel", { task: title })}
                  onClick={() => onStop(row.id)}
                >
                  <Square aria-hidden="true" />
                </button>
              ) : null}
            </li>
          );
        })}
      </ul>
    </section>
  );
}
