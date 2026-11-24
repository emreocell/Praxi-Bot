import {
  Eye,
  MonitorSmartphone,
  PenLine,
  ShieldCheck,
  SlidersHorizontal,
  Trash2,
  X,
  Zap
} from "lucide-react";
import { useId, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button } from "ui";
import {
  type McpServer,
  PERMISSION_CATEGORIES,
  type PermissionLevel,
  type ProvidersDto
} from "../api/types";
import {
  addFallback,
  type CustomRole,
  type CustomText,
  customRefIssue,
  type DeleteBlock,
  fallbackList,
  parseFallbackInput,
  removeFallback
} from "../lib/botEditor";
import {
  type BotDraft,
  formatModelRef,
  parseModelRef,
  PRESET_CHOICES,
  type PresetChoice
} from "../lib/botProfile";
import { brandIcon } from "../lib/brandIcons";
import { modelGroups } from "../lib/modelPicker";
import { modelLine } from "../lib/modelName";
import { BotAvatarUploadField } from "./BotAdvancedFields";
import { BotMemorySettings } from "./BotMemorySettings";
import styles from "./BotEditorFields.module.css";
import { BrandMark } from "./BrandMark";
import { CharacterColorRow, CharacterShapeGrid } from "./CharacterPicker";
import { Callout, dialogStyles, Field } from "./DialogParts";
import { ModelPicker } from "./ModelPicker";
import { Segmented, SettingGroup, SettingRow, Switch } from "./SettingsUi";

type Patch = (p: Partial<BotDraft>) => void;

const LEVELS: readonly PermissionLevel[] = ["deny", "ask", "allow"] as const;

const PRESET_ICONS: Record<PresetChoice, typeof Eye> = {
  "read-only": Eye,
  "approve-destructive": ShieldCheck,
  full: Zap,
  "read-and-draft": PenLine,
  custom: SlidersHorizontal
};


/** Identity: the face, the name and how the bot behaves. */
export function IdentityFields({
  botId,
  draft,
  patch,
  nameMissing
}: {
  botId: string;
  draft: BotDraft;
  patch: Patch;
  nameMissing: boolean;
}) {
  const { t } = useTranslation();
  const uid = useId();
  return (
    <>
      <div className={styles.identityHead}>
        <BotAvatar
          bot={{
            id: botId,
            name: draft.name,
            shape: draft.shape,
            color: draft.color,
            avatar: draft.avatar
          }}
          size="lg"
          decorative
        />
        <Field
          label={t("botForm.name")}
          htmlFor={`${uid}-name`}
          error={nameMissing ? t("botForm.nameRequired") : undefined}
        >
          <input
            id={`${uid}-name`}
            className={`${dialogStyles.input} ${styles.nameInput}`}
            value={draft.name}
            maxLength={60}
            data-autofocus
            data-testid="bot-edit-name"
            aria-invalid={nameMissing ? true : undefined}
            onChange={(e) => patch({ name: e.target.value })}
          />
        </Field>
      </div>

      <section className={dialogStyles.block}>
        <h3 className={dialogStyles.blockTitle}>{t("botEdit.look")}</h3>
        <CharacterShapeGrid
          value={draft.shape}
          color={draft.color}
          onChange={(shape) => patch({ shape })}
        />
        <CharacterColorRow
          value={draft.color}
          centered={false}
          onChange={(color) => patch({ color })}
        />
        <BotAvatarUploadField avatar={draft.avatar} onChange={(avatar) => patch({ avatar })} />
      </section>

      <Field
        label={t("botForm.persona")}
        htmlFor={`${uid}-persona`}
        hint={t("botEdit.personaHint")}
      >
        <textarea
          id={`${uid}-persona`}
          className={dialogStyles.textarea}
          rows={5}
          value={draft.persona}
          placeholder={t("botForm.personaPlaceholder")}
          onChange={(e) => patch({ persona: e.target.value })}
        />
      </Field>
    </>
  );
}

