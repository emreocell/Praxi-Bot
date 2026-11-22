import { useTranslation } from "react-i18next";
import type { BotShapeId } from "ui";
import { FIRST_PROMPT_KEYS } from "../../lib/firstRun";
import { BotIdentityPicker } from "../../components/BotIdentityPicker";
import styles from "./FirstRun.module.css";

export interface BotLook {
  name: string;
  shape: BotShapeId;
  color: string;
}

/** The last decision: who the bot is. The same picker as the New-bot modal. */
export function BotStep({
  look,
  recap,
  error,
  onLook
}: {
  look: BotLook;
  recap: string;
  error: string | null;
  onLook: (patch: Partial<BotLook>) => void;
}) {
  const { t } = useTranslation();
  return (
    <div className={styles.stack}>
      <BotIdentityPicker
        previewId="first-run-bot"
        name={look.name}
        shape={look.shape}
        color={look.color}
        onName={(name) => onLook({ name })}
        onShape={(shape) => onLook({ shape })}
        onColor={(color) => onLook({ color })}
      />
      <p className={styles.recap}>{recap}</p>
      <section className={styles.note} aria-labelledby="fr-where" data-testid="fr-where">
        <h2 id="fr-where" className={styles.eyebrow}>{t("firstRun.bot.whereTitle")}</h2>
        <p className={styles.noteText}>{t("firstRun.bot.whereBody")}</p>
        <p className={styles.noteText}>{t("firstRun.bot.whereLater")}</p>
      </section>
      <section className={styles.note} aria-labelledby="fr-try" data-testid="fr-try-first">
        <h2 id="fr-try" className={styles.eyebrow}>{t("firstRun.bot.tryTitle")}</h2>
        <ul className={styles.tryList}>
          {FIRST_PROMPT_KEYS.map((key) => (
            <li key={key}>{t(`firstRun.prompts.${key}`)}</li>
          ))}
        </ul>
      </section>
      {error ? (
        <p className={styles.statusBad} role="alert" data-testid="fr-error">
          {error}
        </p>
      ) : null}
    </div>
  );
}
