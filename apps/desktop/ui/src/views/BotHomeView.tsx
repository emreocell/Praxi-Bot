import { MessageSquare, PenLine, Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Avatar, Button, Card } from "ui";
import formStyles from "../components/Form.module.css";
import { botView, draftFromBot } from "../lib/botProfile";
import { useDaemon } from "../state/DaemonContext";
import styles from "./BotHomeView.module.css";

export function BotHomeView({
  botId,
  onOpenChat,
  onEdit,
  onDeleted
}: {
  botId: string;
  onOpenChat: (chatId: string) => void;
  onEdit: () => void;
  onDeleted: () => void;
}) {
  const { t, i18n } = useTranslation();
  const { client, bots, chats, runs, needs, refreshBots, refreshChats } =
    useDaemon();
  const bot = bots.find((b) => b.id === botId);
  const [busy, setBusy] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!bot) {
    return (
      <div className={styles.pane}>
        <p className={formStyles.hint}>{t("bots.notFound")}</p>
      </div>
    );
  }
  const view = botView(bot);
  const draft = draftFromBot(bot);
  const botChats = chats
    .filter((c) => c.bot_id === botId)
    .sort((a, b) => b.created_at_ms - a.created_at_ms);

  const newChat = async () => {
    if (!client) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const chat = await client.createChat(botId);
      await refreshChats();
      onOpenChat(chat.id);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!client) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await client.deleteBot(botId);
      await refreshBots();
      onDeleted();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

  const stateOf = (chatId: string): "needs" | "running" | null => {
    if (needs.some((n) => n.chatId === chatId)) {
      return "needs";
    }
    if (
      runs.some(
        (r) =>
          r.chat_id === chatId &&
          (r.state === "running" || r.state === "waiting_approval")
      )
    ) {
      return "running";
    }
    return null;
  };

  return (
    <div className={styles.pane}>
      <div className={styles.container}>
        <Card padding="lg" accentColor={view.color} className={styles.hero}>
          <Avatar
            name={view.name}
            src={view.avatar}
            color={view.color}
            size="lg"
          />
          <div className={styles.heroText}>
            <h1 className={styles.title}>{view.name}</h1>
            <p className={styles.meta}>
              {view.executor || t("bots.noModel")} ·{" "}
              {t(`botForm.preset.${draft.preset}.name`)}
            </p>
            {draft.persona ? (
              <p className={styles.persona}>{draft.persona}</p>
            ) : null}
          </div>
          <div className={styles.heroActions}>
            <Button
              variant="primary"
              disabled={busy}
              onClick={() => void newChat()}
            >
              <Plus />{t("bots.newChat")}
            </Button>
            <Button variant="secondary" onClick={onEdit}>
              <PenLine />{t("common.edit")}
            </Button>
            {confirmDelete ? (
              <Button
                variant="danger"
                disabled={busy}
                onClick={() => void remove()}
              >
                {t("bots.confirmDelete")}
              </Button>
            ) : (
              <Button variant="ghost" onClick={() => setConfirmDelete(true)}>
                <Trash2 />{t("common.delete")}
              </Button>
            )}
          </div>
        </Card>
        {error ? (
          <p className={formStyles.error} role="alert">
            {error}
          </p>
        ) : null}

        <h2 className={styles.heading}>{t("bots.chats")}</h2>
        {botChats.length === 0 ? (
          <p className={formStyles.hint}><MessageSquare className={formStyles.emptyIcon} />{t("bots.noChats")}</p>
        ) : (
          <ul className={styles.chatList}>
            {botChats.map((c) => {
              const st = stateOf(c.id);
              return (
                <li key={c.id}>
                  <button
                    type="button"
                    className={styles.chatRow}
                    onClick={() => onOpenChat(c.id)}
                  >
                    <MessageSquare /><span className={styles.chatTitle}>{c.title}</span>
                    {st ? (
                      <span
                        className={[
                          styles.chatState,
                          st === "needs" ? styles.chatStateNeeds : ""
                        ]
                          .filter(Boolean)
                          .join(" ")}
                      >
                        {t(`bots.chatState.${st}`)}
                      </span>
                    ) : null}
                    <span className={styles.chatDate}>
                      {new Date(c.created_at_ms).toLocaleString(i18n.language)}
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}
