import { type FormEvent, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown, ChevronUp, X } from "lucide-react";
import { Button } from "ui";
import type { Bot } from "../api/types";
import { BotAvatarUploadField, useBotFormDeps } from "../components/BotAdvancedFields";
import {
  AdvancedFields,
  AppFields,
  ModelFields,
  PermissionFields
} from "../components/BotEditorFields";
import { BotIdentityPicker } from "../components/BotIdentityPicker";
import formStyles from "../components/Form.module.css";
import { Modal } from "../components/Modal";
import { ModelPicker } from "../components/ModelPicker";
import { Segmented, SettingGroup, SettingRow } from "../components/SettingsUi";
import {
  AUTONOMY_LEVELS,
  type AutonomyLevel,
  autonomyToPreset,
  BOT_TEMPLATES,
  type BotDraft,
  type BotTemplate,
  type BotTemplateId,
  configFromDraft,
  DEFAULT_NEW_BOT_COLOR,
  draftFromBot,
  formatModelRef,
  presetToAutonomy,
  resolveAutoExecutorRef
} from "../lib/botProfile";
import { invalidCustomRoles } from "../lib/botEditor";
import { botHasAccess, changedApps, saveBotAndApps } from "../lib/mcpApps";
import { botWriteMessage } from "../lib/botWrite";
import { modelGroups } from "../lib/modelPicker";
import { LOCAL_COMPUTER, useDaemon } from "../state/DaemonContext";
import { useEngineConsent } from "../state/EngineConsent";
import { useCustomText } from "../state/useCustomText";
import styles from "./NewBotModal.module.css";
import { HomeHint } from "../components/HomeHint";

