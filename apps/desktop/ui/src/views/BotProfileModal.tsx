import { PenLine, Plus } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button } from "ui";
import { type Bot, type McpServer, PERMISSION_CATEGORIES } from "../api/types";
import { ModelLineText, PermissionChip } from "../components/BotPanel";
import { BrandMark } from "../components/BrandMark";
import { DialogFooter, DialogHeader, dialogStyles } from "../components/DialogParts";
import { Modal } from "../components/Modal";
import { SettingGroup, SettingRow } from "../components/SettingsUi";
import { fallbackList } from "../lib/botEditor";
import { botChats } from "../lib/botLanding";
import { botView, draftFromBot, formatModelRef } from "../lib/botProfile";
import { skillPreview, summarizeBotSkills } from "../lib/botSkills";
import { brandIcon } from "../lib/brandIcons";
import { botHasAccess } from "../lib/mcpApps";
import { useDaemon } from "../state/DaemonContext";
import type { BotSkillsState } from "../state/useBotSkills";
import styles from "./BotProfileModal.module.css";

const SKILL_CHIPS = 12;

/**
 * "Bot profile" as a dialog (owner feedback 244, point 5): a read-only card of who the bot is
 * and what it may do, in the same visual language as "Edit bot".  Editing is one button away.
 */
export function BotProfileModal({
  open,
  bot,
  skills,
  newChatBusy,
  onClose,
  onEdit,
  onNewChat
}: {
  open: boolean;
  bot: Bot | undefined;
  skills: BotSkillsState;
  newChatBusy: boolean;
  onClose: () => void;
  onEdit: () => void;
  onNewChat: () => void;
}) {
  const { t } = useTranslation();
  const shown = open && bot !== undefined;
  return (
    <Modal open={shown} onClose={onClose} label={t("botPanel.profile")} size="md">
      {shown && bot ? (
        <ProfileDialog
          bot={bot}
          skills={skills}
          newChatBusy={newChatBusy}
          onClose={onClose}
          onEdit={onEdit}
          onNewChat={onNewChat}
        />
      ) : null}
    </Modal>
  );
}

