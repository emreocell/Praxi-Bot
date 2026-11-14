import { MessageSquare, SlidersHorizontal, UserRound, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { BotAvatar, type BotAvatarState, type BotIdentity } from "ui";
import { BotComputerBlock } from "./BotComputerBlock";
import { QuotaBlock } from "./QuotaRings";
import styles from "./BotPanel.module.css";

export interface BotPanelChat {
  id: string;
  title: string;
  createdAtMs: number;
}

/**
 * Right panel of the app shell (card 198-B), opened by the bot chip above the chat: the bot's
 * face and name, its recent chats and the way to its profile / settings.  Further sections
 * (quota rings, upcoming tasks) slot in below the chats.
 */
export function BotPanel({
  bot,
  model,
  state,
  chats,
  activeChatId,
  onClose,
  onOpenChat,
  onOpenProfile,
  onOpenSettings
}: {
  bot: BotIdentity;
  /** `provider/model` the bot runs on. */
  model?: string;
  state?: BotAvatarState;
  chats: readonly BotPanelChat[];
  activeChatId?: string;
  onClose: () => void;
  onOpenChat: (chatId: string) => void;
  onOpenProfile: () => void;
  onOpenSettings: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div className={styles.root} id="bot-panel" data-testid="bot-panel">
      <button type="button" className={styles.close} title={t("botPanel.close")} aria-label={t("botPanel.close")} onClick={onClose}>
        <X />
      </button>
      <div className={styles.hero}>
        <BotAvatar bot={bot} size="hero" state={state ?? "idle"} decorative />
        <h2 className={styles.name}>{bot.name}</h2>
        {model ? <p className={styles.model}>{model.split("/").pop()}</p> : null}
        <div className={styles.actions}>
          <button type="button" className={styles.pill} onClick={onOpenProfile}>
            <UserRound />
            {t("botPanel.profile")}
          </button>
          <button type="button" className={styles.pill} onClick={onOpenSettings}>
            <SlidersHorizontal />
            {t("botPanel.settings")}
          </button>
        </div>
      </div>
      <section className={styles.section} aria-labelledby="bot-panel-chats">
        <h3 className={styles.label} id="bot-panel-chats">
          {t("botPanel.chats")}
        </h3>
        {chats.length === 0 ? (
          <p className={styles.empty}>{t("botPanel.noChats")}</p>
        ) : (
          <ul className={styles.list}>
            {chats.map((c) => (
              <li key={c.id}>
                <button
                  type="button"
                  className={styles.chat}
                  aria-current={c.id === activeChatId ? "true" : undefined}
                  onClick={() => onOpenChat(c.id)}
                >
                  <MessageSquare />
                  <span>{c.title || t("botPanel.untitled")}</span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>
      <BotComputerBlock />
      <QuotaBlock />
    </div>
  );
}
