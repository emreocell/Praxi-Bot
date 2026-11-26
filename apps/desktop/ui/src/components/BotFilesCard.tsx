import { FileCog } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { Bot } from "../api/types";
import { filesAndCommandsMissing, filesAndCommandsPatch } from "../lib/botFiles";
import { humanLine } from "../lib/humanError";
import { useDaemon } from "../state/DaemonContext";
import styles from "./ComputerAccess.module.css";

/**
 * Shown while a bot's tool list leaves out writing files and running commands (older bots set up
 * from the command line): one sentence and one button, no engine or `cli:` words. The button only
 * adds those tools to the list; the bot's permission level still decides whether it asks first.
 */
export function BotFilesCard({ bot }: { bot: Bot }) {
  const { t } = useTranslation();
  const { client, refreshBots } = useDaemon();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!client || !filesAndCommandsMissing(bot.config)) return null;
  const enable = async () => {
    setBusy(true);
    setError(null);
    try {
      await client.patchBot(bot.id, filesAndCommandsPatch(bot));
      await refreshBots();
    } catch (e) {
      setError(t("botFiles.error", { message: humanLine(e) }));
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className={styles.enable} aria-label={t("botFiles.title")} data-testid="bot-files-card">
      <FileCog className={styles.enableIcon} aria-hidden="true" />
      <div className={styles.stack}>
        <strong>{t("botFiles.title")}</strong>
        <p className={styles.hint}>{t("botFiles.hint")}</p>
        {error ? <p className={styles.error} role="alert">{error}</p> : null}
      </div>
      <Button variant="primary" size="sm" disabled={busy} onClick={() => void enable()}>
        {t("botFiles.action")}
      </Button>
    </section>
  );
}