/** Models: one searchable, provider-grouped picker per role - no raw ids, no native selects. */
export function ModelFields({
  draft,
  patch,
  providersDto,
  executorMissing,
  pendingFallback,
  onPendingFallback
}: {
  draft: BotDraft;
  patch: Patch;
  providersDto: ProvidersDto | null;
  executorMissing: boolean;
  /** The "Other model id" text, owned by the dialog (so it survives a section change). */
  pendingFallback: string;
  onPendingFallback: (text: string) => void;
}) {
  const { t } = useTranslation();
  const executorRef = formatModelRef(draft.executor);
  const plannerRef = formatModelRef(draft.planner);
  const classifierRef = formatModelRef(draft.classifier);
  const executorGroups = useMemo(
    () => modelGroups(providersDto, executorRef),
    [providersDto, executorRef]
  );
  // Planner / classifier / fallback run over HTTP providers only (as before).
  const plannerGroups = useMemo(
    () => modelGroups(providersDto, plannerRef, { includeCli: false }),
    [providersDto, plannerRef]
  );
  const classifierGroups = useMemo(
    () => modelGroups(providersDto, classifierRef, { includeCli: false }),
    [providersDto, classifierRef]
  );
  const fallbackGroups = useMemo(
    () => modelGroups(providersDto, undefined, { includeCli: false }),
    [providersDto]
  );
  const fallbacks = fallbackList(draft.fallback);
  const noProviders = providersDto !== null && executorGroups.length === 0;

  const optional = (
    role: "planner" | "classifier",
    ref: string,
    groups: ReturnType<typeof modelGroups>
  ) => (
    <SettingRow
      title={t(`botForm.role.${role}`)}
      description={t(`botEdit.roleHint.${role}`)}
    >
      <ModelPicker
        groups={groups}
        value={ref}
       
        reviewOpen={false}
        cliNote={t(`botEdit.cliNote.${role}`)}
        testId={`bot-edit-${role}`}
        autoOption={{
          label: t("botEdit.sameAsMain"),
          selected: ref === "",
          onPick: () => patch({ [role]: { provider: "", model: "" } } as Partial<BotDraft>)
        }}
        onPick={(next) => patch({ [role]: parseModelRef(next) } as Partial<BotDraft>)}
      />
    </SettingRow>
  );

  return (
    <>
      {noProviders ? (
        <Callout tone="warning" title={t("botEdit.noProvidersTitle")}>
          {t("botEdit.noProvidersBody")}
        </Callout>
      ) : null}
      <SettingGroup>
        <SettingRow
          title={t("botEdit.mainModel")}
          description={
            executorMissing ? t("botForm.executorRequired") : t("botEdit.roleHint.executor")
          }
        >
          <ModelPicker
            groups={executorGroups}
            value={executorRef}
            engines
            reviewOpen={false}
            placeholder={t("botEdit.pickModel")}
            testId="bot-edit-executor"
            onPick={(next) => patch({ executor: parseModelRef(next) })}
          />
        </SettingRow>
        {optional("planner", plannerRef, plannerGroups)}
        {optional("classifier", classifierRef, classifierGroups)}
      </SettingGroup>

      <section className={dialogStyles.block}>
        <div className={dialogStyles.blockHead}>
          <h3 className={dialogStyles.blockTitle}>{t("botForm.role.fallback")}</h3>
          <p className={dialogStyles.blockLead}>{t("botEdit.fallbackHint")}</p>
        </div>
        <div className={styles.fallbacks}>
          {fallbacks.map((ref, i) => {
            const line = modelLine(ref);
            return (
              <span key={ref} className={styles.fallbackChip} title={ref}>
                <span className={styles.fallbackOrder}>{i + 1}</span>
                {line.brand ? <BrandMark name={line.brand} size={14} /> : null}
                <span className={styles.fallbackName}>{line.name || ref}</span>
                <button
                  type="button"
                  className={styles.chipRemove}
                  title={t("botEdit.removeFallback", { name: line.name || ref })}
                  aria-label={t("botEdit.removeFallback", { name: line.name || ref })}
                  onClick={() => patch({ fallback: removeFallback(draft.fallback, ref) })}
                >
                  <X aria-hidden="true" />
                </button>
              </span>
            );
          })}
          <ModelPicker
            groups={fallbackGroups}
            value=""
           
            reviewOpen={false}
            cliNote={t("botEdit.cliNote.fallback")}
            placement="bottom-start"
            placeholder={t("botEdit.addFallback")}
            testId="bot-edit-fallback"
            onPick={(next) => patch({ fallback: addFallback(draft.fallback, next) })}
          />
        </div>
        <CustomFallbackEntry
          text={pendingFallback}
          onText={onPendingFallback}
          onAdd={(refs) => patch({ fallback: refs.reduce(addFallback, draft.fallback) })}
        />
      </section>
    </>
  );
}

