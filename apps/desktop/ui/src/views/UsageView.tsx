import { Download, Gauge, Info, LoaderCircle, RefreshCw } from "lucide-react";
import { EmptyState } from "../components/EmptyState";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button, Card } from "ui";
import type { UsageSummary } from "../api/types";
import formStyles from "../components/Form.module.css";
import { QuotaBlock } from "../components/QuotaRings";
import { SpendLimitCard } from "../components/SpendLimitCard";
import { useToast } from "../components/Toast";
import { UsageTrendChart } from "../components/UsageTrendChart";
import { botView } from "../lib/botProfile";
import { humanLine } from "../lib/humanError";
import {
  USAGE_RANGES,
  exportFileName,
  formatTokenCount,
  formatUsageCost,
  isAllUnknownUsage,
  sortModelUsage,
  totalTokens,
  usageQueryForRange,
  type UsageRange
} from "../lib/usage";
import { useDaemon } from "../state/DaemonContext";
import styles from "./UsageView.module.css";

/** `embedded`: inside the settings modal (no page padding / title). */
export function UsageView({ embedded = false }: { embedded?: boolean }) {
  const { t, i18n } = useTranslation();
  const toast = useToast();
  const { client, bots, events } = useDaemon();
  const [range, setRange] = useState<UsageRange>("month");
  const [usage, setUsage] = useState<UsageSummary | null>(null);
  const [loading, setLoading] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const nf = useMemo(() => new Intl.NumberFormat(i18n.language), [i18n.language]);
  const unknownLabel = t("usage.unknown");
  const money = (v?: number | null) =>
    formatUsageCost(v, i18n.language, unknownLabel);

  const load = useCallback(async () => {
    if (!client) {
      return;
    }
    setLoading(true);
    try {
      setUsage(await client.usage(usageQueryForRange(range)));
      setError(null);
    } catch (e) {
      setError(humanLine(e));
    } finally {
      setLoading(false);
    }
  }, [client, range]);

  // reload on range change and whenever a run finishes
  const finished = Object.values(events.runs).filter(
    (r) => r.finishedMs !== undefined
  ).length;
  useEffect(() => {
    void load();
  }, [load, finished]);

  const downloadCsv = async () => {
    if (!client || exporting) {
      return;
    }
    setExporting(true);
    try {
      const csv = await client.usageExport(usageQueryForRange(range));
      const url = URL.createObjectURL(
        new Blob([csv], { type: "text/csv;charset=utf-8" })
      );
      const a = document.createElement("a");
      a.href = url;
      a.download = exportFileName(range);
      a.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 1000);
      toast.show(t("usage.exportDone"), "success");
    } catch (e) {
      toast.show(`${t("usage.exportFailed")} (${humanLine(e)})`, "danger");
    } finally {
      setExporting(false);
    }
  };

  const botRows = (usage?.by_bot ?? []).map((u) => {
    const bot = bots.find((b) => b.id === u.bot_id);
    return { usage: u, bot, view: bot ? botView(bot) : null };
  });
  const modelRows = useMemo(
    () => sortModelUsage(usage?.by_model ?? []),
    [usage?.by_model]
  );

  const callsWithoutUsage = usage?.calls_without_usage ?? 0;
  const callsWithoutCost = usage?.calls_without_cost ?? 0;
  const isEmpty =
    usage !== null && usage.runs === 0 && usage.total.model_calls === 0;
  const totalTokensUnknown =
    usage !== null &&
    isAllUnknownUsage({
      model_calls: usage.total.model_calls,
      calls_without_usage: callsWithoutUsage
    });

  return (
    <div className={embedded ? styles.embeddedPane : styles.pane}>
      <div className={embedded ? styles.embeddedContainer : styles.container}>
        {embedded ? null : (
          <header className={styles.header}>
            <h1 className={styles.title}>{t("usage.title")}</h1>
            <p className={styles.subtitle}>{t("usage.subtitle")}</p>
          </header>
        )}

        <QuotaBlock />

        <SpendLimitCard refreshKey={finished} />

        <div className={styles.toolbar}>
          <div
            className={styles.rangeRow}
            role="group"
            aria-label={t("usage.range")}
          >
            {USAGE_RANGES.map((r) => (
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
          </div>
          <div className={styles.actions}>
            <Button
              size="sm"
              variant="ghost"
              disabled={!client || loading}
              onClick={() => void load()}
            >
              <RefreshCw
                size={14}
                aria-hidden="true"
                className={loading ? styles.spin : undefined}
              />
              {t("usage.refresh")}
            </Button>
            <Button
              size="sm"
              variant="secondary"
              disabled={!client || loading || exporting}
              onClick={() => void downloadCsv()}
              data-testid="usage-export-csv"
            >
              {exporting ? (
                <LoaderCircle size={14} aria-hidden="true" className={styles.spin} />
              ) : (
                <Download size={14} aria-hidden="true" />
              )}
              {t("usage.exportCsv")}
            </Button>
          </div>
        </div>

        {error ? (
          <Card padding="md" className={styles.errorCard}>
            <p className={formStyles.error} role="alert">
              {error}
            </p>
            <Button size="sm" variant="secondary" onClick={() => void load()}>
              {t("usage.retry")}
            </Button>
          </Card>
        ) : null}

        {loading && !usage ? (
          <Card padding="md" className={styles.stateCard} role="status">
            <LoaderCircle size={16} aria-hidden="true" className={styles.spin} />
            <span>{t("usage.loading")}</span>
          </Card>
        ) : null}

        {usage ? (
          <div className={styles.content} aria-busy={loading}>
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
                  {formatTokenCount(
                    totalTokens(usage.total),
                    totalTokensUnknown,
                    i18n.language,
                    unknownLabel
                  )}
                </span>
              </Card>
              <Card padding="md" className={styles.stat}>
                <span className={styles.statLabel}>{t("usage.cost")}</span>
                <span className={styles.statValue} data-testid="usage-cost">
                  {usage.total.model_calls === 0
                    ? "—"
                    : money(usage.total.cost_usd)}
                </span>
              </Card>
            </div>

            {callsWithoutUsage > 0 ||
            (callsWithoutCost > 0 && usage.total.cost_usd !== null && usage.total.cost_usd !== undefined) ? (
              <div className={styles.notices} data-testid="usage-unknown-notice">
                {callsWithoutUsage > 0 ? (
                  <p className={styles.notice}>
                    <Info size={14} aria-hidden="true" />
                    <span>
                      {t("usage.unknownUsageNotice", { count: callsWithoutUsage })}
                    </span>
                  </p>
                ) : null}
                {callsWithoutCost > 0 &&
                usage.total.cost_usd !== null &&
                usage.total.cost_usd !== undefined ? (
                  <p className={styles.notice}>
                    <Info size={14} aria-hidden="true" />
                    <span>
                      {t("usage.unknownCostNotice", { count: callsWithoutCost })}
                    </span>
                  </p>
                ) : null}
              </div>
            ) : null}

            {isEmpty ? (
              <Card padding="md" className={styles.stateCard} data-testid="usage-empty">
                <EmptyState icon={Gauge} title={t("usage.emptyTitle")}>{t("usage.empty")}</EmptyState>
              </Card>
            ) : (
              <>
                <UsageTrendChart
                  daily={usage.daily ?? []}
                  timeZone={usage.time_zone}
                />

                {modelRows.length > 0 ? (
                  <section className={styles.section} aria-labelledby="usage-by-model-h">
                    <h2 id="usage-by-model-h" className={styles.sectionTitle}>
                      {t("usage.byModel")}
                    </h2>
                    <Card
                      padding="none"
                      className={styles.tableCard}
                      data-testid="usage-by-model"
                    >
                      <table className={styles.table}>
                        <thead>
                          <tr>
                            <th scope="col">{t("usage.model")}</th>
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
                          {modelRows.map((m) => {
                            const unknownTokens = isAllUnknownUsage(m);
                            const missingUsage = m.calls_without_usage ?? 0;
                            const missingCost = m.calls_without_cost ?? 0;
                            return (
                              <tr key={m.model}>
                                <th scope="row">
                                  <div className={styles.modelCell}>
                                    <span className={styles.modelName}>{m.model}</span>
                                    {m.provider ? (
                                      <span className={styles.providerTag}>
                                        {m.provider}
                                      </span>
                                    ) : null}
                                  </div>
                                </th>
                                <td className={styles.num}>
                                  <span>{nf.format(m.model_calls)}</span>
                                  {missingUsage > 0 ? (
                                    <span
                                      className={styles.subNote}
                                      title={t("usage.unknownUsageNotice", {
                                        count: missingUsage
                                      })}
                                    >
                                      {t("usage.noUsageShort", { count: missingUsage })}
                                    </span>
                                  ) : null}
                                </td>
                                <td className={styles.num}>
                                  {formatTokenCount(
                                    m.prompt_tokens,
                                    unknownTokens,
                                    i18n.language,
                                    unknownLabel
                                  )}
                                </td>
                                <td className={styles.num}>
                                  {formatTokenCount(
                                    m.completion_tokens,
                                    unknownTokens,
                                    i18n.language,
                                    unknownLabel
                                  )}
                                </td>
                                <td className={styles.num}>
                                  {formatTokenCount(
                                    m.cached_tokens,
                                    unknownTokens,
                                    i18n.language,
                                    unknownLabel
                                  )}
                                </td>
                                <td className={styles.num}>
                                  {formatTokenCount(
                                    totalTokens(m),
                                    unknownTokens,
                                    i18n.language,
                                    unknownLabel
                                  )}
                                </td>
                                <td className={styles.num}>
                                  <span>{money(m.cost_usd)}</span>
                                  {missingCost > 0 &&
                                  m.cost_usd !== null &&
                                  m.cost_usd !== undefined ? (
                                    <span
                                      className={styles.subNote}
                                      title={t("usage.unknownCostNotice", {
                                        count: missingCost
                                      })}
                                    >
                                      {t("usage.partialCostShort", {
                                        count: missingCost
                                      })}
                                    </span>
                                  ) : null}
                                </td>
                              </tr>
                            );
                          })}
                        </tbody>
                      </table>
                    </Card>
                  </section>
                ) : null}

                {botRows.length > 0 ? (
                  <section className={styles.section} aria-labelledby="usage-by-bot-h">
                    <h2 id="usage-by-bot-h" className={styles.sectionTitle}>
                      {t("usage.byBot")}
                    </h2>
                    <Card
                      padding="none"
                      className={styles.tableCard}
                      data-testid="usage-by-bot"
                    >
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
                          {botRows.map(({ usage: u, view }) => (
                            <tr key={u.bot_id}>
                              <th scope="row">
                                <div className={styles.botCell}>
                                  <BotAvatar
                                    bot={view ?? { id: "unknown", name: "Bot" }}
                                    size="sm"
                                    decorative
                                  />
                                  <span>{view?.name ?? t("usage.deletedBot")}</span>
                                </div>
                              </th>
                              <td className={styles.num}>
                                {nf.format(u.model_calls)}
                              </td>
                              <td className={styles.num}>
                                {nf.format(u.prompt_tokens)}
                              </td>
                              <td className={styles.num}>
                                {nf.format(u.completion_tokens)}
                              </td>
                              <td className={styles.num}>
                                {nf.format(u.cached_tokens)}
                              </td>
                              <td className={styles.num}>
                                {nf.format(totalTokens(u))}
                              </td>
                              <td className={styles.num}>{money(u.cost_usd)}</td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </Card>
                  </section>
                ) : null}
              </>
            )}

            <p className={formStyles.hint}>{t("usage.costNote")}</p>
          </div>
        ) : null}
      </div>
    </div>
  );
}
