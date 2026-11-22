import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Card } from "ui";
import type { DailyUsage } from "../api/types";
import {
  buildDailyBars,
  formatTokenCount,
  formatUsageCost,
  isAllUnknownUsage,
  shortDayLabel,
  totalTokens,
  type TrendMetric
} from "../lib/usage";
import styles from "./UsageTrendChart.module.css";

const PLOT_W = 560;
const PLOT_H = 104;
const PAD_TOP = 10;
const PAD_BOTTOM = 24;
const VIEW_H = PAD_TOP + PLOT_H + PAD_BOTTOM;

const METRICS: readonly TrendMetric[] = ["tokens", "calls", "cost"];

export interface UsageTrendChartProps {
  daily: readonly DailyUsage[];
  timeZone?: string;
}

/** Inline-SVG daily bar chart using theme tokens, plus an accessible table behind it. */
export function UsageTrendChart({ daily, timeZone }: UsageTrendChartProps) {
  const { t, i18n } = useTranslation();
  const [metric, setMetric] = useState<TrendMetric>("tokens");
  const [activeIndex, setActiveIndex] = useState<number | null>(null);
  const nf = useMemo(() => new Intl.NumberFormat(i18n.language), [i18n.language]);
  const unknownLabel = t("usage.unknown");

  const model = useMemo(
    () => buildDailyBars(daily, metric, PLOT_W, PLOT_H),
    [daily, metric]
  );

  if (daily.length === 0) {
    return null;
  }

  const active =
    activeIndex !== null && activeIndex >= 0 && activeIndex < model.bars.length
      ? model.bars[activeIndex]
      : null;

  const ariaSummary = t("usage.trend.ariaLabel", {
    days: daily.length,
    metric: t(`usage.trend.metrics.${metric}`)
  });

  const metricReadout = (bar: (typeof model.bars)[number]) => {
    const tokensText = formatTokenCount(
      bar.tokens,
      isAllUnknownUsage({ model_calls: bar.calls, calls_without_usage: bar.callsWithoutUsage }),
      i18n.language,
      unknownLabel
    );
    const costText = formatUsageCost(bar.costUsd, i18n.language, unknownLabel);
    return t("usage.trend.dayReadout", {
      day: shortDayLabel(bar.day, i18n.language),
      calls: nf.format(bar.calls),
      tokens: tokensText,
      cost: costText
    });
  };

  return (
    <Card padding="md" className={styles.card} data-testid="usage-daily-trend">
      <div className={styles.header}>
        <div className={styles.titleBlock}>
          <h2 className={styles.title}>{t("usage.trend.title")}</h2>
          {timeZone ? (
            <span className={styles.meta}>
              {t("usage.trend.timeZone", { tz: timeZone })}
            </span>
          ) : null}
        </div>
        <div
          className={styles.metricGroup}
          role="group"
          aria-label={t("usage.trend.metric")}
        >
          {METRICS.map((m) => (
            <Button
              key={m}
              size="sm"
              variant="secondary"
              active={metric === m}
              onClick={() => setMetric(m)}
            >
              {t(`usage.trend.metrics.${m}`)}
            </Button>
          ))}
        </div>
      </div>

      <p className={styles.readout} aria-live="polite">
        {active ? metricReadout(active) : t("usage.trend.hoverHint")}
      </p>

      <div className={styles.chartWrap}>
        <svg
          className={styles.svg}
          viewBox={`0 0 ${PLOT_W} ${VIEW_H}`}
          role="group"
          aria-label={ariaSummary}
          onMouseLeave={() => setActiveIndex(null)}
        >
          <line
            className={styles.gridLine}
            x1={0}
            x2={PLOT_W}
            y1={PAD_TOP}
            y2={PAD_TOP}
          />
          <line
            className={styles.gridLine}
            x1={0}
            x2={PLOT_W}
            y1={PAD_TOP + PLOT_H / 2}
            y2={PAD_TOP + PLOT_H / 2}
          />
          <line
            className={styles.axisLine}
            x1={0}
            x2={PLOT_W}
            y1={PAD_TOP + PLOT_H}
            y2={PAD_TOP + PLOT_H}
          />

          {model.bars.map((bar, index) => {
            const isActive = activeIndex === index;
            const barClass = bar.unknownOnly
              ? styles.barUnknown
              : isActive
                ? styles.barActive
                : styles.bar;
            const slotWidth = PLOT_W / model.bars.length;
            return (
              <g
                key={bar.day}
                className={styles.barGroup}
                tabIndex={0}
                role="button"
                aria-label={metricReadout(bar)}
                onMouseEnter={() => setActiveIndex(index)}
                onFocus={() => setActiveIndex(index)}
                onBlur={() => setActiveIndex(null)}
              >
                <rect
                  className={styles.hitArea}
                  x={index * slotWidth}
                  y={PAD_TOP}
                  width={slotWidth}
                  height={PLOT_H}
                />
                {bar.height > 0 ? (
                  <rect
                    className={barClass}
                    x={bar.x}
                    y={PAD_TOP + bar.y}
                    width={bar.width}
                    height={bar.height}
                    rx={2}
                  />
                ) : null}
              </g>
            );
          })}

          {model.labelIndices.map((idx) => {
            const bar = model.bars[idx];
            if (!bar) {
              return null;
            }
            const anchor =
              idx === 0
                ? "start"
                : idx === model.bars.length - 1
                  ? "end"
                  : "middle";
            const x =
              idx === 0
                ? 0
                : idx === model.bars.length - 1
                  ? PLOT_W
                  : bar.x + bar.width / 2;
            return (
              <text
                key={bar.day}
                className={styles.axisLabel}
                x={x}
                y={PAD_TOP + PLOT_H + 16}
                textAnchor={anchor}
              >
                {shortDayLabel(bar.day, i18n.language)}
              </text>
            );
          })}
        </svg>
      </div>

      <details className={styles.details}>
        <summary className={styles.summary}>{t("usage.trend.showTable")}</summary>
        <div className={styles.tableWrap}>
          <table className={styles.table}>
            <thead>
              <tr>
                <th scope="col">{t("usage.day")}</th>
                <th scope="col" className={styles.num}>
                  {t("usage.calls")}
                </th>
                <th scope="col" className={styles.num}>
                  {t("usage.tokens")}
                </th>
                <th scope="col" className={styles.num}>
                  {t("usage.cost")}
                </th>
              </tr>
            </thead>
            <tbody>
              {[...daily].reverse().map((d) => {
                const unknownTokens = isAllUnknownUsage(d);
                return (
                  <tr key={d.day}>
                    <th scope="row">{d.day}</th>
                    <td className={styles.num}>{nf.format(d.model_calls)}</td>
                    <td className={styles.num}>
                      {formatTokenCount(
                        totalTokens(d),
                        unknownTokens,
                        i18n.language,
                        unknownLabel
                      )}
                    </td>
                    <td className={styles.num}>
                      {d.model_calls === 0
                        ? "—"
                        : formatUsageCost(d.cost_usd, i18n.language, unknownLabel)}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </details>
    </Card>
  );
}
