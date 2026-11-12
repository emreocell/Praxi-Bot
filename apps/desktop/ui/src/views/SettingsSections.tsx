import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button, Card } from "ui";
import {
  type Bot,
  type GrantTarget,
  PERMISSION_CATEGORIES,
  type PolicyDto
} from "../api/types";
import { isTauriRuntime } from "../overlay/events";
import formStyles from "../components/Form.module.css";
import { botView } from "../lib/botProfile";
import { addProvider, setDefaultProvider, testProvider } from "../lib/providers";
import { type ProviderRow, providersView } from "../lib/providerView";
import { useDaemon } from "../state/DaemonContext";
import styles from "./SettingsSections.module.css";

const CUSTOM = "__custom__";

interface Outcome {
  ok: boolean;
  text: string;
}

function outcomeOf(r: { code: number; stdout: string; stderr: string }): Outcome {
  const text = (r.code === 0 ? r.stdout : r.stderr || r.stdout).trim();
  return { ok: r.code === 0, text };
}

// ------------------------------------------------------------------ providers

export function ProvidersSection() {
  const { t } = useTranslation();
  const { client, conn, restartDaemon } = useDaemon();
  const native = isTauriRuntime();
  const [list, setList] = useState<ProviderRow[]>([]);
  const [catalog, setCatalog] = useState<string[]>(() => providersView(null).catalog);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [choice, setChoice] = useState<string>(() => providersView(null).catalog[0]);
  const [customId, setCustomId] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [model, setModel] = useState("");
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const [needsRestart, setNeedsRestart] = useState(false);

  const reload = useCallback(async () => {
    if (!client) {
      return;
    }
    try {
      const view = providersView(await client.providers());
      setList(view.configured);
      setCatalog(view.catalog);
      setChoice((c) => (c === CUSTOM || view.catalog.includes(c) ? c : view.catalog[0]));
      setLoadError(null);
    } catch (e) {
      setLoadError(e instanceof Error ? e.message : String(e));
    }
  }, [client]);

  useEffect(() => {
    void reload();
  }, [reload]);

  const run = async (fn: () => Promise<Outcome>, changes: boolean) => {
    setBusy(true);
    setOutcome(null);
    try {
      const o = await fn();
      setOutcome(o);
      if (o.ok && changes) {
        // The daemon rebuilds its providers in place; only when it cannot
        // (an older daemon, the fake provider) is a restart needed.
        let reloaded = false;
        try {
          await client?.reloadProviders();
          reloaded = true;
        } catch {
          reloaded = false;
        }
        setNeedsRestart(!reloaded);
      }
    } catch (e) {
      setOutcome({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
      await reload();
    }
  };

  const isCustom = choice === CUSTOM;
  const id = isCustom ? customId.trim() : choice;
  const canAdd = id !== "" && (!isCustom || baseUrl.trim() !== "") && !busy;

  return (
    <Card padding="md" className={styles.card}>
      <div className={styles.info}>
        <h2 className={styles.heading}>{t("settings.providers.title")}</h2>
        <p className={formStyles.hint}>{t("settings.providers.description")}</p>
      </div>
      {!native ? (
        <p className={formStyles.hint}>{t("settings.providers.nativeOnly")}</p>
      ) : (
        <>
          {loadError ? <p className={formStyles.error}>{loadError}</p> : null}
          {list.length === 0 && !loadError ? (
            <p className={formStyles.hint}>{t("settings.providers.none")}</p>
          ) : (
            <ul className={styles.list}>
              {list.map((p) => (
                <li key={p.id} className={styles.listRow}>
                  <span className={styles.rowMain}>
                    <span className={styles.rowName}>{p.id}</span>
                    {p.isDefault ? (
                      <span className={styles.tag}>
                        {t("settings.providers.default")}
                      </span>
                    ) : null}
                    {!p.enabled ? (
                      <span className={styles.tag}>
                        {t("settings.providers.disabled")}
                      </span>
                    ) : null}
                    {p.enabled && !p.active ? (
                      <span className={styles.tag}>
                        {t("settings.providers.notLoaded")}
                      </span>
                    ) : null}
                  </span>
                  <span className={styles.rowMeta}>
                    {p.model || t("settings.providers.noModel")} ·{" "}
                    {p.hasKey
                      ? t("settings.providers.keyStored")
                      : t("settings.providers.noKey")}
                    {p.models.length > 0
                      ? ` · ${t("settings.providers.modelCount", { count: p.models.length })}`
                      : ""}
                  </span>
                  <span className={styles.rowActions}>
                    <Button
                      size="sm"
                      variant="secondary"
                      disabled={busy}
                      onClick={() =>
                        void run(async () => outcomeOf(await testProvider(p.id)), false)
                      }
                    >
                      {t("settings.providers.test")}
                    </Button>
                    {!p.isDefault ? (
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={busy}
                        onClick={() =>
                          void run(
                            async () => outcomeOf(await setDefaultProvider(p.id)),
                            true
                          )
                        }
                      >
                        {t("settings.providers.makeDefault")}
                      </Button>
                    ) : null}
                  </span>
                </li>
              ))}
            </ul>
          )}

          <div className={styles.addBox}>
            <h3 className={styles.subHeading}>{t("settings.providers.add")}</h3>
            <div className={formStyles.row}>
              <div className={formStyles.field}>
                <label className={formStyles.label} htmlFor="prov-choice">
                  {t("settings.providers.provider")}
                </label>
                <select
                  id="prov-choice"
                  className={formStyles.select}
                  value={choice}
                  onChange={(e) => setChoice(e.target.value)}
                >
                  {catalog.map((p) => (
                    <option key={p} value={p}>
                      {p}
                    </option>
                  ))}
                  <option value={CUSTOM}>{t("settings.providers.custom")}</option>
                </select>
              </div>
              {isCustom ? (
                <>
                  <div className={`${formStyles.field} ${formStyles.grow}`}>
                    <label className={formStyles.label} htmlFor="prov-id">
                      {t("settings.providers.customId")}
                    </label>
                    <input
                      id="prov-id"
                      className={formStyles.input}
                      value={customId}
                      onChange={(e) => setCustomId(e.target.value)}
                    />
                  </div>
                  <div className={`${formStyles.field} ${formStyles.grow}`}>
                    <label className={formStyles.label} htmlFor="prov-url">
                      {t("settings.providers.baseUrl")}
                    </label>
                    <input
                      id="prov-url"
                      className={formStyles.input}
                      value={baseUrl}
                      placeholder="https://"
                      onChange={(e) => setBaseUrl(e.target.value)}
                    />
                  </div>
                </>
              ) : null}
            </div>
            <div className={formStyles.row}>
              <div className={`${formStyles.field} ${formStyles.grow}`}>
                <label className={formStyles.label} htmlFor="prov-key">
                  {t("settings.providers.apiKey")}
                </label>
                <input
                  id="prov-key"
                  className={formStyles.input}
                  type="password"
                  autoComplete="off"
                  value={apiKey}
                  onChange={(e) => setApiKey(e.target.value)}
                />
              </div>
              <div className={`${formStyles.field} ${formStyles.grow}`}>
                <label className={formStyles.label} htmlFor="prov-model">
                  {t("settings.providers.defaultModel")}
                </label>
                <input
                  id="prov-model"
                  className={formStyles.input}
                  value={model}
                  onChange={(e) => setModel(e.target.value)}
                />
              </div>
            </div>
            <p className={formStyles.hint}>{t("settings.providers.keyHint")}</p>
            <div>
              <Button
                variant="primary"
                disabled={!canAdd}
                onClick={() =>
                  void run(async () => {
                    const o = outcomeOf(
                      await addProvider({
                        id,
                        baseUrl: isCustom ? baseUrl.trim() : undefined,
                        model: model.trim() || undefined,
                        apiKey: apiKey || undefined
                      })
                    );
                    if (o.ok) {
                      setApiKey("");
                    }
                    return o;
                  }, true)
                }
              >
                {t("settings.providers.addAndTest")}
              </Button>
            </div>
          </div>
          {outcome ? (
            <pre
              className={outcome.ok ? styles.outputOk : styles.outputBad}
              role="status"
            >
              {outcome.text}
            </pre>
          ) : null}
          {needsRestart ? (
            <div className={styles.restartBox}>
              <p className={formStyles.hint}>
                {conn?.started_by_app
                  ? t("settings.providers.restartHint")
                  : t("settings.providers.restartManual")}
              </p>
              {conn?.started_by_app ? (
                <Button
                  size="sm"
                  variant="secondary"
                  onClick={() => {
                    setNeedsRestart(false);
                    void restartDaemon();
                  }}
                >
                  {t("settings.providers.restart")}
                </Button>
              ) : null}
            </div>
          ) : null}
        </>
      )}
    </Card>
  );
}

// ---------------------------------------------------------------- permissions

function grantLabel(g: GrantTarget): string {
  return `${g.kind}:${g.name}`;
}

function BotPermissions({ bot }: { bot: Bot }) {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const [detail, setDetail] = useState<Bot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const view = botView(bot);

  const load = useCallback(async () => {
    if (!client) {
      return;
    }
    try {
      setDetail(await client.getBot(bot.id));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [client, bot.id]);

  useEffect(() => {
    void load();
  }, [load, bot.config]);

  const revoke = async (target: GrantTarget) => {
    if (!client) {
      return;
    }
    try {
      await client.revokeGrant(bot.id, target);
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const eff = detail?.effective_policy;
  return (
    <div className={styles.botPerm}>
      <div className={styles.botPermHead}>
        <BotAvatar bot={view} size="sm" decorative />
        <span className={styles.rowName}>{view.name}</span>
      </div>
      {error ? <p className={formStyles.error}>{error}</p> : null}
      {eff ? (
        <>
          <ul className={styles.chips}>
            {Object.entries(eff.categories).map(([cat, d]) => (
              <li
                key={cat}
                className={[styles.chip, styles[`chip_${d.decision}`]].join(" ")}
                title={d.via_grant ? t("settings.permissions.viaGrant") : undefined}
              >
                <span>{t(`botForm.category.${cat}`, cat)}</span>
                <span className={styles.chipLevel}>
                  {t(`botForm.level.${d.decision}`)}
                  {d.via_grant ? " ★" : ""}
                </span>
              </li>
            ))}
          </ul>
          {eff.grants.length === 0 ? (
            <p className={formStyles.hint}>{t("settings.permissions.noGrants")}</p>
          ) : (
            <ul className={styles.list}>
              {eff.grants.map((g) => (
                <li key={grantLabel(g.target)} className={styles.listRow}>
                  <span className={styles.rowMain}>
                    <span className={styles.rowName}>
                      {g.target.kind === "category"
                        ? t("settings.permissions.grantCategory", {
                            name: t(`botForm.category.${g.target.name}`, g.target.name)
                          })
                        : t("settings.permissions.grantTool", {
                            name: g.target.name
                          })}
                    </span>
                  </span>
                  <span className={styles.rowMeta}>
                    {new Date(g.granted_at_ms).toLocaleString()}
                  </span>
                  <Button
                    size="sm"
                    variant="danger"
                    onClick={() => void revoke(g.target)}
                  >
                    {t("settings.permissions.revoke")}
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </>
      ) : null}
    </div>
  );
}

const PRESETS = [
  "read-only",
  "approve-destructive",
  "read-and-draft",
  "full"
] as const;
const LEVELS = ["deny", "ask", "allow"] as const;

/** The workspace default policy (`GET` / `PUT /v1/policy`), editable. */
function WorkspacePolicy() {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const [policy, setPolicy] = useState<PolicyDto | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<Outcome | null>(null);

  useEffect(() => {
    if (!client) {
      return;
    }
    client
      .getPolicy()
      .then(setPolicy)
      .catch((e: unknown) =>
        setMessage({ ok: false, text: e instanceof Error ? e.message : String(e) })
      );
  }, [client]);

  const apply = async (input: { preset?: string; categories?: Record<string, string> }) => {
    if (!client) {
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      setPolicy(await client.putPolicy(input));
      setMessage({ ok: true, text: t("settings.permissions.workspace.saved") });
    } catch (e) {
      setMessage({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  };

  if (!policy) {
    return message ? <p className={formStyles.error}>{message.text}</p> : null;
  }
  return (
    <div className={styles.botPerm}>
      <h3 className={styles.subHeading}>
        {t("settings.permissions.workspace.title")}
      </h3>
      <p className={formStyles.hint}>
        {t("settings.permissions.workspace.description")}
      </p>
      <div className={formStyles.field}>
        <label className={formStyles.label} htmlFor="ws-preset">
          {t("settings.permissions.workspace.preset")}
        </label>
        <select
          id="ws-preset"
          className={formStyles.select}
          value={policy.preset ?? "custom"}
          disabled={busy}
          onChange={(e) => void apply({ preset: e.target.value })}
        >
          {PRESETS.map((p) => (
            <option key={p} value={p}>
              {t(`botForm.preset.${p}.name`)}
            </option>
          ))}
          {policy.preset === null ? (
            <option value="custom" disabled>
              {t("botForm.preset.custom.name")}
            </option>
          ) : null}
        </select>
      </div>
      <div className={formStyles.row}>
        {PERMISSION_CATEGORIES.map((cat) => (
          <div key={cat} className={formStyles.field}>
            <label className={formStyles.label} htmlFor={`ws-${cat}`}>
              {t(`botForm.category.${cat}`)}
            </label>
            <select
              id={`ws-${cat}`}
              className={formStyles.select}
              value={policy.categories[cat] ?? "ask"}
              disabled={busy}
              onChange={(e) => void apply({ categories: { [cat]: e.target.value } })}
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
      {message ? (
        <p className={message.ok ? formStyles.hint : formStyles.error} role="status">
          {message.text}
        </p>
      ) : null}
    </div>
  );
}

export function PermissionsSection() {
  const { t } = useTranslation();
  const { bots } = useDaemon();
  return (
    <Card padding="md" className={styles.card}>
      <div className={styles.info}>
        <h2 className={styles.heading}>{t("settings.permissions.title")}</h2>
        <p className={formStyles.hint}>{t("settings.permissions.description")}</p>
      </div>
      <WorkspacePolicy />
      {bots.length === 0 ? (
        <p className={formStyles.hint}>{t("settings.permissions.noBots")}</p>
      ) : (
        bots.map((b) => <BotPermissions key={b.id} bot={b} />)
      )}
    </Card>
  );
}