/** Permissions: the daemon's presets as cards, and the per-category matrix for "Custom". */
export function PermissionFields({ draft, patch }: { draft: BotDraft; patch: Patch }) {
  const { t } = useTranslation();
  const group = useId();
  return (
    <>
      <div className={styles.presets} role="radiogroup" aria-label={t("botForm.permissions")}>
        {PRESET_CHOICES.map((p) => {
          const Icon = PRESET_ICONS[p];
          const active = draft.preset === p;
          return (
            <label
              key={p}
              className={[styles.preset, active ? styles.presetActive : ""].filter(Boolean).join(" ")}
              data-testid={`bot-edit-preset-${p}`}
            >
              <input
                type="radio"
                className={dialogStyles.visuallyHidden}
                name={group}
                value={p}
                checked={active}
                onChange={() => patch({ preset: p })}
              />
              <span className={styles.presetIcon}>
                <Icon aria-hidden="true" />
              </span>
              <span className={styles.presetText}>
                <span className={styles.presetName}>{t(`botForm.preset.${p}.name`)}</span>
                <span className={styles.presetHint}>{t(`botForm.preset.${p}.hint`)}</span>
              </span>
              <span className={styles.presetDot} aria-hidden="true" />
            </label>
          );
        })}
      </div>
      {draft.preset === "custom" ? (
        <section className={dialogStyles.block}>
          <div className={dialogStyles.blockHead}>
            <h3 className={dialogStyles.blockTitle}>{t("botEdit.categories")}</h3>
            <p className={dialogStyles.blockLead}>{t("botEdit.categoriesHint")}</p>
          </div>
          <SettingGroup>
            {PERMISSION_CATEGORIES.map((c) => (
              <SettingRow key={c} title={t(`botForm.category.${c}`)}>
                <Segmented
                  label={t(`botForm.category.${c}`)}
                  value={draft.categories[c]}
                  options={LEVELS.map((l) => ({
                    value: l,
                    label: t(`botForm.level.${l}`),
                    testId: `bot-edit-cat-${c}-${l}`
                  }))}
                  onChange={(level) =>
                    patch({ categories: { ...draft.categories, [c]: level } })
                  }
                />
              </SettingRow>
            ))}
          </SettingGroup>
        </section>
      ) : null}
      <SettingGroup>
        <SettingRow
          title={t("botForm.allowEgressAfterReads.title")}
          description={t("botForm.allowEgressAfterReads.hint")}
        >
          <Switch
            checked={draft.allowEgressAfterReads}
            label={t("botForm.allowEgressAfterReads.title")}
            onChange={(allowEgressAfterReads) => patch({ allowEgressAfterReads })}
          />
        </SettingRow>
        <SettingRow title={t("botForm.manageBots")} description={t("botForm.manageBotsHint")}>
          <Switch
            checked={draft.canManageBots}
            label={t("botForm.manageBots")}
            onChange={(canManageBots) => patch({ canManageBots })}
          />
        </SettingRow>
      </SettingGroup>
      <section className={dialogStyles.block}>
        <div className={dialogStyles.blockHead}>
          <h3 className={dialogStyles.blockTitle}>{t("botForm.files.label")}</h3>
          <p className={dialogStyles.blockLead}>{t(`botForm.files.${draft.files.mode}.hint`)}</p>
        </div>
        <SettingGroup>
          <SettingRow title={t("botForm.files.label")}>
            <Segmented
              label={t("botForm.files.label")}
              value={draft.files.mode}
              options={(["computer", "folder"] as const).map((m) => ({
                value: m,
                label: t(`botForm.files.${m}.name`),
                testId: `bot-edit-files-${m}`
              }))}
              onChange={(mode) => patch({ files: { ...draft.files, mode } })}
            />
          </SettingRow>
        </SettingGroup>
        {draft.files.mode === "folder" ? (
          <textarea
            className={dialogStyles.textarea}
            rows={3}
            aria-label={t("botForm.files.foldersLabel")}
            placeholder={t("botForm.files.foldersPlaceholder")}
            value={draft.files.folders.join("\n")}
            onChange={(e) =>
              patch({ files: { ...draft.files, folders: e.target.value.split("\n") } })
            }
          />
        ) : null}
      </section>
    </>
  );
}

