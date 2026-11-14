import { type FormEvent, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown, ChevronUp, Sparkles, X } from "lucide-react";
import { BotAvatar, Button } from "ui";
import type { Bot } from "../api/types";
import {
  BotAppsFields,
  BotAvatarUploadField,
  BotModelRolesFields,
  BotPermissionsFields,
  BotRuntimeFields,
  useBotFormDeps
} from "../components/BotAdvancedFields";
import { CharacterColorRow, CharacterShapeGrid } from "../components/CharacterPicker";
import formStyles from "../components/Form.module.css";
import { Modal } from "../components/Modal";
import {
  AUTONOMY_LEVELS,
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
import { botHasAccess, changedApps, saveBotAndApps } from "../lib/mcpApps";
import { modelGroups } from "../lib/modelPicker";
import { useDaemon } from "../state/DaemonContext";
import styles from "./NewBotModal.module.css";

function NewBotDialogContent({
  onDone,
  onClose
}: {
  onDone: (botId: string) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const { client, bots, conn, refreshBots } = useDaemon();
  const {
    providersDto,
    providerIds,
    httpProviders,
    servers,
    setServers,
    allowed,
    setAllowed,
    initialApps,
    setInitialApps
  } = useBotFormDeps(client, undefined, Boolean(conn?.fake_provider), true);

  const [createdBot, setCreatedBot] = useState<Bot | null>(null);
  const [templateId, setTemplateId] = useState<BotTemplateId | null>("blank");
  const [draft, setDraft] = useState<BotDraft>(() => ({
    ...draftFromBot(undefined),
    shape: "pebble",
    color: DEFAULT_NEW_BOT_COLOR
  }));
  const [brainMode, setBrainMode] = useState<"auto" | "manual">("auto");
  const [brainMenuOpen, setBrainMenuOpen] = useState(false);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const patch = (p: Partial<BotDraft>) => setDraft((d) => ({ ...d, ...p }));

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
    () => modelGroups(providersDto, formatModelRef(effectiveExecutor)),
    [providersDto, effectiveExecutor]
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

  const brainButtonLabel =
    brainMode === "auto"
      ? t("newBot.brainAuto")
      : effectiveExecutor.model.trim() || t("newBot.brainAuto");

  const save = async () => {
    if (!client || nameMissing || executorMissing || saving) {
      return;
    }
    setSaving(true);
    setError(null);
    try {
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
      setError(err instanceof Error ? err.message : String(err));
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

        <div className={styles.preview}>
          <BotAvatar
            bot={{
              id: createdBot?.id ?? "new-bot-preview",
              name: draft.name.trim() || t("newBot.title"),
              shape: draft.shape,
              color: draft.color,
              avatar: draft.avatar
            }}
            size="hero"
            live
            decorative
          />
        </div>

        <div className={styles.nameWrap}>
          <input
            type="text"
            className={styles.nameInput}
            data-autofocus
            data-testid="new-bot-name"
            maxLength={24}
            value={draft.name}
            placeholder={t("newBot.namePlaceholder")}
            aria-label={t("botForm.name")}
            onChange={(e) => {
              setTemplateId(null);
              patch({ name: e.target.value });
            }}
          />
        </div>

        <CharacterShapeGrid
          value={draft.shape}
          color={draft.color}
          onChange={(shape) => {
            setTemplateId(null);
            patch({ shape });
          }}
        />

        <CharacterColorRow
          value={draft.color}
          onChange={(color) => {
            setTemplateId(null);
            patch({ color });
          }}
        />

        <div className={styles.settings}>
          <div className={styles.kvRow}>
            <div className={styles.kvMeta}>
              <span className={styles.kvLabel}>{t("newBot.brain")}</span>
              <p className={styles.kvHint}>{t("newBot.brainHint")}</p>
            </div>
            <button
              type="button"
              className={styles.brainButton}
              aria-expanded={brainMenuOpen}
              data-testid="new-bot-brain"
              onClick={() => setBrainMenuOpen((v) => !v)}
            >
              <Sparkles aria-hidden="true" />
              <span className={styles.brainLabel}>{brainButtonLabel}</span>
              <ChevronDown aria-hidden="true" />
            </button>
          </div>

          {brainMenuOpen ? (
            <div className={styles.brainMenu} role="listbox" aria-label={t("newBot.brain")}>
              <button
                type="button"
                role="option"
                aria-selected={brainMode === "auto"}
                className={[styles.brainOption, brainMode === "auto" ? styles.brainOptionActive : ""]
                  .filter(Boolean)
                  .join(" ")}
                onClick={() => {
                  setBrainMode("auto");
                  setBrainMenuOpen(false);
                }}
              >
                <span>{t("newBot.brainAuto")}</span>
                {autoExecutor.model ? (
                  <span className={styles.brainSub}>{formatModelRef(autoExecutor)}</span>
                ) : null}
              </button>
              {groups.map((g) => (
                <div key={g.provider}>
                  <div className={styles.brainGroupTitle}>{g.provider}</div>
                  {g.models.map((m) => {
                    const selected =
                      brainMode === "manual" &&
                      formatModelRef(draft.executor) === m.ref;
                    return (
                      <button
                        key={m.ref}
                        type="button"
                        role="option"
                        aria-selected={selected}
                        className={[
                          styles.brainOption,
                          selected ? styles.brainOptionActive : ""
                        ]
                          .filter(Boolean)
                          .join(" ")}
                        onClick={() => {
                          setBrainMode("manual");
                          patch({ executor: { provider: m.provider, model: m.model } });
                          setBrainMenuOpen(false);
                        }}
                      >
                        <span>{m.model}</span>
                      </button>
                    );
                  })}
                </div>
              ))}
            </div>
          ) : null}

          <div className={styles.kvRow}>
            <span className={styles.kvLabel}>{t("newBot.autonomy")}</span>
            <div
              className={styles.segmented}
              role="radiogroup"
              aria-label={t("newBot.autonomy")}
            >
              {AUTONOMY_LEVELS.map((level) => {
                const selected = activeAutonomy === level;
                return (
                  <button
                    key={level}
                    type="button"
                    role="radio"
                    aria-checked={selected}
                    className={[styles.segButton, selected ? styles.segActive : ""]
                      .filter(Boolean)
                      .join(" ")}
                    data-testid={`autonomy-${level}`}
                    onClick={() => patch({ preset: autonomyToPreset(level) })}
                  >
                    {t(`newBot.autonomyLevels.${level}`)}
                  </button>
                );
              })}
            </div>
          </div>

          <p className={styles.kvHint}>
            {activeAutonomy
              ? t(`newBot.autonomyHints.${activeAutonomy}`)
              : t("newBot.autonomyCustom", {
                  preset: t(`botForm.preset.${draft.preset}.name`)
                })}
          </p>
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
              <BotModelRolesFields
                draft={effectiveDraft}
                providerIds={providerIds}
                httpProviders={httpProviders}
                providersDto={providersDto}
                executorMissing={executorMissing}
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
              <BotPermissionsFields draft={draft} patch={patch} />
            </div>

            <div>
              <h3 className={styles.advancedHeading}>{t("botForm.apps.title")}</h3>
              <BotAppsFields
                servers={servers}
                allowed={allowed}
                onToggle={(name, checked) =>
                  setAllowed((prev) => ({ ...prev, [name]: checked }))
                }
              />
            </div>

            <div>
              <h3 className={styles.advancedHeading}>{t("botForm.runtime")}</h3>
              <BotRuntimeFields draft={draft} patch={patch} />
            </div>
          </div>
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
          disabled={saving || nameMissing || executorMissing}
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
