import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Card } from "ui";
import type { BotUsage, ModelUsage } from "../api/types";
import { shareRows } from "../lib/costInsights";
import { formatTokenCount, formatUsageCost } from "../lib/usage";
import styles from "./CostBreakdown.module.css";

export interface CostBreakdownProps {
  byBot: readonly BotUsage[];
  byModel: readonly ModelUsage[];
  /** Bot id to display name; unknown ids are shown as a deleted bot. */
  botName: (botId: string) => string | null;
}

type Metric = "cost" | "tokens";

function Bars({ title, rows, metric }: { title: string; rows: ReturnType<typeof shareRows>; metric: Metric }) {
  const { t, i18n } = useTranslation();
  const unknown = t("usage.unknown");
  return (
    <div className={styles.group}>
      <h3 className={styles.groupTitle}>
        {title}
      </h3>
      <ul className={styles.list}>
        {rows.map((r) => {
          const value =
            metric === "cost"
              ? formatUsageCost(r.cost, i18n.language, unknown)
              : formatTokenCount(r.tokens, r.tokensUnknown, i18n.language, unknown);
          // A row whose value is unknown has no bar: it is never drawn as zero.
          const pct = r.fraction === null ? 0 : Math.max(2, r.fraction * 100);
          return (
            <li key={r.key} className={styles.row}>
              <span className={styles.name} title={r.key}>
                {r.key}
              </span>
              <svg className={styles.bar} viewBox="0 0 100 6" preserveAspectRatio="none" aria-hidden="true">
                <rect className={styles.track} x="0" y="0" width="100" height="6" rx="1.5" />
                {pct > 0 ? <rect className={styles.fill} x="0" y="0" width={pct} height="6" rx="1.5" /> : null}
              </svg>
              <span className={styles.value}>{value}</span>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/** Spend or tokens by bot and by model as bars; the tables below on the page carry the same numbers. */
export function CostBreakdown({ byBot, byModel, botName }: CostBreakdownProps) {
  const { t } = useTranslation();
  const [metric, setMetric] = useState<Metric>("cost");
  const deleted = t("usage.deletedBot");
  const botRows = useMemo(
    () => shareRows(byBot.map((b) => ({ key: botName(b.bot_id) ?? deleted, totals: b })), metric),
    [byBot, botName, metric, deleted]
  );
  const modelRows = useMemo(
    () =>
      shareRows(
        byModel.map((m) => ({ key: m.model, totals: m, callsWithoutUsage: m.calls_without_usage })),
        metric
      ),
    [byModel, metric]
  );
  if (botRows.length === 0 && modelRows.length === 0) {
    return null;
  }
  const anyUnknownCost = metric === "cost" && [...botRows, ...modelRows].some((r) => r.cost === null);
  return (
    <Card padding="md" className={styles.card} data-testid="usage-breakdown">
      <div className={styles.header}>
        <h2 className={styles.title}>{t("usage.insights.title")}</h2>
        <div className={styles.toggle} role="group" aria-label={t("usage.trend.metric")}>
          {(["cost", "tokens"] as const).map((m) => (
            <Button key={m} size="sm" variant="secondary" active={metric === m} onClick={() => setMetric(m)}>
              {t(`usage.trend.metrics.${m}`)}
            </Button>
          ))}
        </div>
      </div>
      <div className={styles.groups}>
        {botRows.length > 0 ? <Bars title={t("usage.byBot")} rows={botRows} metric={metric} /> : null}
        {modelRows.length > 0 ? <Bars title={t("usage.byModel")} rows={modelRows} metric={metric} /> : null}
      </div>
      {anyUnknownCost ? <p className={styles.note}>{t("usage.insights.unknownPriceNote")}</p> : null}
    </Card>
  );
}