/** Apps: which connected apps this bot may use. */
export function AppFields({
  servers,
  allowed,
  onToggle
}: {
  servers: McpServer[] | null;
  allowed: Record<string, boolean>;
  onToggle: (serverName: string, checked: boolean) => void;
}) {
  const { t } = useTranslation();
  if (servers === null) {
    return <p className={dialogStyles.hint}>{t("botEdit.appsLoading")}</p>;
  }
  if (servers.length === 0) {
    return <Callout title={t("botForm.apps.none")}>{t("botEdit.appsNoneBody")}</Callout>;
  }
  return (
    <>
      <p className={dialogStyles.hint}>{t("botForm.apps.hint")}</p>
      <div data-testid="bot-apps">
        <SettingGroup>
          {servers.map((s) => {
            const brand = brandIcon(s.name);
            const label = brand.known ? brand.name : s.name;
            return (
              <div key={s.name} className={styles.appRow} data-testid={`bot-app-${s.name}`}>
                <BrandMark name={s.name} size={20} />
                <span className={styles.appName}>{label}</span>
                <Switch
                  checked={allowed[s.name] ?? false}
                  label={label}
                  onChange={(checked) => onToggle(s.name, checked)}
                />
              </div>
            );
          })}
        </SettingGroup>
      </div>
    </>
  );
}

/**
 * Any model id as free text - a fallback that is not in the catalog, a default-provider id, a
 * provider that is not configured here.  Several refs may be added at once, comma separated.
 */
function CustomFallbackEntry({
  text,
  onText,
  onAdd
}: {
  text: string;
  onText: (text: string) => void;
  onAdd: (refs: string[]) => void;
}) {
  const { t } = useTranslation();
  const uid = useId();
  const parsed = text.trim() === "" ? null : parseFallbackInput(text);
  const issue = parsed && "issue" in parsed ? parsed.issue : null;
  const add = () => {
    if (!parsed || "issue" in parsed) return;
    onAdd(parsed.refs);
    onText("");
  };
  return (
    <Field
      label={t("botEdit.fallbackCustom")}
      htmlFor={uid}
      hint={t("botEdit.fallbackCustomHint")}
      error={issue ? t(`botEdit.customIssue.${issue}`) : undefined}
    >
      <div className={styles.customAdd}>
        <input
          id={uid}
          className={`${dialogStyles.input} ${dialogStyles.mono}`}
          value={text}
          placeholder={t("botEdit.customPlaceholder")}
          spellCheck={false}
          autoCapitalize="off"
          data-testid="bot-edit-fallback-custom"
          aria-invalid={issue ? true : undefined}
          onChange={(e) => onText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              add();
            }
          }}
        />
        <Button
          variant="secondary"
          disabled={parsed === null || issue !== null}
          data-testid="bot-edit-fallback-custom-add"
          onClick={add}
        >
          {t("botEdit.addFallback")}
        </Button>
      </div>
    </Field>
  );
}

/** A hand-typed `provider/model` id; the text is owned by the dialog so it survives a section change. */
function CustomIdField({
  label,
  value,
  text,
  required,
  onChange
}: {
  label: string;
  value: BotDraft["executor"];
  /** The text as typed while it differs from `value` (an unusable id); else `undefined`. */
  text: string | undefined;
  required: boolean;
  onChange: (text: string) => void;
}) {
  const { t } = useTranslation();
  const uid = useId();
  const shown = text ?? formatModelRef(value);
  const empty = shown.trim() === "";
  const issue = empty && !required ? null : customRefIssue(shown);
  return (
    <Field
      label={label}
      htmlFor={uid}
      error={issue ? t(`botEdit.customIssue.${issue}`) : undefined}
    >
      <input
        id={uid}
        className={`${dialogStyles.input} ${dialogStyles.mono}`}
        value={shown}
        placeholder={t("botEdit.customPlaceholder")}
        spellCheck={false}
        autoCapitalize="off"
        aria-invalid={issue ? true : undefined}
        onChange={(e) => onChange(e.target.value)}
      />
    </Field>
  );
}

const KNOWN_TARGETS = new Set(["auto", "pc", "host", "browser", "off"]);

/** A stored computer/browser target in words (`node:<id>` and unknown values as stored). */
function targetText(t: (key: string) => string, target: string): string {
  if (target === "local") return t("botForm.local");
  // The brain stays on this computer; only the browser tools act on the paired server.
  if (target.startsWith("server:")) return t("botEdit.target.server");
  return KNOWN_TARGETS.has(target) ? t(`botEdit.target.${target}`) : target;
}

