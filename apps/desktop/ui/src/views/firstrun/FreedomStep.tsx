import { Hand, Scale, ShieldCheck, Zap } from "lucide-react";
import { type KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import { accessLevelKey } from "../../lib/accessLevels";
import { AUTONOMY_LEVELS, type AutonomyLevel, autonomyToPreset } from "../../lib/botProfile";
import styles from "./FirstRun.module.css";

const ICONS = { ask: Hand, balanced: Scale, auto: Zap } as const;

/** One decision: how much the bot may do without asking. Three cards, "Balanced" preselected. */
export function FreedomStep({
  level,
  hotkey,
  onLevel
}: {
  level: AutonomyLevel;
  hotkey: string;
  onLevel: (level: AutonomyLevel) => void;
}) {
  const { t } = useTranslation();

  // Radio-group keys: arrows move the choice, as in any native radio group.
  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    const step = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
    if (!step) {
      return;
    }
    e.preventDefault();
    const at = AUTONOMY_LEVELS.indexOf(level);
    const next = AUTONOMY_LEVELS[(at + step + AUTONOMY_LEVELS.length) % AUTONOMY_LEVELS.length];
    onLevel(next);
    e.currentTarget.querySelector<HTMLElement>(`[data-level="${next}"]`)?.focus();
  };

  return (
    <div className={styles.stack}>
      <div
        className={styles.levels}
        role="radiogroup"
        aria-label={t("firstRun.freedom.title")}
        onKeyDown={onKey}
      >
        {AUTONOMY_LEVELS.map((id) => {
          const on = level === id;
          const Icon = ICONS[id];
          const examples = t(`firstRun.freedom.levels.${id}.examples`, { returnObjects: true }) as string[];
          return (
            <button
              key={id}
              type="button"
              role="radio"
              aria-checked={on}
              tabIndex={on ? 0 : -1}
              data-level={id}
              data-testid={`fr-level-${id}`}
              className={`${styles.card} ${styles.level} ${on ? styles.cardOn : ""}`}
              onClick={() => onLevel(id)}
            >
              <span className={styles.levelHead}>
                <Icon className={styles.levelIcon} aria-hidden="true" />
                <span className={styles.cardTitle}>{t(`${accessLevelKey(autonomyToPreset(id))}.name`)}</span>
                {id === "balanced" ? <span className={styles.badge}>{t("firstRun.freedom.recommended")}</span> : null}
              </span>
              <span className={styles.levelPitch}>{t(`firstRun.freedom.levels.${id}.pitch`)}</span>
              <ul className={styles.examples}>
                {examples.map((line) => (
                  <li key={line}>{line}</li>
                ))}
              </ul>
            </button>
          );
        })}
      </div>

      <section className={styles.always} aria-labelledby="fr-always">
        <h2 id="fr-always" className={styles.alwaysTitle}>
          <ShieldCheck aria-hidden="true" />
          {t("firstRun.freedom.alwaysTitle")}
        </h2>
        <ul className={styles.alwaysList}>
          <li>{t("firstRun.freedom.alwaysMoney")}</li>
          <li>{t("firstRun.freedom.alwaysStop", { hotkey })}</li>
        </ul>
      </section>
    </div>
  );
}
