import { type FormEvent, type KeyboardEvent, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Ban,
  Check,
  ChevronDown,
  CircleHelp,
  CodeXml,
  Dices,
  FileUp,
  HeartHandshake,
  Inbox,
  MessageCircle,
  Search,
  Sparkles,
  Sunrise,
  Target
} from "lucide-react";
import { BOT_SHAPE_IDS, BotAvatar, Button } from "ui";
import { ProfileImportView, type ProfileSource } from "../components/ProfileImportView";
import { type SharedProfile, templateDescription, templateName } from "../lib/profileShare";
import type { Bot, PermissionPreset } from "../api/types";
import { BotAvatarUploadField, useBotFormDeps } from "../components/BotAdvancedFields";
import { AdvancedFields, AppFields, ModelFields, PermissionFields } from "../components/BotEditorFields";
import { BotInstructionsField } from "../components/BotInstructionsField";
import { CharacterColorRow, CharacterShapeGrid } from "../components/CharacterPicker";
import { Callout, DialogBody, DialogFooter, DialogHeader } from "../components/DialogParts";
import { Modal } from "../components/Modal";
import { ModelPicker } from "../components/ModelPicker";
import { Popover } from "../components/Popover";
import { Segmented } from "../components/SettingsUi";
import { accessLevelKey } from "../lib/accessLevels";
import { RECOMMENDED_SAFETY_PRESET } from "../lib/settingsPages";
import {
  BOT_COLORS,
  type BotDraft,
  configFromDraft,
  DEFAULT_NEW_BOT_COLOR,
  draftFromBot,
  formatModelRef,
  resolveAutoExecutorRef
} from "../lib/botProfile";
import { invalidCustomRoles } from "../lib/botEditor";
import { botHasAccess, changedApps, saveBotAndApps } from "../lib/mcpApps";
import { AvatarFileError } from "../lib/avatar";
import { botWriteMessage } from "../lib/botWrite";
import { modelGroups } from "../lib/modelPicker";
import {
  applyLook,
  createFromTemplate,
  GENERAL_CHOICE,
  levelAfterPick,
  levelsFor,
  LookNotAppliedError,
  nameAfterPick,
  purposeKey,
  shuffleLook,
  templateNeedsOtherModel
} from "../lib/newBotForm";
import { LOCAL_COMPUTER, useDaemon } from "../state/DaemonContext";
import { useEngineConsent } from "../state/EngineConsent";
import { useCustomText } from "../state/useCustomText";
import styles from "./NewBotModal.module.css";
import { HomeHint } from "../components/HomeHint";

const NEW_BOT_SUBTITLE_ID = "new-bot-subtitle";

/** A small picture per choice; a template this app does not know yet gets the sparkles. */
const CHOICE_ICONS: Record<string, typeof Search> = {
  [GENERAL_CHOICE]: MessageCircle,
  researcher: Search,
  "inbox-triage": Inbox,
  "daily-briefing": Sunrise,
  coder: CodeXml,
  "family-helper": HeartHandshake
};

const templateKey = (p: SharedProfile) => p.meta?.id ?? p.bot.name;