/** Advanced: the custom model id escape hatch, where the bot runs, and deleting it. */
export function AdvancedFields({
  draft,
  customText,
  onCustomText,
  computerTarget,
  browserTarget,
  deleteBlocked,
  chatCount,
  runCount,
  onDelete,
  memoryBotId
}: {
  draft: BotDraft;
  customText: CustomText;
  onCustomText: (role: CustomRole, text: string) => void;
  /** Where the bot runs, as stored (read-only here: this form never changes it). */
  computerTarget: string;
  browserTarget: string;
  /** Why the daemon would refuse to delete the bot (it needs no chats and no runs), or `null`. */
  deleteBlocked: DeleteBlock;
  chatCount: number;
  runCount: number;
  /** Absent for a bot that does not exist yet. */
  onDelete?: () => void;
  /** Existing bot: shows its memory switch (On / Ask before saving / Off). */
  memoryBotId?: string;
}) {
  const { t } = useTranslation();
  return (
    <>
      {memoryBotId ? (
        <section className={dialogStyles.block}>
          <div className={dialogStyles.blockHead}>
            <h3 className={dialogStyles.blockTitle}>{t("memorySettings.sectionTitle")}</h3>
            <p className={dialogStyles.blockLead}>{t("memorySettings.sectionHint")}</p>
          </div>
          <BotMemorySettings botId={memoryBotId} />
        </section>
      ) : null}
      <section className={dialogStyles.block}>
        <div className={dialogStyles.blockHead}>
          <h3 className={dialogStyles.blockTitle}>{t("botEdit.customTitle")}</h3>
          <p className={dialogStyles.blockLead}>{t("botEdit.customHint")}</p>
        </div>
        <CustomIdField
          label={t("botEdit.mainModel")}
          value={draft.executor}
          text={customText.executor}
          required
          onChange={(text) => onCustomText("executor", text)}
        />
        <div className={styles.pair}>
          <CustomIdField
            label={t("botForm.role.planner")}
            value={draft.planner}
            text={customText.planner}
            required={false}
            onChange={(text) => onCustomText("planner", text)}
          />
          <CustomIdField
            label={t("botForm.role.classifier")}
            value={draft.classifier}
            text={customText.classifier}
            required={false}
            onChange={(text) => onCustomText("classifier", text)}
          />
        </div>
      </section>

      <section className={dialogStyles.block}>
        <div className={dialogStyles.blockHead}>
          <h3 className={dialogStyles.blockTitle}>{t("botForm.runtime")}</h3>
          <p className={dialogStyles.blockLead}>{t("botForm.runtimeHint")}</p>
        </div>
        <SettingGroup>
          <SettingRow title={t("botForm.computer")}>
            <span className={dialogStyles.chip} data-testid="bot-edit-computer">
              <MonitorSmartphone aria-hidden="true" />
              <span>{targetText(t, computerTarget)}</span>
            </span>
          </SettingRow>
          <SettingRow title={t("botForm.browser")}>
            <span className={dialogStyles.chip} data-testid="bot-edit-browser">
              <MonitorSmartphone aria-hidden="true" />
              <span>{targetText(t, browserTarget)}</span>
            </span>
          </SettingRow>
        </SettingGroup>
      </section>

      {onDelete ? (
        <section className={dialogStyles.block}>
          <div className={dialogStyles.blockHead}>
            <h3 className={dialogStyles.blockTitle}>{t("botEdit.dangerTitle")}</h3>
          </div>
          <div className={styles.danger}>
            <div className={styles.dangerText}>
              <p className={styles.dangerTitle}>{t("botEdit.deleteTitle")}</p>
              <p className={dialogStyles.hint}>
                {deleteBlocked === "chats"
                  ? t("botEdit.deleteBlocked", { count: chatCount })
                  : deleteBlocked === "runs"
                    ? t("botEdit.deleteBlockedRuns", { count: runCount })
                    : t("botEdit.deleteHint")}
              </p>
            </div>
            <Button
              variant="secondary"
              disabled={deleteBlocked !== null}
              data-testid="bot-edit-delete"
              onClick={onDelete}
            >
              <Trash2 aria-hidden="true" />
              {t("botEdit.delete")}
            </Button>
          </div>
        </section>
      ) : null}
    </>
  );
}
