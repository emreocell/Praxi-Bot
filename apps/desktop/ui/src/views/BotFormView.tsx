import { type ChangeEvent, type CSSProperties, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Avatar, Button, Card } from "ui";
import {
  type McpServer,
  PERMISSION_CATEGORIES,
  type PermissionCategoryName,
  type PermissionLevel
} from "../api/types";
import formStyles from "../components/Form.module.css";
import { useToast } from "../components/Toast";
import { avatarFromFile, AvatarFileError } from "../lib/avatar";
import {
  BOT_COLORS,
  type BotDraft,
  configFromDraft,
  draftFromBot,
  type ModelRef,
  PRESET_CHOICES,
  type PresetChoice
} from "../lib/botProfile";
import { botHasAccess, botListAfterToggle } from "../lib/mcpApps";
import { FAKE_PROVIDER_ID } from "../lib/providers";
import { type ProvidersView, providersView } from "../lib/providerView";
import { useDaemon } from "../state/DaemonContext";
import styles from "./BotFormView.module.css";

const LEVELS: readonly PermissionLevel[] = ["deny", "ask", "allow"] as const;

function ModelField({
  label,
  value,
  providers,
  required,
  onChange
}: {
  label: string;
  value: ModelRef;
  providers: string[];
  required?: boolean;
  onChange: (v: ModelRef) => void;
}) {
  const { t } = useTranslation();
  const known = providers.includes(value.provider) || value.provider === "";
  const options = known ? providers : [value.provider, ...providers];
  return (
    <div className={formStyles.field}>
      <span className={formStyles.label}>
        {label}
        {required ? " *" : ""}
      </span>
      <div className={formStyles.row}>
        <select
          className={formStyles.select}
          value={value.provider}
          aria-label={`${label}: ${t("botForm.provider")}`}
          onChange={(e) => onChange({ ...value, provider: e.target.value })}
        >
          <option value="">{t("botForm.defaultProvider")}</option>
          {options.map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
        </select>
        <input
          className={`${formStyles.input} ${formStyles.grow}`}
          value={value.model}
          placeholder={t("botForm.modelPlaceholder")}
          aria-label={`${label}: ${t("botForm.model")}`}
          onChange={(e) => onChange({ ...value, model: e.target.value })}
        />
      </div>
    </div>
  );
}

export function BotFormView({
  botId,
  onDone,
  onCancel
}: {
  /** `undefined` = create a new bot. */
  botId?: string;
  onDone: (botId: string) => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const { client, bots, conn, refreshBots } = useDaemon();
  const existing = bots.find((b) => b.id === botId);
  // Apps: one source of truth, the server's `bots` list (bot UUIDs).  `null`
  // until loaded; `allowed` is this bot's ticked servers by name.
  const [servers, setServers] = useState<McpServer[] | null>(null);
  const [allowed, setAllowed] = useState<Record<string, boolean>>({});
  const [draft, setDraft] = useState<BotDraft>(() => draftFromBot(existing));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [avatarError, setAvatarError] = useState<string | null>(null);
  const [providerList, setProviderList] = useState<ProvidersView>(() =>
    providersView(null)
  );
  const fileRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    if (!client) {
      return undefined;
    }
    let cancelled = false;
    client
      .providers()
      .then((p) => {
        if (!cancelled) {
          setProviderList(providersView(p));
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [client]);

  useEffect(() => {
    if (!client) {
      return undefined;
    }
    let cancelled = false;
    client
      .listMcpServers()
      .then((list) => {
        if (!cancelled) {
          setServers(list);
          setAllowed(
            Object.fromEntries(
              list.map((s) => [s.name, botHasAccess(s, existing?.id)])
            )
          );
        }
      })
      .catch(() => {
        if (!cancelled) {
          setServers([]);
        }
      });
    return () => {
      cancelled = true;
    };
    // The list is loaded once per form; ticks are local until Save.
  }, [client, existing?.id]);

  const providerIds = useMemo(() => {
    const ids = new Set<string>(providerList.selectable);
    if (conn?.fake_provider) {
      ids.add(FAKE_PROVIDER_ID);
    }
    return [...ids];
  }, [providerList, conn?.fake_provider]);

  const patch = (p: Partial<BotDraft>) => setDraft((d) => ({ ...d, ...p }));

  const onFile = async (e: ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) {
      return;
    }
    setAvatarError(null);
    try {
      patch({ avatar: await avatarFromFile(file) });
    } catch (err) {
      setAvatarError(
        err instanceof AvatarFileError && err.reason === "tooLarge"
          ? t("botForm.avatarTooLarge")
          : t("botForm.avatarUnreadable")
      );
    }
  };

  const nameMissing = draft.name.trim() === "";
  const executorMissing = draft.executor.model.trim() === "";

  /** Write this bot's Apps ticks back to the servers' `bots` lists. */
  const saveApps = async (savedId: string) => {
    if (!client || !servers) {
      return;
    }
    try {
      // Work from the servers as they are now, not as they were when the form opened.
      const fresh = await client.listMcpServers();
      const allIds = bots.map((b) => b.id);
      for (const s of fresh) {
        const want = allowed[s.name];
        if (want === undefined || want === botHasAccess(s, savedId)) {
          continue;
        }
        const list = botListAfterToggle(s, savedId, want, allIds);
        if (list === null && s.bots === null) {
          continue;
        }
        await client.updateMcpServer(s.name, { bots: list });
      }
    } catch (e) {
      toast.show(e instanceof Error ? e.message : String(e), "danger");
    }
  };

  const save = async () => {
    if (!client || nameMissing || executorMissing) {
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const config = configFromDraft(draft, existing);
      const input = { name: draft.name.trim(), config };
      const saved = existing
        ? await client.updateBot(existing.id, input)
        : await client.createBot(input);
      await saveApps(saved.id);
      await refreshBots();
      onDone(saved.id);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className={styles.pane}>
      <div className={styles.container}>
        <header className={styles.header}>
          <h1 className={styles.title}>
            {existing ? t("botForm.editTitle") : t("botForm.createTitle")}
          </h1>
        </header>

        <Card padding="md" className={styles.section}>
          <h2 className={styles.heading}>{t("botForm.identity")}</h2>
          <div className={formStyles.field}>
            <label className={formStyles.label} htmlFor="bot-name">
              {t("botForm.name")} *
            </label>
            <input
              id="bot-name"
              className={formStyles.input}
              value={draft.name}
              maxLength={60}
              onChange={(e) => patch({ name: e.target.value })}
            />
            {nameMissing ? (
              <p className={formStyles.hint}>{t("botForm.nameRequired")}</p>
            ) : null}
          </div>

          <div className={formStyles.field}>
            <span className={formStyles.label}>{t("botForm.avatar")}</span>
            <div className={formStyles.row}>
              <Avatar
                name={draft.name || "?"}
                src={draft.avatar ?? undefined}
                color={draft.color}
                size="lg"
              />
              <input
                ref={fileRef}
                type="file"
                accept="image/png,image/jpeg,image/webp,image/gif"
                className={styles.hiddenFile}
                onChange={(e) => void onFile(e)}
              />
              <Button
                size="sm"
                variant="secondary"
                onClick={() => fileRef.current?.click()}
              >
                {t("botForm.avatarChoose")}
              </Button>
              {draft.avatar ? (
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => patch({ avatar: null })}
                >
                  {t("botForm.avatarUseInitials")}
                </Button>
              ) : (
                <span className={formStyles.hint}>
                  {t("botForm.avatarInitialsHint")}
                </span>
              )}
            </div>
            {avatarError ? (
              <p className={formStyles.error}>{avatarError}</p>
            ) : (
              <p className={formStyles.hint}>{t("botForm.avatarHint")}</p>
            )}
          </div>

          <div className={formStyles.field}>
            <span className={formStyles.label}>{t("botForm.color")}</span>
            <div className={styles.swatches}>
              {BOT_COLORS.map((c) => (
                <button
                  key={c}
                  type="button"
                  className={[
                    styles.swatch,
                    draft.color === c ? styles.swatchActive : ""
                  ]
                    .filter(Boolean)
                    .join(" ")}
                  style={{ "--swatch-color": c } as CSSProperties}
                  aria-label={c}
                  aria-pressed={draft.color === c}
                  onClick={() => patch({ color: c })}
                />
              ))}
              <input
                type="color"
                className={styles.colorInput}
                value={draft.color}
                aria-label={t("botForm.colorCustom")}
                onChange={(e) => patch({ color: e.target.value })}
              />
            </div>
          </div>

          <div className={formStyles.field}>
            <label className={formStyles.label} htmlFor="bot-persona">
              {t("botForm.persona")}
            </label>
            <textarea
              id="bot-persona"
              className={formStyles.textarea}
              value={draft.persona}
              placeholder={t("botForm.personaPlaceholder")}
              onChange={(e) => patch({ persona: e.target.value })}
            />
          </div>
        </Card>

        <Card padding="md" className={styles.section}>
          <h2 className={styles.heading}>{t("botForm.models")}</h2>
          <p className={formStyles.hint}>{t("botForm.modelsHint")}</p>
          <ModelField
            label={t("botForm.role.executor")}
            required
            value={draft.executor}
            providers={providerIds}
            onChange={(executor) => patch({ executor })}
          />
          {executorMissing ? (
            <p className={formStyles.hint}>{t("botForm.executorRequired")}</p>
          ) : null}
          <ModelField
            label={t("botForm.role.planner")}
            value={draft.planner}
            providers={providerIds}
            onChange={(planner) => patch({ planner })}
          />
          <ModelField
            label={t("botForm.role.classifier")}
            value={draft.classifier}
            providers={providerIds}
            onChange={(classifier) => patch({ classifier })}
          />
          <div className={formStyles.field}>
            <label className={formStyles.label} htmlFor="bot-fallback">
              {t("botForm.role.fallback")}
            </label>
            <input
              id="bot-fallback"
              className={formStyles.input}
              value={draft.fallback}
              placeholder={t("botForm.fallbackPlaceholder")}
              onChange={(e) => patch({ fallback: e.target.value })}
            />
          </div>
        </Card>

        <Card padding="md" className={styles.section}>
          <h2 className={styles.heading}>{t("botForm.permissions")}</h2>
          <div
            className={styles.presets}
            role="radiogroup"
            aria-label={t("botForm.permissions")}
          >
            {PRESET_CHOICES.map((p: PresetChoice) => (
              <label
                key={p}
                className={[
                  styles.preset,
                  draft.preset === p ? styles.presetActive : ""
                ]
                  .filter(Boolean)
                  .join(" ")}
              >
                <input
                  type="radio"
                  name="preset"
                  value={p}
                  checked={draft.preset === p}
                  onChange={() => patch({ preset: p })}
                />
                <span className={styles.presetText}>
                  <span className={styles.presetName}>
                    {t(`botForm.preset.${p}.name`)}
                  </span>
                  <span className={formStyles.hint}>
                    {t(`botForm.preset.${p}.hint`)}
                  </span>
                </span>
              </label>
            ))}
          </div>
          {draft.preset === "custom" ? (
            <div className={styles.categories}>
              {PERMISSION_CATEGORIES.map((c: PermissionCategoryName) => (
                <div key={c} className={formStyles.field}>
                  <label
                    className={formStyles.label}
                    htmlFor={`cat-${c}`}
                  >
                    {t(`botForm.category.${c}`)}
                  </label>
                  <select
                    id={`cat-${c}`}
                    className={formStyles.select}
                    value={draft.categories[c]}
                    onChange={(e) =>
                      patch({
                        categories: {
                          ...draft.categories,
                          [c]: e.target.value as PermissionLevel
                        }
                      })
                    }
                  >
                    {LEVELS.map((l) => (
                      <option key={l} value={l}>
                        {t(`botForm.level.${l}`)}
                      </option>
                    ))}
                  </select>
                </div>
              ))}
            </div>
          ) : null}
        </Card>

        <Card padding="md" className={styles.section}>
          <h2 className={styles.heading}>{t("botForm.apps.title")}</h2>
          {servers !== null && servers.length === 0 ? (
            <p className={formStyles.hint}>{t("botForm.apps.none")}</p>
          ) : (
            <>
              <p className={formStyles.hint}>{t("botForm.apps.hint")}</p>
              <ul className={styles.appList} data-testid="bot-apps">
                {(servers ?? []).map((s) => (
                  <li key={s.name}>
                    <label className={styles.appItem}>
                      <input
                        type="checkbox"
                        className={styles.appCheck}
                        checked={allowed[s.name] ?? false}
                        data-testid={`bot-app-${s.name}`}
                        onChange={(e) =>
                          setAllowed((a) => ({ ...a, [s.name]: e.target.checked }))
                        }
                      />
                      <span className={styles.appName}>{s.name}</span>
                    </label>
                  </li>
                ))}
              </ul>
            </>
          )}
        </Card>

        <Card padding="md" className={styles.section}>
          <h2 className={styles.heading}>{t("botForm.runtime")}</h2>
          <div className={formStyles.row}>
            <div className={`${formStyles.field} ${formStyles.grow}`}>
              <label className={formStyles.label} htmlFor="bot-computer">
                {t("botForm.computer")}
              </label>
              <select
                id="bot-computer"
                className={formStyles.select}
                value={draft.computer}
                onChange={() => patch({ computer: "local" })}
              >
                <option value="local">{t("botForm.local")}</option>
                <option value="remote" disabled>
                  {t("botForm.remoteSoon")}
                </option>
              </select>
            </div>
            <div className={`${formStyles.field} ${formStyles.grow}`}>
              <label className={formStyles.label} htmlFor="bot-browser">
                {t("botForm.browser")}
              </label>
              <select
                id="bot-browser"
                className={formStyles.select}
                value={draft.browser}
                onChange={() => patch({ browser: "local" })}
              >
                <option value="local">{t("botForm.local")}</option>
                <option value="remote" disabled>
                  {t("botForm.remoteSoon")}
                </option>
              </select>
            </div>
          </div>
          <p className={formStyles.hint}>{t("botForm.runtimeHint")}</p>
        </Card>

        {error ? (
          <p className={formStyles.error} role="alert">
            {error}
          </p>
        ) : null}
        <div className={styles.footer}>
          <Button variant="secondary" onClick={onCancel}>
            {t("common.cancel")}
          </Button>
          <Button
            variant="primary"
            disabled={saving || nameMissing || executorMissing}
            onClick={() => void save()}
          >
            {existing ? t("common.save") : t("botForm.create")}
          </Button>
        </div>
      </div>
    </div>
  );
}
