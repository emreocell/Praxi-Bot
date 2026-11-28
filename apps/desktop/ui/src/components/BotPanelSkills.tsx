import { Sparkles } from "lucide-react";
import { useTranslation } from "react-i18next";
import { panelSkillRows } from "../lib/botSkills";
import type { BotSkillsState } from "../state/useBotSkills";
import { PanelRows, type PanelRow } from "./PanelRows";
import styles from "./Skills.module.css";

/** Skills in the Library tab: one line per skill the bot has, searchable once the list is long. */
export function BotPanelSkills({ skills, onManage }: { skills: BotSkillsState; onManage: () => void }) {
  const { t } = useTranslation();
  const { data, loading, error } = skills;
  const rows: PanelRow[] = data
    ? panelSkillRows(data).map((r) => ({
        id: `${r.kind}:${r.name}`,
        name: r.name,
        icon: Sparkles,
        onOpen: onManage,
        status:
          r.kind === "learned"
            ? { tone: "muted", label: t("panelTabs.library.learned") }
            : r.kind === "missing"
              ? { tone: "warn", label: t("bots.skills.missingTitle") }
              : undefined
      }))
    : [];

  return (
    <div data-testid="bot-panel-skills">
      {error && !data ? (
        <div className={styles.panelError} role="alert">
          {error}
          <button type="button" className={styles.textButton} onClick={() => void skills.reload()}>
            {t("health.retry")}
          </button>
        </div>
      ) : !data ? (
        <p className={styles.panelMuted}>{loading ? t("bots.skills.loading") : ""}</p>
      ) : rows.length === 0 ? (
        <p className={styles.panelMuted}>{t("bots.skills.emptyCatalog")}</p>
      ) : (
        <PanelRows
          testId="bot-skills-list"
          rows={rows}
          label={t("bots.skills.heading")}
          searchLabel={t("panelTabs.library.searchSkills")}
          footer={
            <button type="button" className={styles.panelManage} data-testid="choose-skills-button" onClick={onManage}>
              {t("bots.skills.manage")}
            </button>
          }
        />
      )}
    </div>
  );
}
