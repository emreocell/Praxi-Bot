import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { buildDigest, startOfLocalDay } from "../lib/dailyDigest";
import { dayKey, digestText, planDigest } from "../lib/digestNotify";
import type { QuietHours } from "../lib/quietHours";
import { showNotification } from "../lib/shell";
import { formatUsageCost, localTimeZone } from "../lib/usage";
import { recordNotification } from "../store/notificationHistoryStore";
import { useDaemon } from "./DaemonContext";

const CHECK_EVERY_MS = 60_000;

/**
 * The optional daily digest notification (#589). Once a minute it asks `planDigest` whether the
 * owner's chosen time has come; if so it builds the summary from the run list and today's usage
 * (no model call) and shows it, or, inside quiet hours, only keeps it in the notification
 * history. Renders nothing.
 */
export function DigestNotifier({
  digestTime,
  digestLastDay,
  quietHours,
  isPrivate,
  onHandled
}: {
  digestTime: string;
  digestLastDay: string;
  quietHours: QuietHours;
  isPrivate: boolean;
  /** Called with the local day (YYYY-MM-DD) once today's digest is handled. */
  onHandled: (day: string) => void;
}) {
  const { t, i18n } = useTranslation();
  const { client, runs } = useDaemon();
  const latest = useRef({ digestTime, digestLastDay, quietHours, isPrivate, client, runs, t, onHandled });
  latest.current = { digestTime, digestLastDay, quietHours, isPrivate, client, runs, t, onHandled };
  const busy = useRef(false);
  const language = i18n.language;

  useEffect(() => {
    if (digestTime === "") {
      return undefined;
    }
    const tick = async () => {
      const cur = latest.current;
      const now = new Date();
      const plan = planDigest(now, cur.digestTime, cur.digestLastDay, cur.quietHours);
      if (plan === "wait" || busy.current || !cur.client) {
        return;
      }
      busy.current = true;
      try {
        if (plan !== "skip") {
          const dayStartMs = startOfLocalDay(now);
          const tz = localTimeZone();
          const summary = await cur.client.usage(tz ? { since_ms: dayStartMs, tz } : { since_ms: dayStartMs });
          const rows = buildDigest({ runs: cur.runs, usage: summary.by_bot, limits: new Map(), dayStartMs });
          const text = digestText(
            rows,
            { private: cur.isPrivate, cost: (usd) => formatUsageCost(usd, language, "") },
            (k, v) => cur.t(k, v) as string
          );
          if (text) {
            const shown = plan === "show";
            recordNotification({
              id: `digest-${dayKey(now)}`,
              kind: "digest",
              title: text.title,
              body: text.body,
              botId: "",
              botName: "",
              chatId: "",
              ts: now.getTime(),
              shown,
              ...(shown ? {} : { heldBy: "quiet" as const }),
              read: false
            });
            if (shown) {
              await showNotification(text).catch(() => {});
            }
          }
        }
        cur.onHandled(dayKey(now));
      } catch {
        // Usage was not reachable: try again on the next tick instead of losing the day.
      } finally {
        busy.current = false;
      }
    };
    void tick();
    const id = window.setInterval(() => void tick(), CHECK_EVERY_MS);
    return () => window.clearInterval(id);
  }, [digestTime, language]);

  return null;
}
