import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { freshNotices, latestSeq, noticeEntry } from "../lib/ownerNotices";
import { getNotificationHistory, recordNotification } from "../store/notificationHistoryStore";
import { useDaemon } from "./DaemonContext";

const POLL_MS = 20_000;

/**
 * The same digest Telegram and Discord get (a mission or a background run that finished, failed or
 * needs you) in the notifications panel. Polls the daemon's notice list and records what is new;
 * no toast, no model call. Renders nothing.
 */
export function OwnerNoticeSync({ isPrivate }: { isPrivate: boolean }) {
  const { t } = useTranslation();
  const { client, bots } = useDaemon();
  const latest = useRef({ t, isPrivate, bots });
  latest.current = { t, isPrivate, bots };

  useEffect(() => {
    if (!client) {
      return undefined;
    }
    let cancelled = false;
    let after = 0;
    const tick = async () => {
      try {
        const list = await client.ownerNotices(after);
        if (cancelled) return;
        after = latestSeq(list, after);
        const cur = latest.current;
        for (const n of freshNotices(list, getNotificationHistory())) {
          const bot = cur.bots.find((b) => b.id === n.bot_id);
          recordNotification(
            noticeEntry(n, (k, v) => cur.t(k, v) as string, {
              private: cur.isPrivate,
              ...(bot ? { botName: bot.name } : {})
            })
          );
        }
      } catch {
        // the daemon is busy or older than the app: try again at the next tick
      }
    };
    void tick();
    const timer = window.setInterval(() => void tick(), POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [client]);

  return null;
}
