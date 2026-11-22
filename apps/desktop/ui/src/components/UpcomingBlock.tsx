import { Clock, Zap } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Schedule } from "../api/types";
import { type UpcomingRow, upcomingRows } from "../lib/activityPanel";
import { affectsSchedules, toggleInput } from "../lib/schedules";
import { useDaemon } from "../state/DaemonContext";
import styles from "./UpcomingBlock.module.css";

/** The bot's scheduled runs and triggers ("Yaklaşanlar"), live; empty when it has none. */
export function useUpcoming(botId: string): {
  rows: UpcomingRow[];
  toggle: (id: string) => Promise<void>;
} {
  const { client, onDaemonEvent } = useDaemon();
  const [schedules, setSchedules] = useState<Schedule[]>([]);
  const load = useCallback(async () => {
    if (!client) {
      return;
    }
    try {
      setSchedules(await client.listSchedules());
    } catch {
      // the schedules page shows the error; the panel simply has nothing to say
    }
  }, [client]);
  useEffect(() => {
    void load();
  }, [load]);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => {
    const off = onDaemonEvent((env) => {
      if (affectsSchedules(env.event)) {
        window.clearTimeout(timer.current);
        timer.current = window.setTimeout(() => void load(), 300);
      }
    });
    return () => {
      off();
      window.clearTimeout(timer.current);
    };
  }, [onDaemonEvent, load]);
  const toggle = async (id: string) => {
    const s = schedules.find((x) => x.id === id);
    if (!client || !s) {
      return;
    }
    await client.updateSchedule(id, toggleInput(s));
    await load();
  };
  return { rows: upcomingRows(schedules, botId), toggle };
}

function whenLabel(ms: number, lang: string): string {
  const d = new Date(ms);
  const sameDay = d.toDateString() === new Date().toDateString();
  return sameDay
    ? d.toLocaleTimeString(lang, { hour: "2-digit", minute: "2-digit" })
    : d.toLocaleString(lang, { weekday: "short", hour: "2-digit", minute: "2-digit" });
}

export function UpcomingBlock({
  rows,
  onToggle
}: {
  rows: readonly UpcomingRow[];
  onToggle: (id: string) => Promise<void>;
}) {
  const { t, i18n } = useTranslation();
  return (
    <section className={styles.block} aria-labelledby="bot-panel-upcoming" data-testid="bot-panel-upcoming">
      <h3 className={styles.label} id="bot-panel-upcoming">
        {t("botPanel.upcoming.title")}
        <span className={styles.count}>{rows.length}</span>
      </h3>
      <ul className={styles.card}>
        {rows.map((row) => {
          const Icon = row.nextMs === undefined ? Zap : Clock;
          const sub = !row.enabled
            ? t("botPanel.upcoming.paused")
            : row.nextMs !== undefined
              ? whenLabel(row.nextMs, i18n.language)
              : t("botPanel.upcoming.trigger");
          return (
            <li key={row.id} className={styles.row}>
              <Icon className={styles.icon} aria-hidden="true" />
              <span className={styles.text}>
                <span className={styles.name}>{row.name}</span>
                <span className={styles.sub}>{sub}</span>
              </span>
              <button
                type="button"
                role="switch"
                aria-checked={row.enabled}
                aria-label={t("botPanel.upcoming.toggle", { name: row.name })}
                title={t("botPanel.upcoming.toggle", { name: row.name })}
                className={styles.toggle}
                onClick={() => void onToggle(row.id)}
              />
            </li>
          );
        })}
      </ul>
    </section>
  );
}
