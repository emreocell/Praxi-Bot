import { ArrowUpRight, CheckCheck } from "lucide-react";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button, Card } from "ui";
import formStyles from "../components/Form.module.css";
import { botView } from "../lib/botProfile";
import { useDaemon } from "../state/DaemonContext";
import styles from "./NeedsYouView.module.css";

/** Everything waiting for the user (approvals and questions) across all chats. */
export function NeedsYouView({
  onOpenChat
}: {
  onOpenChat: (chatId: string) => void;
}) {
  const { t, i18n } = useTranslation();
  const { needs, bots } = useDaemon();
  return (
    <div className={styles.pane}>
      <div className={styles.container}>
        <header>
          <h1 className={styles.title}>{t("needs.title")}</h1>
          <p className={formStyles.hint}>{t("needs.subtitle")}</p>
        </header>
        {needs.length === 0 ? (
          <p className={formStyles.hint}><CheckCheck className={formStyles.emptyIcon} />{t("needs.empty")}</p>
        ) : (
          <ul className={styles.list}>
            {needs.map((n) => {
              const bot = bots.find((b) => b.id === n.botId);
              const view = bot ? botView(bot) : null;
              return (
                <li key={n.callId}>
                  <Card padding="md" className={styles.row}>
                    <BotAvatar bot={view ?? { id: "unknown", name: "Bot" }} size="md" />
                    <div className={styles.text}>
                      <span className={styles.kind}>
                        {t(`needs.kind.${n.kind}`)}
                        {view ? ` · ${view.name}` : ""}
                      </span>
                      <span className={styles.summary}>{n.title}</span>
                      <span className={formStyles.hint}>
                        {new Date(n.sinceMs).toLocaleTimeString(i18n.language)}
                      </span>
                    </div>
                    <Button
                      size="sm"
                      variant="primary"
                      disabled={!n.chatId}
                      onClick={() => onOpenChat(n.chatId)}
                    >
                      <ArrowUpRight />{t("needs.open")}
                    </Button>
                  </Card>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}
