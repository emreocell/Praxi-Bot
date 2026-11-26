import { type FormEvent, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { ArrowLeft, ArrowRight, Check, ChevronDown, ChevronRight, FileUp, Plus, ShieldCheck } from "lucide-react";
import { BotAvatar, Button } from "ui";
import { ProfileImportView, type ProfileSource } from "../components/ProfileImportView";
import {
  type SharedProfile,
  templateDescription,
  templateName
} from "../lib/profileShare";
import type { Bot } from "../api/types";
import { BotAvatarUploadField, useBotFormDeps } from "../components/BotAdvancedFields";
import {
  AdvancedFields,
  AppFields,
  ModelFields,
  PermissionFields
} from "../components/BotEditorFields";
import { BotIdentityPicker } from "../components/BotIdentityPicker";
import { Callout, DialogBody, DialogFooter, DialogHeader } from "../components/DialogParts";
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
import { canGoTo, galleryLevel, NEW_BOT_STEPS, type NewBotStep, nextStep, prevStep, stepNumber } from "../lib/newBotSteps";

const NEW_BOT_SUBTITLE_ID = "new-bot-subtitle";

function NewBotDialogContent({
  onDone,
  onClose
}: {
  onDone: (botId: string) => void;
  onClose: () => void;
}) {
  const { t, i18n } = useTranslation();
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
  // Gallery templates and "Import from file" open the preview view instead of this form.
  const [importSource, setImportSource] = useState<ProfileSource | null>(null);
  const [gallery, setGallery] = useState<SharedProfile[]>([]);
  useEffect(() => {
    if (!client) return undefined;
    let cancelled = false;
    client
      .profileTemplates()
      .then((list) => {
        if (!cancelled) setGallery(list);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [client]);
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

  const [step, setStep] = useState<NewBotStep>("start");
  const goNext = () => {
    const next = nextStep(step);
    if (next && canGoTo(step, next, { nameMissing })) setStep(next);
  };
  const goBack = () => {
    const prev = prevStep(step);
    if (prev) setStep(prev);
  };

  const footerNote = step === "identity" && nameMissing && !saving ? t("newBot.nameNeeded") : undefined;

  // Enter in the name field moves on; only the last step creates the bot.
  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    if (step === "access") void save();
    else if (step === "identity") goNext();
  };

  if (importSource) {
    return (
      <ProfileImportView
        source={importSource}
        providersDto={providersDto}
        onBack={() => setImportSource(null)}
        onDone={onDone}
      />
    );
  }

  const levelLabel = (preset: string | undefined) => {
    const level = galleryLevel(preset);
    if (!level) return null;
    return level.kind === "autonomy"
      ? t(`newBot.autonomyLevels.${level.level}`)
      : t(`botForm.preset.${level.preset}.name`);
  };

  return (
    <form className={styles.modalForm} onSubmit={handleSubmit} noValidate data-step={step}>
      <DialogHeader
        title={t("newBot.title")}
        subtitle={t(`newBot.stepHints.${step}`)}
        subtitleId={NEW_BOT_SUBTITLE_ID}
        actions={<HomeHint />}
        closeDisabled={saving}
        onClose={onClose}
      />

      <nav className={styles.stepperWrap} aria-label={t("newBot.stepsLabel")}>
        <ol className={styles.stepper} data-testid="new-bot-steps">
          {NEW_BOT_STEPS.map((s) => {
            const current = s === step;
            const done = stepNumber(s) < stepNumber(step);
            const reachable = !current && canGoTo(step, s, { nameMissing });
            const inner = (
              <>
                <span className={styles.stepDot} aria-hidden="true">
                  {done ? <Check /> : stepNumber(s)}
                </span>
                <span className={styles.stepLabel}>{t(`newBot.steps.${s}`)}</span>
              </>
            );
            return (
              <li
                key={s}
                className={styles.stepItem}
                data-state={current ? "current" : done ? "done" : "todo"}
                aria-current={current ? "step" : undefined}
              >
                {reachable && !saving ? (
                  <button type="button" className={styles.stepButton} data-testid={`new-bot-step-${s}`} onClick={() => setStep(s)}>
                    {inner}
                  </button>
                ) : (
                  <span className={styles.stepButton}>{inner}</span>
                )}
              </li>
            );
          })}
        </ol>
      </nav>

      <DialogBody className={styles.body} testId="new-bot-body">
        {step === "start" ? (
          <>
            <div className={styles.startGrid}>
              <button
                type="button"
                className={[styles.card, styles.cardPrimary].join(" ")}
                data-testid="new-bot-start-blank"
                onClick={() => setStep("identity")}
              >
                <span className={styles.cardIcon} aria-hidden="true">
                  <Plus />
                </span>
                <span className={styles.cardText}>
                  <span className={styles.cardName}>{t("newBot.blankTitle")}</span>
                  <span className={styles.cardDesc}>{t("newBot.blankHint")}</span>
                </span>
                <ChevronRight className={styles.cardChevron} aria-hidden="true" />
              </button>
              <button
                type="button"
                className={styles.card}
                data-testid="profile-import-open"
                onClick={() => setImportSource({ kind: "file" })}
              >
                <span className={styles.cardIcon} aria-hidden="true">
                  <FileUp />
                </span>
                <span className={styles.cardText}>
                  <span className={styles.cardName}>{t("newBot.importTitle")}</span>
                  <span className={styles.cardDesc}>{t("newBot.importHint")}</span>
                </span>
                <ChevronRight className={styles.cardChevron} aria-hidden="true" />
              </button>
            </div>

            {!createdBot && gallery.length > 0 ? (
              <section
                className={styles.ready}
                aria-labelledby="new-bot-ready-title"
                data-testid="profile-gallery"
              >
                <div className={styles.readyHead}>
                  <h3 id="new-bot-ready-title" className={styles.sectionTitle}>{t("newBot.readyTitle")}</h3>
                  <p className={styles.sectionHint}>{t("newBot.readyHint")}</p>
                </div>
                <div className={styles.readyGrid}>
                  {gallery.map((p) => {
                    const level = levelLabel(p.permissions?.preset ?? undefined);
                    return (
                      <button
                        key={p.meta?.id ?? p.bot.name}
                        type="button"
                        className={styles.card}
                        data-testid={`gallery-${p.meta?.id ?? ""}`}
                        onClick={() =>
                          setImportSource({
                            kind: "template",
                            contents: JSON.stringify(p),
                            requested: p.permissions?.preset ?? undefined
                          })
                        }
                      >
                        <span className={styles.cardText}>
                          <span className={styles.cardName}>{templateName(p, i18n.language)}</span>
                          <span className={styles.cardDesc}>{templateDescription(p, i18n.language)}</span>
                          {level ? (
                            <span className={styles.cardTag}>
                              <ShieldCheck aria-hidden="true" />
                              {level}
                            </span>
                          ) : null}
                        </span>
                        <ChevronRight className={styles.cardChevron} aria-hidden="true" />
                      </button>
                    );
                  })}
                </div>
              </section>
            ) : null}
          </>
        ) : null}

        {step === "identity" ? (
          <>
            <div className={styles.looks} role="group" aria-label={t("newBot.templatesLabel")}>
              <span className={styles.looksLabel} aria-hidden="true">{t("newBot.quickLooks")}</span>
              {BOT_TEMPLATES.filter((tpl) => tpl.id !== "blank").map((tpl) => {
                const active = templateId === tpl.id;
                return (
                  <button
                    key={tpl.id}
                    type="button"
                    className={[styles.tplChip, active ? styles.tplChipActive : ""].filter(Boolean).join(" ")}
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
          </>
        ) : null}

        {step === "access" ? (
          <>
            <div className={styles.who} data-testid="new-bot-summary">
              <BotAvatar
                bot={{
                  id: createdBot?.id ?? "new-bot-preview",
                  name: draft.name.trim() || t("newBot.title"),
                  shape: draft.shape,
                  color: draft.color,
                  avatar: draft.avatar ?? undefined
                }}
                size="md"
                decorative
              />
              <span className={styles.whoName}>{draft.name.trim()}</span>
              <Button variant="ghost" size="sm" disabled={saving} onClick={() => setStep("identity")}>
                {t("newBot.changeLook")}
              </Button>
            </div>

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
              <span className={styles.advancedText}>
                <span className={styles.advancedName}>{t("newBot.advanced")}</span>
                <span className={styles.advancedHint}>{t("newBot.advancedHint")}</span>
              </span>
              <ChevronDown
                className={[styles.chevron, advancedOpen ? styles.chevronOpen : ""].filter(Boolean).join(" ")}
                aria-hidden="true"
              />
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
              <Callout tone="warning" role="status">
                <span data-testid="new-bot-host-needs-model">
                  {t("hostMode.newBotNeedsModel", { name: host.name })}
                </span>
              </Callout>
            ) : null}
          </>
        ) : null}

        {error ? (
          <Callout tone="danger" role="alert">
            {error}
          </Callout>
        ) : null}
      </DialogBody>

      <DialogFooter note={footerNote}>
        {step === "start" ? (
          <Button variant="ghost" disabled={saving} onClick={onClose}>
            {t("common.cancel")}
          </Button>
        ) : (
          <Button variant="ghost" disabled={saving} onClick={goBack} data-testid="new-bot-back">
            <ArrowLeft aria-hidden="true" />
            {t("common.back")}
          </Button>
        )}
        {step === "identity" ? (
          <Button type="submit" variant="primary" disabled={nameMissing} data-testid="new-bot-next">
            {t("newBot.next")}
            <ArrowRight aria-hidden="true" />
          </Button>
        ) : null}
        {step === "access" ? (
          <Button
            type="submit"
            variant="primary"
            aria-busy={saving}
            disabled={saving || nameMissing || executorMissing || customInvalid}
            data-testid="new-bot-submit"
          >
            {saving ? t("newBot.creating") : t("newBot.create")}
          </Button>
        ) : null}
      </DialogFooter>
    </form>
  );
}

/** New-bot dialog in three steps: how to start, name and look, what it can do (then Create). */
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
    <Modal open={open} onClose={onClose} size="md" label={t("newBot.title")} describedBy={NEW_BOT_SUBTITLE_ID}>
      {open ? <NewBotDialogContent onDone={onDone} onClose={onClose} /> : null}
    </Modal>
  );
}
