import { ChevronRight, Plus, SlidersHorizontal, TriangleAlert } from "lucide-react";
import { useTranslation } from "react-i18next";
import { skillPreview, summarizeBotSkills } from "../lib/botSkills";
import type { BotSkillsState } from "../state/useBotSkills";
import styles from "./Skills.module.css";

const PREVIEW = 4;

/** Skills in the right panel: what the bot has at a glance, manage and add one click away. */
export function BotPanelSkills({
  skills,
  onManage,
  onAdd,
  bare = false
}: {
  skills: BotSkillsState;
  onManage: () => void;
  onAdd: () => void;
  /** Inside a Library shelf, which already shows the title and the add button. */
  bare?: boolean;
}) {
  const { t } = useTranslation();
  const { data, loading, error } = skills;
  const summary = data ? summarizeBotSkills(data) : null;
  const preview = data ? skillPreview(data, PREVIEW) : null;

  return (
    <section aria-labelledby={bare ? undefined : "bot-panel-skills"} data-testid="bot-panel-skills">
      {bare ? null : <div className={styles.panelHead}>
        <h3 className={styles.panelLabel} id="bot-panel-skills">
          {t("bots.skills.heading")}
        </h3>
        <div className={styles.panelTools}>
          <button
            type="button"
            className={styles.panelTool}
            title={t("bots.skills.manage")}
            aria-label={t("bots.skills.manage")}
            disabled={!data}
            onClick={onManage}
          >
            <SlidersHorizontal aria-hidden="true" />
          </button>
          <button
            type="button"
            className={styles.panelTool}
            title={t("addSkill.open")}
            aria-label={t("addSkill.open")}
            data-testid="bot-panel-add-skill"
            onClick={onAdd}
          >
            <Plus aria-hidden="true" />
          </button>
        </div>
      </div>}
      {error && !data ? (
        <div className={styles.panelError} role="alert">
          {error}
          <button type="button" className={styles.textButton} onClick={() => void skills.reload()}>
            {t("health.retry")}
          </button>
        </div>
      ) : !summary || !preview ? (
        <p className={styles.panelMuted}>{loading ? t("bots.skills.loading") : ""}</p>
      ) : (
        <button
          type="button"
          className={styles.panelCard}
          data-testid="choose-skills-button"
          onClick={onManage}
        >
          <span className={styles.panelSummary}>
            <span data-testid="bot-skills-summary">
              {summary.mode === "all"
                ? t("bots.skills.summaryAll", { count: summary.availableCount })
                : t("bots.skills.summarySelected", {
                    count: summary.assignedCount,
                    total: summary.availableCount
                  })}
              {summary.ownCount > 0
                ? ` ${t("bots.skills.summaryLearned", { count: summary.ownCount })}`
                : ""}
            </span>
            <ChevronRight aria-hidden="true" />
          </span>
          {preview.names.length > 0 ? (
            <span className={styles.panelChips}>
              {preview.names.map((name) => (
                <span key={name} className={styles.panelChip}>
                  {name}
                </span>
              ))}
              {preview.more > 0 ? (
                <span className={styles.panelChip}>{t("bots.skills.more", { count: preview.more })}</span>
              ) : null}
            </span>
          ) : null}
          {summary.missingCount > 0 ? (
            <span className={styles.panelWarn}>
              <TriangleAlert aria-hidden="true" />
              {t("bots.skills.summaryMissing", { count: summary.missingCount })}
            </span>
          ) : null}
        </button>
      )}
    </section>
  );
}