function ProfileDialog({
  bot,
  skills,
  newChatBusy,
  onClose,
  onEdit,
  onNewChat
}: {
  bot: Bot;
  skills: BotSkillsState;
  newChatBusy: boolean;
  onClose: () => void;
  onEdit: () => void;
  onNewChat: () => void;
}) {
  const { t, i18n } = useTranslation();
  const { client, chats } = useDaemon();
  const view = botView(bot);
  const draft = draftFromBot(bot);
  const [apps, setApps] = useState<McpServer[] | null>(null);

  useEffect(() => {
    if (!client) return undefined;
    let cancelled = false;
    client
      .listMcpServers()
      .then((list) => {
        if (!cancelled) setApps(list.filter((s) => botHasAccess(s, bot.id)));
      })
      .catch(() => {
        if (!cancelled) setApps([]);
      });
    return () => {
      cancelled = true;
    };
  }, [client, bot.id]);

  const fallbacks = fallbackList(draft.fallback);
  const summary = skills.data ? summarizeBotSkills(skills.data) : null;
  const preview = skills.data ? skillPreview(skills.data, SKILL_CHIPS) : null;
  const chatCount = botChats(chats, bot.id).length;
  const created = new Date(bot.created_at_ms).toLocaleDateString(i18n.language, {
    year: "numeric",
    month: "long",
    day: "numeric"
  });

  return (
    <div className={dialogStyles.root} data-testid="bot-profile-dialog">
      <DialogHeader title={t("botPanel.profile")} onClose={onClose} />
      <div className={dialogStyles.body} tabIndex={0}>
        <div className={styles.hero}>
          <BotAvatar bot={view} size="hero" decorative />
          <div className={styles.heroText}>
            <h3 className={styles.name}>{view.name}</h3>
            <div className={styles.meta}>
              <ModelLineText model={view.executor} />
              <PermissionChip preset={draft.preset} />
            </div>
            <p className={styles.facts}>
              {t("botProfile.facts", { date: created, chats: chatCount })}
            </p>
          </div>
        </div>

        <section className={dialogStyles.block}>
          <h3 className={dialogStyles.blockTitle}>{t("botForm.persona")}</h3>
          {draft.persona.trim() ? (
            <p className={styles.persona}>{draft.persona}</p>
          ) : (
            <p className={dialogStyles.hint}>{t("botProfile.noPersona")}</p>
          )}
        </section>

        <section className={dialogStyles.block}>
          <h3 className={dialogStyles.blockTitle}>{t("botForm.models")}</h3>
          <SettingGroup>
            <SettingRow title={t("botEdit.mainModel")}>
              <ModelLineText model={view.executor} />
            </SettingRow>
            <SettingRow title={t("botForm.role.planner")}>
              {formatModelRef(draft.planner) ? (
                <ModelLineText model={formatModelRef(draft.planner)} />
              ) : (
                <span className={styles.muted}>{t("botEdit.sameAsMain")}</span>
              )}
            </SettingRow>
            <SettingRow title={t("botForm.role.classifier")}>
              {formatModelRef(draft.classifier) ? (
                <ModelLineText model={formatModelRef(draft.classifier)} />
              ) : (
                <span className={styles.muted}>{t("botEdit.sameAsMain")}</span>
              )}
            </SettingRow>
            {fallbacks.length > 0 ? (
              <SettingRow title={t("botForm.role.fallback")}>
                <span className={styles.stack}>
                  {fallbacks.map((ref) => (
                    <ModelLineText key={ref} model={ref} />
                  ))}
                </span>
              </SettingRow>
            ) : null}
          </SettingGroup>
        </section>

        <section className={dialogStyles.block}>
          <h3 className={dialogStyles.blockTitle}>{t("botForm.permissions")}</h3>
          <SettingGroup>
            <SettingRow
              title={t(`botForm.preset.${draft.preset}.name`)}
              description={t(`botForm.preset.${draft.preset}.hint`)}
            />
            {draft.preset === "custom"
              ? PERMISSION_CATEGORIES.map((c) => (
                  <SettingRow key={c} title={t(`botForm.category.${c}`)}>
                    <span className={styles.level} data-level={draft.categories[c]}>
                      {t(`botForm.level.${draft.categories[c]}`)}
                    </span>
                  </SettingRow>
                ))
              : null}
          </SettingGroup>
        </section>

        <section className={dialogStyles.block}>
          <h3 className={dialogStyles.blockTitle}>{t("bots.skills.heading")}</h3>
          {summary && preview ? (
            <>
              <p className={dialogStyles.hint}>
                {summary.mode === "all"
                  ? t("bots.skills.summaryAll", { count: summary.availableCount })
                  : t("bots.skills.summarySelected", {
                      count: summary.assignedCount,
                      total: summary.availableCount
                    })}
                {summary.ownCount > 0
                  ? ` ${t("bots.skills.summaryLearned", { count: summary.ownCount })}`
                  : ""}
              </p>
              {preview.names.length > 0 ? (
                <div className={styles.chips}>
                  {preview.names.map((name) => (
                    <span key={name} className={dialogStyles.chip}>
                      <span>{name}</span>
                    </span>
                  ))}
                  {preview.more > 0 ? (
                    <span className={dialogStyles.chip}>
                      <span>{t("bots.skills.more", { count: preview.more })}</span>
                    </span>
                  ) : null}
                </div>
              ) : null}
            </>
          ) : (
            <p className={dialogStyles.hint}>
              {skills.error ?? t("bots.skills.loading")}
            </p>
          )}
        </section>

        <section className={dialogStyles.block}>
          <h3 className={dialogStyles.blockTitle}>{t("botForm.apps.title")}</h3>
          {apps === null ? (
            <p className={dialogStyles.hint}>{t("botEdit.appsLoading")}</p>
          ) : apps.length === 0 ? (
            <p className={dialogStyles.hint}>{t("botProfile.noApps")}</p>
          ) : (
            <div className={styles.chips}>
              {apps.map((s) => {
                const brand = brandIcon(s.name);
                return (
                  <span key={s.name} className={dialogStyles.chip}>
                    <BrandMark name={s.name} size={14} />
                    <span>{brand.known ? brand.name : s.name}</span>
                  </span>
                );
              })}
            </div>
          )}
        </section>
      </div>
      <DialogFooter>
        <Button variant="ghost" disabled={newChatBusy} onClick={onNewChat}>
          <Plus aria-hidden="true" />
          {t("bots.newChat")}
        </Button>
        <Button variant="primary" data-autofocus data-testid="bot-profile-edit" onClick={onEdit}>
          <PenLine aria-hidden="true" />
          {t("botPanel.edit")}
        </Button>
      </DialogFooter>
    </div>
  );
}
