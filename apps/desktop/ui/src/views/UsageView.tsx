import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Avatar, Button, Card } from "ui";
import type { UsageSummary, UsageTotals } from "../api/types";
import formStyles from "../components/Form.module.css";
import { botView } from "../lib/botProfile";
import { useDaemon } from "../state/DaemonContext";
import styles from "./UsageView.module.css";

type Range = "all" | "day" | "week" | "month";
const RANGE_MS: Record<Exclude<Range, "all">, number> = {
  day: 24 * 3600 * 1000,
  week: 7 * 24 * 3600 * 1000,
  month: 30 * 24 * 3600 * 1000
};

function totalTokens(u: UsageTotals): number {
  return u.prompt_tokens + u.completion_tokens;
}

export function UsageView() {
  const { t, i18n } = useTranslation();
  const { client, bots, events } = useDaemon();
  const [range, setRange] = useState<Range>("all");
  const [usage, setUsage] = useState<UsageSummary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const nf = new Intl.NumberFormat(i18n.language);
  const usd = new Intl.NumberFormat(i18n.language, {
    style: "currency",
    currency: "USD",
    maximumFractionDigits: 4
  });
  const money = (v?: number | null) =>
    v === null || v === undefined ? "—" : usd.format(v);

  const load = useCallback(async () => {
    if (!client) {
      return;
    }
    try {
      const since = range === "all" ? undefined : Date.now() - RANGE_MS[range];
      setUsage(await client.usage(since));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [client, range]);

  // reload on range change and whenever a run finishes
  const finished = Object.values(events.runs).filter(
    (r) => r.finishedMs !== undefined
  ).length;
  useEffect(() => {
    void load();
  }, [load, finished]);

  const rows = (usage?.by_bot ?? []).map((u) => {
    const bot = bots.find((b) => b.id === u.bot_id);
    return { usage: u, bot, view: bot ? botView(bot) : null };
  });

  return (
    <div className={styles.pane}>
      <div className={styles.container}>
        <header className={styles.header}>
          <h1 className={styles.title}>{t("usage.title")}</h1>
          <p className={styles.subtitle}>{t("usage.subtitle")}</p>
        </header>

        <div className={styles.rangeRow} role="group" aria-label={t("usage.range")}>
          {(["all", "day", "week", "month"] as const).map((r) => (
            <Button
              key={r}
              size="sm"
              variant="secondary"
              active={range === r}
              onClick={() => setRange(r)}
            >
              {t(`usage.ranges.${r}`)}
            </Button>
          ))}
          <Button size="sm" variant="ghost" onClick={() => void load()}>
            {t("usage.refresh")}
          </Button>
        </div>

        {error ? (
          <p className={formStyles.error} role="alert">
            {error}
          </p>
        ) : null}

        {usage ? (
          <>
            <div className={styles.totals} data-testid="usage-totals">
              <Card padding="md" className={styles.stat}>
                <span className={styles.statLabel}>{t("usage.runs")}</span>
                <span className={styles.statValue}>{nf.format(usage.runs)}</span>
              </Card>
              <Card padding="md" className={styles.stat}>
                <span className={styles.statLabel}>{t("usage.modelCalls")}</span>
                <span className={styles.statValue}>
                  {nf.format(usage.total.model_calls)}
                </span>
              </Card>
              <Card padding="md" className={styles.stat}>
                <span className={styles.statLabel}>{t("usage.tokens")}</span>
                <span className={styles.statValue}>
                  {nf.format(totalTokens(usage.total))}
                </span>
              </Card>
              <Card padding="md" className={styles.stat}>
                <span className={styles.statLabel}>{t("usage.cost")}</span>
                <span className={styles.statValue} data-testid="usage-cost">
                  {money(usage.total.cost_usd)}
                </span>
              </Card>
            </div>

            {rows.length === 0 ? (
              <p className={formStyles.hint}>{t("usage.empty")}</p>
            ) : (
              <Card padding="none" className={styles.tableCard}>
                <table className={styles.table}>
                  <thead>
                    <tr>
                      <th scope="col">{t("usage.bot")}</th>
                      <th scope="col" className={styles.num}>
                        {t("usage.calls")}
                      </th>
                      <th scope="col" className={styles.num}>
                        {t("usage.prompt")}
                      </th>
                      <th scope="col" className={styles.num}>
                        {t("usage.completion")}
                      </th>
                      <th scope="col" className={styles.num}>
                        {t("usage.cached")}
                      </th>
                      <th scope="col" className={styles.num}>
                        {t("usage.total")}
                      </th>
                      <th scope="col" className={styles.num}>
                        {t("usage.cost")}
                      </th>
                    </tr>
                  </thead>
                  <tbody>
                    {rows.map(({ usage: u, view }) => (
                      <tr key={u.bot_id}>
                        <th scope="row">
                          <div className={styles.botCell}>
                          <Avatar
                            name={view?.name ?? "?"}
                            src={view?.avatar}
                            color={view?.color}
                            size="sm"
                          />
                          <span>{view?.name ?? t("usage.deletedBot")}</span>
                          </div>
                        </th>
                        <td className={styles.num}>{nf.format(u.model_calls)}</td>
                        <td className={styles.num}>{nf.format(u.prompt_tokens)}</td>
                        <td className={styles.num}>
                          {nf.format(u.completion_tokens)}
                        </td>
                        <td className={styles.num}>{nf.format(u.cached_tokens)}</td>
                        <td className={styles.num}>{nf.format(totalTokens(u))}</td>
                        <td className={styles.num}>{money(u.cost_usd)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </Card>
            )}
            <p className={formStyles.hint}>{t("usage.costNote")}</p>
          </>
        ) : null}
      </div>
    </div>
  );
}
