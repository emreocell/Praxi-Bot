import { ExternalLink } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button, Card } from "ui";
import type { TopRunUsage } from "../api/types";
import { requestOpenChat } from "../lib/openChatLink";
import { formatTokenCount, formatUsageCost, isAllUnknownUsage, totalTokens } from "../lib/usage";
import styles from "./TopRunsCard.module.css";

export interface TopRunsCardProps {
  runs: readonly TopRunUsage[];
  botName: (botId: string) => string | null;
  /** Called with the chat id; defaults to asking the app shell to open it. */
  onOpenChat?: (chatId: string) => void;
}

/** The five most expensive runs of the range. A run of unknown price shows its tokens only. */
export function TopRunsCard({ runs, botName, onOpenChat = requestOpenChat }: TopRunsCardProps) {
  const { t, i18n } = useTranslation();
  if (runs.length === 0) {
    return null;
  }
  const unknown = t("usage.unknown");
  const when = new Intl.DateTimeFormat(i18n.language, { dateStyle: "medium", timeStyle: "short" });
  return (
    <Card padding="md" className={styles.card} data-testid="usage-top-runs">
      <h2 className={styles.title}>{t("usage.insights.topRuns")}</h2>
      <ol className={styles.list}>
        {runs.map((r) => {
          const bot = botName(r.bot_id) ?? t("usage.deletedBot");
          const at = when.format(new Date(r.created_at_ms));
          return (
            <li key={r.run_id} className={styles.row}>
              <div className={styles.info}>
                <span className={styles.bot}>{bot}</span>
                <span className={styles.meta}>{at}</span>
              </div>
              <div className={styles.numbers}>
                <span className={styles.cost}>
                  {r.cost_usd === null || r.cost_usd === undefined
                    ? t("usage.insights.priceUnknown")
                    : formatUsageCost(r.cost_usd, i18n.language, unknown)}
                </span>
                <span className={styles.meta}>
                  {t("usage.insights.tokensShort", {
                    amount: formatTokenCount(totalTokens(r), isAllUnknownUsage(r), i18n.language, unknown)
                  })}
                </span>
              </div>
              {r.chat_id ? (
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => onOpenChat(r.chat_id as string)}
                  aria-label={t("usage.insights.openRunLabel", { bot, when: at })}
                >
                  <ExternalLink size={14} aria-hidden="true" />
                  {t("usage.insights.openRun")}
                </Button>
              ) : (
                <span className={styles.meta}>{t("usage.insights.noChat")}</span>
              )}
            </li>
          );
        })}
      </ol>
    </Card>
  );
}
