import { CalendarDays } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { BotAvatar, Card } from "ui";
import { botView } from "../lib/botProfile";
import {
  buildDigest,
  startOfLocalDay,
  type BotDigest,
  type DigestLimit,
  type DigestUsage
} from "../lib/dailyDigest";
import { formatUsageCost, localTimeZone } from "../lib/usage";
import { useDaemon } from "../state/DaemonContext";
import styles from "./DailyDigestCard.module.css";

/** "Today": what each bot did since midnight and what it cost, in plain words (no model call). */
export function DailyDigestCard({ refreshKey }: { refreshKey: number }) {
  const { t, i18n } = useTranslation();
  const { client, bots, runs } = useDaemon();
  const [usage, setUsage] = useState<DigestUsage[]>([]);
  const [limits, setLimits] = useState<ReadonlyMap<string, DigestLimit>>(new Map());
  const [ready, setReady] = useState(false);
  const dayStartMs = startOfLocalDay(new Date());

  useEffect(() => {
    if (!client) {
      return undefined;
    }
    let cancelled = false;
    void (async () => {
      try {
        const tz = localTimeZone();
        const summary = await client.usage(
          tz ? { since_ms: dayStartMs, tz } : { since_ms: dayStartMs }
        );
        const found = await Promise.allSettled(
          summary.by_bot.map(async (u) => [u.bot_id, await client.getBotSpendLimits(u.bot_id)] as const)
        );
        const next = new Map<string, DigestLimit>();
        for (const f of found) {
          if (f.status !== "fulfilled") {
            continue;
          }
          const [botId, s] = f.value;
          const daily = s.limits.daily;
          const fallback = s.is_default;
          next.set(botId, {
            hardUsd: daily?.hard_usd ?? (fallback ? s.default_daily_hard_usd : null),
            hardTokens: daily?.hard_tokens ?? (fallback ? s.default_daily_hard_tokens : null)
          });
        }
        if (!cancelled) {
          setUsage(summary.by_bot);
          setLimits(next);
          setReady(true);
        }
      } catch {
        // The full usage tables below show the error; the digest just stays empty.
        if (!cancelled) {
          setReady(true);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, dayStartMs, refreshKey]);

  const rows = useMemo(
    () => buildDigest({ runs, usage, limits, dayStartMs }).filter((r) => bots.some((b) => b.id === r.botId)),
    [runs, usage, limits, dayStartMs, bots]
  );
  const nf = useMemo(() => new Intl.NumberFormat(i18n.language), [i18n.language]);
  const percent = new Intl.NumberFormat(i18n.language, { style: "percent", maximumFractionDigits: 0 });

  const chips = (r: BotDigest): { key: string; text: string; tone?: "warn" | "bad" }[] => {
    const out: { key: string; text: string; tone?: "warn" | "bad" }[] = [];
    if (r.waiting > 0) out.push({ key: "waiting", text: t("digest.waiting", { count: r.waiting }), tone: "warn" });
    if (r.failed > 0) out.push({ key: "failed", text: t("digest.failed", { count: r.failed }), tone: "bad" });
    if (r.running > 0) out.push({ key: "running", text: t("digest.running", { count: r.running }) });
    if (r.done > 0) out.push({ key: "done", text: t("digest.done", { count: r.done }) });
    if (r.stopped > 0) out.push({ key: "stopped", text: t("digest.stopped", { count: r.stopped }) });
    return out;
  };

  return (
    <Card padding="md" data-testid="daily-digest">
      <div className={styles.head}>
        <CalendarDays className={styles.headIcon} size={16} aria-hidden="true" />
        <h2 className={styles.title}>{t("digest.title")}</h2>
      </div>
      <p className={styles.subtitle}>{t("digest.subtitle")}</p>
      {ready && rows.length === 0 ? <p className={styles.empty}>{t("digest.empty")}</p> : null}
      <ul className={styles.list}>
        {rows.map((r) => {
          const bot = bots.find((b) => b.id === r.botId);
          const view = bot ? botView(bot) : null;
          const cost =
            r.calls > 0 ? formatUsageCost(r.costUsd, i18n.language, t("usage.unknown")) : null;
          return (
            <li key={r.botId} className={styles.row}>
              <div className={styles.who}>
                <BotAvatar bot={view ?? { id: "unknown", name: "Bot" }} size="sm" decorative />
                <span className={styles.name}>{view?.name}</span>
              </div>
              <div className={styles.chips}>
                {chips(r).map((c) => (
                  <span key={c.key} className={styles.chip} data-tone={c.tone}>
                    {c.text}
                  </span>
                ))}
              </div>
              <p className={styles.cost}>
                {cost === null
                  ? t("digest.noCalls")
                  : t("digest.cost", { calls: nf.format(r.calls), cost })}
                {r.limitUsed !== null ? ` ${t("digest.ofLimit", { percent: percent.format(r.limitUsed) })}` : ""}
              </p>
            </li>
          );
        })}
      </ul>
    </Card>
  );
}