function NewBotDialogContent({
  onDone,
  onClose,
  onStartFromGoal
}: {
  onDone: (botId: string) => void;
  onClose: () => void;
  onStartFromGoal?: () => void;
}) {
  const { t, i18n } = useTranslation();
  const { client, bots, conn, refreshBots, activeComputerId, activeHostName } = useDaemon();
  const { askEngine, applyEngine } = useEngineConsent();
  const host = activeComputerId === LOCAL_COMPUTER ? null : { name: activeHostName ?? t("computerPicker.host") };
  const { providersDto, servers, setServers, allowed, setAllowed, initialApps, setInitialApps } =
    useBotFormDeps(client, undefined, true);

  const [createdBot, setCreatedBot] = useState<Bot | null>(null);
  // A template bot that exists but whose look the daemon refused: Create then retries the look on it.
  const [lookPending, setLookPending] = useState<{ bot: Bot; choice: string } | null>(null);
  // "Import from a file" and "See what it comes with" open the review view instead of this page.
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

  const [draft, setDraft] = useState<BotDraft>(() => ({
    ...draftFromBot(undefined),
    shape: "pebble",
    color: DEFAULT_NEW_BOT_COLOR,
    preset: RECOMMENDED_SAFETY_PRESET
  }));
  // What it should do: the general helper, or a gallery template (its meta id).
  const [choice, setChoice] = useState<string>(GENERAL_CHOICE);
  // The name the last pick filled in: replaced by the next pick, unlike a typed one.
  const [filledName, setFilledName] = useState("");
  // The owner picked a level themselves: another pick keeps it (lowered if that pick cannot get it).
  const [levelChosen, setLevelChosen] = useState(false);
  const [brainMode, setBrainMode] = useState<"auto" | "manual">("auto");
  const [moreOpen, setMoreOpen] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const helpAnchor = useRef<HTMLButtonElement | null>(null);
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
    () => modelGroups(providersDto, brainMode === "manual" ? formatModelRef(draft.executor) : undefined),
    [providersDto, brainMode, draft.executor]
  );

  // Once a general bot exists (its apps failed to apply), Create only retries those: no switching to a template.
  const templates = createdBot ? [] : gallery;
  const template = choice === GENERAL_CHOICE ? null : (templates.find((p) => templateKey(p) === choice) ?? null);
  const choices = [
    {
      id: GENERAL_CHOICE,
      name: t("newBot.quick.general"),
      line: t("newBot.quick.purpose.general"),
      full: t("newBot.quick.purpose.general")
    },
    ...templates.map((p) => {
      const id = templateKey(p);
      const key = purposeKey(id);
      const full = templateDescription(p, i18n.language);
      return { id, name: templateName(p, i18n.language), line: key ? t(key) : full, full };
    })
  ];

  const pick = (next: string) => {
    if (next === choice || saving) return;
    const tpl = next === GENERAL_CHOICE ? null : templates.find((p) => templateKey(p) === next);
    const nextName = tpl ? templateName(tpl, i18n.language) : "";
    patch({
      name: nameAfterPick(draft.name, filledName, nextName),
      preset: levelAfterPick(draft.preset, levelChosen, next, tpl?.permissions?.preset, RECOMMENDED_SAFETY_PRESET)
    });
    setFilledName(nextName);
    setChoice(next);
  };

  // The card buttons by choice, so the arrow keys move the focus to exactly the card the roving tabIndex marks.
  const choiceRefs = useRef(new Map<string, HTMLButtonElement>());

  // The cards are one radio group: arrows move the choice (and the focus), Tab leaves the group.
  const onChoiceKey = (e: KeyboardEvent<HTMLDivElement>) => {
    const step = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
    if (!step) return;
    e.preventDefault();
    const at = Math.max(0, choices.findIndex((c) => c.id === choice));
    const next = choices[(at + step + choices.length) % choices.length];
    if (!next) return;
    pick(next.id);
    choiceRefs.current.get(next.id)?.focus();
  };

  const surprise = () => {
    const next = shuffleLook({ shape: draft.shape, color: draft.color }, BOT_SHAPE_IDS, BOT_COLORS);
    patch({ shape: next.shape, color: next.color });
  };

  const nameMissing = draft.name.trim() === "";
  const executorMissing = effectiveExecutor.model.trim() === "";
  // An unusable hand-typed id blocks creating the bot, as it blocks saving an edit.
  const [pendingFallback, setPendingFallback] = useState("");
  const customInvalid = !template && (invalidCustomRoles(customText).length > 0 || pendingFallback.trim() !== "");
  // A ready-made bot cannot run on a command-line engine (the import refuses one): say so before Create.
  const needsOtherModel = templateNeedsOtherModel(choice, formatModelRef(effectiveExecutor));
  const blocked = nameMissing || executorMissing || customInvalid || needsOtherModel;

  /** A general bot: the form's profile and the chosen apps (a retry after a partial failure updates it). */
  const saveGeneral = async () => {
    if (!client) return;
    // A CLI model that only works with its own tools: the one consent step, before creating.
    const answer = await askEngine(formatModelRef(effectiveDraft.executor), createdBot?.config);
    if (answer === null) return;
    const target = createdBot ?? undefined;
    const config = configFromDraft(effectiveDraft, target);
    const input = { name: effectiveDraft.name.trim(), config };
    const { bot, failed, notApplied } = await saveBotAndApps(client, target, input, initialApps, allowed, bots.map((b) => b.id));
    if (!target) setCreatedBot(bot);
    await applyEngine(bot.id, answer);
    await refreshBots().catch(() => {});
    if (failed.length > 0 || notApplied.length > 0) {
      const fresh = await client.listMcpServers().catch(() => null);
      const problem = new Set([...failed, ...notApplied]);
      const actual = (name: string): boolean | undefined => {
        const srv = fresh?.find((x) => x.name === name);
        return srv ? botHasAccess(srv, bot.id) : undefined;
      };
      if (fresh) setServers(fresh);
      setAllowed((prev) => {
        const next = { ...prev };
        for (const name of notApplied) next[name] = actual(name) ?? prev[name];
        return next;
      });
      setInitialApps((prev) => {
        const next = { ...prev };
        for (const name of changedApps(prev, allowed)) {
          if (notApplied.includes(name)) next[name] = actual(name) ?? prev[name];
          else if (!problem.has(name)) next[name] = allowed[name];
        }
        return next;
      });
      setError(
        [
          failed.length > 0 ? t("botForm.apps.partialFail", { names: failed.join(", ") }) : "",
          notApplied.length > 0 ? t("botForm.apps.notApplied", { names: notApplied.join(", ") }) : ""
        ]
          .filter(Boolean)
          .join(" ")
      );
      return;
    }
    onDone(bot.id);
  };

  /**
   * A ready-made bot: imported like the review view does (the daemon clamps every rule to the level
   * chosen here), with the owner's model and name, then given the look picked on this page.
   */
  const saveTemplate = async (tpl: SharedProfile) => {
    if (!client) return;
    const model = formatModelRef(effectiveExecutor);
    const answer = await askEngine(model);
    if (answer === null) return;
    let bot: Bot;
    if (lookPending && lookPending.choice === choice) {
      // The bot was made by an earlier try whose look failed: give it the look again, never a second bot.
      bot = await applyLook(client, await client.getBot(lookPending.bot.id), draft);
    } else {
      try {
        bot = await createFromTemplate(client, { template: tpl, preset: draft.preset, model, name: draft.name, look: draft });
      } catch (err) {
        if (!(err instanceof LookNotAppliedError)) throw err;
        // The bot exists (and is listed); say why its look is missing and let the owner retry or close.
        setLookPending({ bot: err.bot, choice });
        await refreshBots().catch(() => {});
        throw err.reason;
      }
    }
    await applyEngine(bot.id, answer);
    await refreshBots().catch(() => {});
    onDone(bot.id);
  };

  const save = async () => {
    if (!client || blocked || saving) return;
    setSaving(true);
    setError(null);
    try {
      if (template) await saveTemplate(template);
      else await saveGeneral();
    } catch (err) {
      setError(
        err instanceof AvatarFileError
          ? t(err.reason === "tooLarge" ? "botForm.avatarTooLarge" : "botForm.avatarUnreadable")
          : botWriteMessage(err, t)
      );
    } finally {
      setSaving(false);
    }
  };

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    void save();
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

  // The levels this choice can really get (a template never above what it asks for).
  const levels = levelsFor(choice, template?.permissions?.preset);
  const level: PermissionPreset = levels.includes(draft.preset as PermissionPreset)
    ? (draft.preset as PermissionPreset)
    : (levels[0] ?? RECOMMENDED_SAFETY_PRESET);
  const capped = template !== null && levels.length < 4;
  const list = (key: string) => {
    const items = t(key, { returnObjects: true, defaultValue: [] });
    return Array.isArray(items) ? (items as string[]) : [];
  };
  const autoLabel = formatModelRef(autoExecutor);
  // The owner's default model by its readable name ("Claude Sonnet 5.5"), the id when the catalog has none.
  const autoName = groups.flatMap((g) => g.models).find((m) => m.ref === autoLabel)?.displayName || autoLabel;
  const footerNote = saving ? undefined : nameMissing ? t("newBot.nameNeeded") : undefined;

  return (
    <form className={styles.modalForm} onSubmit={handleSubmit} noValidate>
      <DialogHeader
        title={t("newBot.title")}
        subtitle={t("newBot.quick.subtitle")}
        subtitleId={NEW_BOT_SUBTITLE_ID}
        actions={<HomeHint />}
        closeDisabled={saving}
        onClose={onClose}
      />

      <DialogBody className={styles.body} testId="new-bot-body">
        <div className={styles.identity} inert={saving}>
          <div className={styles.preview}>
            <BotAvatar
              bot={{
                id: createdBot?.id ?? "new-bot-preview",
                name: draft.name.trim() || t("newBot.title"),
                shape: draft.shape,
                color: draft.color,
                avatar: draft.avatar ?? undefined
              }}
              size="hero"
              live
              decorative
            />
            <button
              type="button"
              className={styles.surprise}
              title={t("newBot.quick.surprise")}
              aria-label={t("newBot.quick.surprise")}
              data-testid="new-bot-surprise"
              onClick={surprise}
            >
              <Dices aria-hidden="true" />
            </button>
          </div>
          <div className={styles.identityFields}>
            <label className={styles.label} htmlFor="new-bot-name">
              {t("botForm.name")}
            </label>
            <input
              id="new-bot-name"
              type="text"
              className={styles.nameInput}
              data-autofocus
              data-testid="new-bot-name"
              maxLength={60}
              value={draft.name}
              placeholder={t("newBot.quick.namePlaceholder")}
              onChange={(e) => patch({ name: e.target.value })}
            />
            <CharacterShapeGrid compact value={draft.shape} color={draft.color} onChange={(shape) => patch({ shape })} />
            <CharacterColorRow compact centered={false} value={draft.color} onChange={(color) => patch({ color })} />
          </div>
        </div>

        <section className={styles.section} aria-labelledby="new-bot-what-title" inert={saving}>
          <h3 id="new-bot-what-title" className={styles.label}>
            {t("newBot.quick.what")}
          </h3>
          <div
            className={styles.choices}
            role="radiogroup"
            aria-labelledby="new-bot-what-title"
            data-testid="new-bot-choices"
            onKeyDown={onChoiceKey}
          >
            {choices.map((c, i) => {
              const active = c.id === choice;
              // The group stays reachable with Tab even if the checked card disappeared.
              const tabStop = active || (i === 0 && !choices.some((x) => x.id === choice));
              const Icon = CHOICE_ICONS[c.id] ?? Sparkles;
              return (
                <button
                  key={c.id}
                  type="button"
                  role="radio"
                  aria-checked={active}
                  tabIndex={tabStop ? 0 : -1}
                  className={[styles.choice, active ? styles.choiceActive : ""].filter(Boolean).join(" ")}
                  title={c.full}
                  ref={(el) => {
                    if (el) choiceRefs.current.set(c.id, el);
                    else choiceRefs.current.delete(c.id);
                  }}
                  data-testid={`new-bot-choice-${c.id}`}
                  onClick={() => pick(c.id)}
                >
                  <span className={styles.choiceIcon} aria-hidden="true">
                    <Icon />
                  </span>
                  <span className={styles.choiceText}>
                    <span className={styles.choiceName}>{c.name}</span>
                    <span className={styles.choiceLine}>{c.line}</span>
                  </span>
                </button>
              );
            })}
          </div>
          <div className={styles.otherWays}>
            {onStartFromGoal && !createdBot ? (
              <button type="button" className={styles.link} data-testid="new-bot-start-goal" onClick={onStartFromGoal}>
                <Target aria-hidden="true" />
                {t("recipes.entry")}
              </button>
            ) : null}
            <button
              type="button"
              className={styles.link}
              data-testid="profile-import-open"
              onClick={() => setImportSource({ kind: "file" })}
            >
              <FileUp aria-hidden="true" />
              {t("newBot.quick.fromFile")}
            </button>
          </div>
        </section>

        <section className={styles.section} aria-labelledby="new-bot-access-title" inert={saving}>
          <div className={styles.accessHead}>
            <h3 id="new-bot-access-title" className={styles.label}>
              {t("accessLevel.title")}
            </h3>
            <button
              ref={helpAnchor}
              type="button"
              className={styles.help}
              aria-label={t("newBot.quick.accessHelp")}
              title={t("newBot.quick.accessHelp")}
              aria-haspopup="dialog"
              aria-expanded={helpOpen}
              aria-controls={helpOpen ? "new-bot-access-popover" : undefined}
              data-testid="new-bot-access-help"
              onClick={() => setHelpOpen((v) => !v)}
            >
              <CircleHelp aria-hidden="true" />
            </button>
            <div className={styles.accessControl}>
              <Segmented
                label={t("accessLevel.title")}
                value={level}
                options={levels.map((p) => ({
                  value: p,
                  label: t(`${accessLevelKey(p)}.name`),
                  testId: `new-bot-access-${p}`
                }))}
                onChange={(preset) => {
                  setLevelChosen(true);
                  patch({ preset });
                }}
              />
            </div>
          </div>
          <p className={styles.accessLine} data-testid="new-bot-access-line">
            {t(`${accessLevelKey(level)}.line`)}
            {capped && template ? (
              <span className={styles.accessCap} data-testid="new-bot-access-cap">
                {" "}
                {t("newBot.quick.templateCap", {
                  name: templateName(template, i18n.language),
                  level: t(`${accessLevelKey(levels[0] ?? level)}.name`)
                })}
              </span>
            ) : null}
          </p>
        </section>

        <Popover
          open={helpOpen}
          anchor={helpAnchor}
          onClose={() => setHelpOpen(false)}
          role="dialog"
          placement="top-start"
          label={t(`${accessLevelKey(level)}.name`)}
          width={360}
        >
          <div id="new-bot-access-popover" className={styles.helpBody} data-testid="new-bot-access-popover">
            <p className={styles.helpTitle}>
              {t(`${accessLevelKey(level)}.name`)}
              {level === RECOMMENDED_SAFETY_PRESET ? <span className={styles.tag}>{t("accessLevel.recommended")}</span> : null}
            </p>
            <p className={styles.helpLead}>{t("accessLevel.lead")}</p>
            <p className={styles.helpHeading}>{t("accessLevel.canTitle")}</p>
            <ul className={styles.helpList}>
              {list(`${accessLevelKey(level)}.can`).map((line) => (
                <li key={line}>
                  <Check className={styles.can} aria-hidden="true" />
                  {line}
                </li>
              ))}
            </ul>
            <p className={styles.helpHeading}>{t("accessLevel.stopsTitle")}</p>
            <ul className={styles.helpList}>
              {list(`${accessLevelKey(level)}.stops`).map((line) => (
                <li key={line}>
                  <Ban className={styles.stops} aria-hidden="true" />
                  {line}
                </li>
              ))}
            </ul>
          </div>
        </Popover>

        <div className={styles.more}>
          <button
            type="button"
            className={styles.moreToggle}
            aria-expanded={moreOpen}
            aria-controls={moreOpen ? "new-bot-more-panel" : undefined}
            data-testid="new-bot-more"
            onClick={() => setMoreOpen((v) => !v)}
          >
            <ChevronDown
              className={[styles.chevron, moreOpen ? styles.chevronOpen : ""].filter(Boolean).join(" ")}
              aria-hidden="true"
            />
            <span className={styles.moreName}>{t("newBot.quick.more")}</span>
            <span className={styles.moreHint}>
              {template ? t("newBot.quick.moreHintTemplate") : t("newBot.quick.moreHint")}
            </span>
          </button>

          {moreOpen ? (
            <div id="new-bot-more-panel" className={styles.morePanel} data-testid="new-bot-more-panel" inert={saving}>
              <div className={styles.brainRow}>
                <div className={styles.brainText}>
                  <span className={styles.brainName}>{t("newBot.brain")}</span>
                  <span className={styles.brainHint}>
                    {autoLabel ? t("newBot.quick.brainDefault", { model: autoName }) : t("newBot.brainHint")}
                  </span>
                </div>
                <ModelPicker
                  groups={groups}
                  value={brainMode === "auto" ? "" : formatModelRef(draft.executor)}
                  autoOption={{
                    label: t("newBot.brainAuto"),
                    sub: autoLabel || undefined,
                    selected: brainMode === "auto",
                    onPick: () => setBrainMode("auto")
                  }}
                  reviewOpen={false}
                  placement="bottom-end"
                  engines={!template}
                  testId="new-bot-brain"
                  onPick={(ref) => {
                    const i = ref.indexOf("/");
                    setBrainMode("manual");
                    patch({
                      executor: i < 0 ? { provider: "", model: ref } : { provider: ref.slice(0, i), model: ref.slice(i + 1) }
                    });
                  }}
                />
              </div>

              {template ? (
                <div className={styles.templateNote}>
                  <p>{t("newBot.quick.templateNote", { name: templateName(template, i18n.language) })}</p>
                  <Button
                    variant="secondary"
                    size="sm"
                    data-testid="new-bot-template-review"
                    onClick={() =>
                      setImportSource({
                        kind: "template",
                        contents: JSON.stringify(template),
                        requested: template.permissions?.preset ?? undefined
                      })
                    }
                  >
                    {t("newBot.quick.review")}
                  </Button>
                </div>
              ) : (
                <>
                  <BotInstructionsField value={draft.persona} onChange={(persona) => patch({ persona })} />
                  <BotAvatarUploadField avatar={draft.avatar} onChange={(avatar) => patch({ avatar })} />
                  <div>
                    <h3 className={styles.moreHeading}>{t("botForm.apps.title")}</h3>
                    <AppFields
                      servers={servers}
                      allowed={allowed}
                      onToggle={(name, checked) => setAllowed((prev) => ({ ...prev, [name]: checked }))}
                    />
                  </div>
                  <div>
                    <h3 className={styles.moreHeading}>{t("botForm.permissions")}</h3>
                    <PermissionFields draft={draft} patch={patch} showLevel={false} />
                  </div>
                  <div>
                    <h3 className={styles.moreHeading}>{t("botForm.models")}</h3>
                    <ModelFields
                      draft={effectiveDraft}
                      providersDto={providersDto}
                      executorMissing={executorMissing}
                      pendingFallback={pendingFallback}
                      onPendingFallback={setPendingFallback}
                      patch={(next) => {
                        if (next.executor) setBrainMode("manual");
                        patch(next);
                      }}
                    />
                  </div>
                  <AdvancedFields
                    draft={effectiveDraft}
                    customText={customText}
                    onCustomText={(role, text) => {
                      if (role === "executor") setBrainMode("manual");
                      setCustom(role, text);
                    }}
                    deleteBlocked={null}
                    chatCount={0}
                    runCount={0}
                  />
                </>
              )}
            </div>
          ) : null}
        </div>

        {needsOtherModel ? (
          <Callout tone="warning" role="status">
            <span data-testid="new-bot-template-cli">{t("newBot.quick.templateCli")}</span>
          </Callout>
        ) : null}

        {host && executorMissing ? (
          <Callout tone="warning" role="status">
            <span data-testid="new-bot-host-needs-model">{t("hostMode.newBotNeedsModel", { name: host.name })}</span>
          </Callout>
        ) : null}

        {error ? (
          <Callout tone="danger" role="alert">
            {error}
          </Callout>
        ) : null}
      </DialogBody>

      <DialogFooter note={footerNote}>
        <Button variant="ghost" disabled={saving} onClick={onClose}>
          {t("common.cancel")}
        </Button>
        <Button
          type="submit"
          variant="primary"
          aria-busy={saving}
          disabled={saving || blocked}
          data-testid="new-bot-submit"
        >
          {saving ? t("newBot.creating") : t("newBot.create")}
        </Button>
      </DialogFooter>
    </form>
  );
}

/** New-bot dialog on one page: name and look, what it should do, how freely it works, Create. */
export function NewBotModal({
  open,
  onDone,
  onClose,
  onStartFromGoal
}: {
  open: boolean;
  onDone: (botId: string) => void;
  onClose: () => void;
  /** Opens "Start from a goal" instead (this dialog closes). */
  onStartFromGoal?: () => void;
}) {
  const { t } = useTranslation();
  return (
    <Modal open={open} onClose={onClose} size="md" label={t("newBot.title")} describedBy={NEW_BOT_SUBTITLE_ID}>
      {open ? <NewBotDialogContent onDone={onDone} onClose={onClose} onStartFromGoal={onStartFromGoal} /> : null}
    </Modal>
  );
}