function NewBotDialogContent({
  onDone,
  onClose
}: {
  onDone: (botId: string) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const { client, bots, conn, refreshBots, activeComputerId, activeHostName } = useDaemon();
  const { askEngine, applyEngine } = useEngineConsent();
  const host = activeComputerId === LOCAL_COMPUTER ? null : { name: activeHostName ?? t("computerPicker.host") };
  const {
    providersDto,
    servers,
    setServers,
    allowed,
    setAllowed,
    initialApps,
    setInitialApps
  } = useBotFormDeps(client, undefined, true);

  const [createdBot, setCreatedBot] = useState<Bot | null>(null);
  const [templateId, setTemplateId] = useState<BotTemplateId | null>("blank");
  const [draft, setDraft] = useState<BotDraft>(() => ({
    ...draftFromBot(undefined),
    shape: "pebble",
    color: DEFAULT_NEW_BOT_COLOR
  }));
  const [brainMode, setBrainMode] = useState<"auto" | "manual">("auto");
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const { customText, setCustom, forgetFor } = useCustomText(setDraft);
  const patch = (p: Partial<BotDraft>) => {
    setDraft((d) => ({ ...d, ...p }));
    forgetFor(p);
  };

  const autoExecutor = useMemo(
    () => resolveAutoExecutorRef(providersDto, Boolean(conn?.fake_provider)),
    [providersDto, conn?.fake_provider]
  );

  const effectiveExecutor = brainMode === "auto" ? autoExecutor : draft.executor;
  const effectiveDraft = useMemo(
    () => (brainMode === "auto" ? { ...draft, executor: autoExecutor } : draft),
    [brainMode, draft, autoExecutor]
  );

  const groups = useMemo(
    () =>
      modelGroups(
        providersDto,
        brainMode === "manual" ? formatModelRef(draft.executor) : undefined
      ),
    [providersDto, brainMode, draft.executor]
  );

  const applyTemplate = (tpl: BotTemplate) => {
    setTemplateId(tpl.id);
    patch({
      name: tpl.defaultName,
      shape: tpl.shape,
      color: tpl.color
    });
  };

  const activeAutonomy = presetToAutonomy(draft.preset);
  const nameMissing = draft.name.trim() === "";
  const executorMissing = effectiveExecutor.model.trim() === "";
  // An unusable hand-typed id blocks creating the bot, as it blocks saving an edit.
  const [pendingFallback, setPendingFallback] = useState("");
  const customInvalid =
    invalidCustomRoles(customText).length > 0 || pendingFallback.trim() !== "";

  const save = async () => {
    if (!client || nameMissing || executorMissing || customInvalid || saving) {
      return;
    }
    setSaving(true);
    setError(null);
    try {
      // A CLI model that only works with its own tools: the one consent step, before creating.
      const answer = await askEngine(formatModelRef(effectiveDraft.executor), createdBot?.config);
      if (answer === null) {
        return;
      }
      const target = createdBot ?? undefined;
      const config = configFromDraft(effectiveDraft, target);
      const input = { name: effectiveDraft.name.trim(), config };
      const { bot, failed, notApplied } = await saveBotAndApps(
        client,
        target,
        input,
        initialApps,
        allowed,
        bots.map((b) => b.id)
      );
      if (!target) {
        setCreatedBot(bot);
      }
      await applyEngine(bot.id, answer);
      await refreshBots().catch(() => {});
      if (failed.length > 0 || notApplied.length > 0) {
        const fresh = await client.listMcpServers().catch(() => null);
        const problem = new Set([...failed, ...notApplied]);
        const actual = (name: string): boolean | undefined => {
          const srv = fresh?.find((x) => x.name === name);
          return srv ? botHasAccess(srv, bot.id) : undefined;
        };
        if (fresh) {
          setServers(fresh);
        }
        setAllowed((prev) => {
          const next = { ...prev };
          for (const name of notApplied) {
            next[name] = actual(name) ?? prev[name];
          }
          return next;
        });
        setInitialApps((prev) => {
          const next = { ...prev };
          for (const name of changedApps(prev, allowed)) {
            if (notApplied.includes(name)) {
              next[name] = actual(name) ?? prev[name];
            } else if (!problem.has(name)) {
              next[name] = allowed[name];
            }
          }
          return next;
        });
        setError(
          [
            failed.length > 0
              ? t("botForm.apps.partialFail", { names: failed.join(", ") })
              : "",
            notApplied.length > 0
              ? t("botForm.apps.notApplied", { names: notApplied.join(", ") })
              : ""
          ]
            .filter(Boolean)
            .join(" ")
        );
        return;
      }
      onDone(bot.id);
    } catch (err) {
      setError(botWriteMessage(err, t));
    } finally {
      setSaving(false);
    }
  };

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    void save();
  };

  return (
    <form className={styles.modalForm} onSubmit={handleSubmit}>
      <header className={styles.header}>
        <h2 className={styles.title}>{t("newBot.title")}</h2>
        <HomeHint />
        <button
          type="button"
          className={styles.closeButton}
          aria-label={t("common.close")}
          onClick={onClose}
        >
          <X aria-hidden="true" />
        </button>
      </header>

      <div className={styles.body}>
        <div className={styles.templates} role="group" aria-label={t("newBot.templatesLabel")}>
          {BOT_TEMPLATES.map((tpl) => {
            const active = templateId === tpl.id;
            return (
              <button
                key={tpl.id}
                type="button"
                className={[styles.tplChip, active ? styles.tplChipActive : ""]
                  .filter(Boolean)
                  .join(" ")}
                aria-pressed={active}
                data-testid={`template-${tpl.id}`}
                onClick={() => applyTemplate(tpl)}
              >
                {t(`newBot.templates.${tpl.id}`)}
              </button>
            );
          })}
        </div>

        <BotIdentityPicker
          previewId={createdBot?.id ?? "new-bot-preview"}
          name={draft.name}
          shape={draft.shape}
          color={draft.color}
          avatar={draft.avatar}
          onName={(name) => {
            setTemplateId(null);
            patch({ name });
          }}
          onShape={(shape) => {
            setTemplateId(null);
            patch({ shape });
          }}
          onColor={(color) => {
            setTemplateId(null);
            patch({ color });
          }}
        />

        <div className={styles.settingsGroup}>
          <SettingGroup>
            <SettingRow
              title={t("newBot.brain")}
              description={t("newBot.brainHint")}
            >
              <ModelPicker
                groups={groups}
                value={brainMode === "auto" ? "" : formatModelRef(draft.executor)}
                autoOption={{
                  label: t("newBot.brainAuto"),
                  sub: formatModelRef(autoExecutor) || undefined,
                  selected: brainMode === "auto",
                  onPick: () => setBrainMode("auto")
                }}
                reviewOpen={false}
                placement="bottom-end"
                engines
                testId="new-bot-brain"
                onPick={(ref) => {
                  const i = ref.indexOf("/");
                  setBrainMode("manual");
                  patch({
                    executor:
                      i < 0
                        ? { provider: "", model: ref }
                        : { provider: ref.slice(0, i), model: ref.slice(i + 1) }
                  });
                }}
              />
            </SettingRow>
            <SettingRow
              stacked
              title={t("newBot.autonomy")}
              description={
                activeAutonomy
                  ? t(`newBot.autonomyHints.${activeAutonomy}`)
                  : t("newBot.autonomyCustom", {
                      preset: t(`botForm.preset.${draft.preset}.name`)
                    })
              }
            >
              <Segmented
                label={t("newBot.autonomy")}
                value={activeAutonomy ?? ("" as AutonomyLevel)}
                options={AUTONOMY_LEVELS.map((level) => ({
                  value: level,
                  label: t(`newBot.autonomyLevels.${level}`),
                  testId: `autonomy-${level}`
                }))}
                onChange={(level) => patch({ preset: autonomyToPreset(level) })}
              />
            </SettingRow>
          </SettingGroup>
        </div>

        <button
          type="button"
          className={styles.advancedToggle}
          aria-expanded={advancedOpen}
          data-testid="new-bot-advanced-toggle"
          onClick={() => setAdvancedOpen((v) => !v)}
        >
          <span>{t("newBot.advanced")}</span>
          {advancedOpen ? <ChevronUp aria-hidden="true" /> : <ChevronDown aria-hidden="true" />}
        </button>

        {advancedOpen ? (
          <div className={styles.advancedPanel} data-testid="new-bot-advanced-panel">
            <div className={formStyles.field}>
              <label className={formStyles.label} htmlFor="new-bot-persona">
                {t("botForm.persona")}
              </label>
              <textarea
                id="new-bot-persona"
                className={formStyles.textarea}
                value={draft.persona}
                placeholder={t("botForm.personaPlaceholder")}
                onChange={(e) => patch({ persona: e.target.value })}
              />
            </div>

            <BotAvatarUploadField
              avatar={draft.avatar}
              onChange={(avatar) => patch({ avatar })}
            />

            <div>
              <h3 className={styles.advancedHeading}>{t("botForm.models")}</h3>
              <ModelFields
                draft={effectiveDraft}
                providersDto={providersDto}
                executorMissing={executorMissing}
                pendingFallback={pendingFallback}
                onPendingFallback={setPendingFallback}
                patch={(next) => {
                  if (next.executor) {
                    setBrainMode("manual");
                  }
                  patch(next);
                }}
              />
            </div>

            <div>
              <h3 className={styles.advancedHeading}>{t("botForm.permissions")}</h3>
              <PermissionFields draft={draft} patch={patch} />
            </div>

            <div>
              <h3 className={styles.advancedHeading}>{t("botForm.apps.title")}</h3>
              <AppFields
                servers={servers}
                allowed={allowed}
                onToggle={(name, checked) =>
                  setAllowed((prev) => ({ ...prev, [name]: checked }))
                }
              />
            </div>

            <AdvancedFields
              draft={effectiveDraft}
              customText={customText}
              onCustomText={(role, text) => {
                if (role === "executor") setBrainMode("manual");
                setCustom(role, text);
              }}
              computerTarget="local"
              browserTarget="local"
              deleteBlocked={null}
              chatCount={0}
              runCount={0}
            />
          </div>
        ) : null}

        {host && executorMissing ? (
          <p className={styles.alert} role="status" data-testid="new-bot-host-needs-model">
            {t("hostMode.newBotNeedsModel", { name: host.name })}
          </p>
        ) : null}
        {error ? (
          <p className={styles.alert} role="alert">
            {error}
          </p>
        ) : null}
      </div>

      <footer className={styles.footer}>
        <Button variant="ghost" onClick={onClose}>
          {t("common.cancel")}
        </Button>
        <Button
          type="submit"
          variant="primary"
          disabled={saving || nameMissing || executorMissing || customInvalid}
          data-testid="new-bot-submit"
        >
          {t("newBot.create")}
        </Button>
      </footer>
    </form>
  );
}

/** Compact v6 bot-creation modal with templates, 16 3D shapes, colours, Auto Brain & Autonomy. */
export function NewBotModal({
  open,
  onDone,
  onClose
}: {
  open: boolean;
  onDone: (botId: string) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  return (
    <Modal open={open} onClose={onClose} size="sm" label={t("newBot.title")}>
      {open ? <NewBotDialogContent onDone={onDone} onClose={onClose} /> : null}
    </Modal>
  );
}
