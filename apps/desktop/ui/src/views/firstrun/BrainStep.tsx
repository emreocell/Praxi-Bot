import { Check, ClipboardPaste, ExternalLink, Eye, EyeOff, LoaderCircle } from "lucide-react";
import { type Dispatch, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { isMockMode, MOCK_BASE_URL } from "../../api/mockMode";
import type { CliEngineDto, ProvidersDto } from "../../api/types";
import { BrandIcon } from "../../components/BrandIcon";
import { BrandMark } from "../../components/BrandMark";
import { LocalInstallCard } from "../../components/LocalInstallCard";
import { LocalModelsAdvice } from "../../components/LocalModelsAdvice";
import { ModelPicker } from "../../components/ModelPicker";
import { detectKeyProvider, keyTargets, looksLikeKey, saveProviderKey, scrubSecret } from "../../lib/apiKey";
import { brandOf } from "../../lib/cliCards";
import { openExternal } from "../../lib/externalLink";
import {
  API_GROUPS,
  type ApiGroup,
  autoModelFor,
  classifyConnectError,
  type ConnectError,
  type FirstRunAction,
  type FirstRunState,
  keyShapeProblem,
  modelCount,
  providerIdFor,
  readyEngines,
  refLabel
} from "../../lib/firstRun";
import { productBrandKey, providerBrand } from "../../lib/modelBrand";
import { humanProviderTestError } from "../../lib/humanError";
import { isCliDefault, modelGroups } from "../../lib/modelPicker";
import { canManageProviders, PROVIDERS_LOCAL_ONLY } from "../../lib/providerConnection";
import { type LocalServer, SUGGESTED_LOCAL_MODEL } from "../../lib/localServers";
import { addProvider, detectLocalServers, setDefaultProvider, testProvider } from "../../lib/providers";
import { isTauriRuntime } from "../../overlay/events";
import { useDaemon } from "../../state/DaemonContext";
import { notifyProvidersChanged } from "../../state/useProviders";
import styles from "./FirstRun.module.css";

/** Where each provider hands out keys; opened in the system browser. */
const KEY_PAGES: Record<ApiGroup, string> = {
  gemini: "https://aistudio.google.com/apikey",
  anthropic: "https://console.anthropic.com/settings/keys",
  openai: "https://platform.openai.com/api-keys",
  ollama: ""
};
const COMPAT_PAGES: Record<string, string> = {
  openai: KEY_PAGES.openai,
  openrouter: "https://openrouter.ai/keys",
  deepseek: "https://platform.deepseek.com/api_keys"
};
const COMPAT_IDS = ["openai", "openrouter", "deepseek"] as const;

export function BrainStep({
  state,
  dispatch
}: {
  state: FirstRunState;
  dispatch: Dispatch<FirstRunAction>;
}) {
  const { t } = useTranslation();
  const { client, conn } = useDaemon();
  const mock = client?.baseUrl === MOCK_BASE_URL;
  const providerTarget = { connection: conn, baseUrl: client?.baseUrl };
  const [engines, setEngines] = useState<CliEngineDto[] | null>(null);
  const [dto, setDto] = useState<ProvidersDto | null>(null);
  const [key, setKey] = useState("");
  const [shown, setShown] = useState(false);
  const [compat, setCompat] = useState<string | null>(null);
  const [note, setNote] = useState("");
  const [autoRef, setAutoRef] = useState("");
  /** Name used in error sentences: the CLI or provider the user just tried. */
  const [subject, setSubject] = useState("");
  const keyInput = useRef<HTMLInputElement | null>(null);
  /** Local model servers answering on loopback (null while looking). */
  const [local, setLocal] = useState<LocalServer[] | null>(null);
  const canProbeLocal = !mock && isTauriRuntime() && canManageProviders(providerTarget);

  useEffect(() => {
    if (!canProbeLocal) {
      setLocal([]);
      return;
    }
    let alive = true;
    void detectLocalServers(providerTarget).then((found) => alive && setLocal(found));
    return () => {
      alive = false;
    };
    // Probe once per daemon connection.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client]);

  useEffect(() => {
    if (!client) {
      setEngines([]);
      return;
    }
    let alive = true;
    client.cliEngines(false).then(
      (list) => alive && setEngines(list),
      () => alive && setEngines([])
    );
    client.providers().then(
      (list) => alive && setDto(list),
      () => {}
    );
    return () => {
      alive = false;
    };
  }, [client]);

  useEffect(() => {
    if (state.phase === "key") {
      keyInput.current?.focus();
    }
  }, [state.phase, state.choice]);

  const ready = useMemo(() => readyEngines(engines ?? []), [engines]);
  const group = state.choice?.kind === "api" ? state.choice.group : null;
  const groupName = group ? t(`firstRun.brain.providers.${group}.name`) : "";
  const picked = (id: string): boolean => state.choice?.kind === "cli" && state.choice.id === id;

  const fresh = async (): Promise<ProvidersDto | null> => {
    if (!client) {
      return null;
    }
    const next = await client.providers();
    setDto(next);
    notifyProvidersChanged();
    return next;
  };

  const fail = (raw: unknown, forGroup: ApiGroup | null, secret = "") => {
    const text = raw instanceof Error ? raw.message : String(raw);
    const kind: ConnectError = text === PROVIDERS_LOCAL_ONLY ? "unavailable" : classifyConnectError(scrubSecret(text, secret), forGroup);
    dispatch({ type: "failed", error: kind });
  };

  const connectCli = async (engine: CliEngineDto) => {
    if (!client) {
      return;
    }
    const id = engine.detected.id;
    setSubject(engine.detected.display_name);
    dispatch({ type: "connecting" });
    try {
      if (!engine.enabled) {
        await client.enableCliProvider(id);
        await client.reloadProviders().catch(() => {});
      }
      const next = await fresh();
      const auto = autoModelFor(next, `cli:${id}`);
      const ref = auto?.ref ?? `cli:${id}/default`;
      setAutoRef(ref);
      setNote(t("firstRun.brain.cliReady", { name: engine.detected.display_name }));
      dispatch({ type: "pickCli", id, model: ref });
    } catch (e) {
      fail(e, null);
    }
  };

  /** Save the key (or just switch Ollama on), read the provider's models back, pick one. */
  const connect = async (g: ApiGroup | null, secret: string, provider: string, server?: LocalServer) => {
    if (!client || (g !== "ollama" && !server && !looksLikeKey(secret))) {
      return;
    }
    dispatch({ type: "connecting" });
    try {
      const current = dto ?? (await fresh());
      const target = keyTargets(current?.catalog ?? []).find((k) => k.id === provider);
      if (mock) {
        if (target) {
          await client.setSecret(target.keyEnv, secret.trim());
        }
      } else if (!canManageProviders(providerTarget) || !isTauriRuntime()) {
        throw new Error(PROVIDERS_LOCAL_ONLY);
      } else {
        if (g === "ollama" || server) {
          const res = await addProvider(
            server && server.id !== "lmstudio" ? { id: server.id, baseUrl: server.baseUrl } : { id: provider },
            providerTarget
          );
          if (res.code !== 0) {
            throw new Error((res.stderr || res.stdout).trim());
          }
          await client.reloadProviders();
        } else {
          await saveProviderKey(
            target ?? { id: provider, keyEnv: "" },
            secret.trim(),
            client,
            (input) => addProvider(input, providerTarget),
            conn
          );
        }
        const probe = await testProvider(provider, providerTarget);
        if (probe.code !== 0) {
          throw new Error((probe.stderr || probe.stdout).trim());
        }
      }
      const next = await fresh();
      const auto = autoModelFor(next, provider);
      if (!auto) {
        dispatch({ type: "failed", error: g === "ollama" ? "ollamaNoModels" : "noModels" });
        return;
      }
      const count = modelCount(next, provider);
      setNote(count > 0 ? t(count === 1 ? "firstRun.brain.connectedModel" : "firstRun.brain.connectedModels", { count }) : t("firstRun.brain.connected"));
      setAutoRef(auto.ref);
      setKey("");
      if (!next?.default_provider) {
        await setDefaultProvider(provider, providerTarget, auto.ref.slice(provider.length + 1)).catch(() => {});
      }
      dispatch({ type: "connected", model: auto.ref });
    } catch (e) {
      fail(e, g, secret);
    }
  };

  /** One click on a detected LM Studio / llama.cpp server. */
  const connectLocal = (server: LocalServer) => {
    setKey("");
    setSubject(t(`firstRun.brain.local.${server.id}`));
    void connect(null, "", server.id, server);
  };

  const pickApi = (g: ApiGroup) => {
    setKey("");
    setShown(false);
    setCompat(null);
    setSubject(t(`firstRun.brain.providers.${g}.name`));
    dispatch({ type: "pickApi", group: g });
    if (g === "ollama") {
      void connect("ollama", "", "ollama");
    }
  };

  const providerFor = (g: ApiGroup, secret: string) =>
    g === "openai" && compat ? compat : providerIdFor(g, detectKeyProvider(secret));

  const submitKey = (secret = key) => {
    if (group) {
      void connect(group, secret, providerFor(group, secret));
    }
  };

  const paste = async () => {
    try {
      const text = (await navigator.clipboard.readText()).trim();
      if (text) {
        setKey(text);
        submitKey(text);
      }
    } catch {
      keyInput.current?.focus();
    }
  };

  // Design review (mock mode only): `?frpick=cli|gemini|gemini-bad` opens the screen in that state.
  const reviewed = useRef(false);
  useEffect(() => {
    const pick = isMockMode() ? new URLSearchParams(window.location.search).get("frpick") : null;
    if (!pick || reviewed.current || engines === null || !dto) {
      return;
    }
    reviewed.current = true;
    if (pick === "cli" && ready[0]) {
      void connectCli(ready[0]);
    } else if (pick === "gemini" || pick === "gemini-bad") {
      setSubject(t("firstRun.brain.providers.gemini.name"));
      dispatch({ type: "pickApi", group: "gemini" });
      if (pick === "gemini-bad") {
        setKey("AIza-example-not-a-real-key");
        dispatch({ type: "failed", error: "badKey" });
      }
    }
    // Runs once, when the lists have loaded.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [engines, dto]);

  const groups = useMemo(() => modelGroups(dto, state.model), [dto, state.model]);
  const chosen = groups.flatMap((g) => g.models).find((m) => m.ref === state.model);
  const chosenName = chosen && !isCliDefault(chosen) ? chosen.displayName : refLabel(state.model);
  const chosenBrand = productBrandKey(
    state.model.slice(0, Math.max(state.model.indexOf("/"), 0)),
    state.model.slice(state.model.indexOf("/") + 1)
  );

  const busy = state.phase === "connecting";
  const connected = state.phase === "connected";
  const errorName = subject || groupName;
  const providerError = state.error === "provider_endpoint" || state.error === "provider_model"
    ? humanProviderTestError(state.error)
    : null;
  const error = providerError
    ? `${providerError.title}. ${providerError.hint}`
    : state.error ? t(`firstRun.errors.${state.error}`, { name: errorName, model: SUGGESTED_LOCAL_MODEL }) : null;
  const ollamaFound = local?.some((s) => s.id === "ollama") === true;
  const otherLocal = (local ?? []).filter((s) => s.id !== "ollama");
  const keyOpen = group !== null && group !== "ollama" && !connected;
  const keyProblem = keyOpen ? keyShapeProblem(group, key) : null;

  return (
    <div className={styles.stack}>
      {engines === null ? (
        <p className={styles.looking} role="status">
          <LoaderCircle className={styles.spin} aria-hidden="true" />
          {t("firstRun.brain.looking")}
        </p>
      ) : ready.length > 0 && !connected ? (
        <section className={styles.block} aria-labelledby="fr-found">
          <h2 id="fr-found" className={styles.eyebrow}>{t("firstRun.brain.foundTitle")}</h2>
          <div className={styles.cards} role="group" aria-label={t("firstRun.brain.foundTitle")}>
            {ready.map((e) => {
              const d = e.detected;
              const on = picked(d.id);
              return (
                <button
                  key={d.id}
                  type="button"
                  className={`${styles.card} ${styles.cliCard} ${on ? styles.cardOn : ""}`}
                  aria-pressed={on}
                  disabled={busy}
                  data-testid={`fr-cli-${d.id}`}
                  onClick={() => void connectCli(e)}
                >
                  <BrandIcon name={brandOf(d.id)} bare size={26} />
                  <span className={styles.cardText}>
                    <span className={styles.cardTitle}>{t("firstRun.brain.useCli", { name: d.display_name })}</span>
                    <span className={styles.cardSub}>
                      {d.account_label
                        ? t("firstRun.brain.cliSubAccount", { account: d.account_label })
                        : t("firstRun.brain.cliSub")}
                    </span>
                  </span>
                  {on ? <Check className={styles.tick} aria-hidden="true" /> : null}
                </button>
              );
            })}
          </div>
        </section>
      ) : null}

      {engines !== null && !connected ? (
        <section className={styles.block} aria-labelledby="fr-easy">
          <h2 id="fr-easy" className={styles.eyebrow}>{t("firstRun.brain.easyTitle")}</h2>
          <button
            type="button"
            className={`${styles.card} ${styles.easy} ${group === "gemini" ? styles.cardOn : ""}`}
            aria-pressed={group === "gemini"}
            disabled={busy}
            data-testid="fr-api-gemini"
            onClick={() => pickApi("gemini")}
          >
            <BrandIcon name="gemini" bare size={26} />
            <span className={styles.cardText}>
              <span className={styles.cardTitle}>
                {t("firstRun.brain.freeTitle")}
                <span className={styles.badge}>{t("firstRun.brain.providers.gemini.badge")}</span>
              </span>
              <span className={styles.cardSub}>{t("firstRun.brain.freeSub")}</span>
            </span>
          </button>
          {group === "gemini" && keyOpen ? (
            <ol className={styles.steps} data-testid="fr-gemini-steps">
              {(["one", "two", "three"] as const).map((n) => (
                <li key={n}>{t(`firstRun.brain.freeSteps.${n}`)}</li>
              ))}
            </ol>
          ) : null}
        </section>
      ) : null}

      {engines !== null && !connected ? (
        <section className={styles.block} aria-labelledby="fr-api">
          <h2 id="fr-api" className={styles.eyebrow}>
            {ready.length > 0 ? t("firstRun.brain.apiTitleAfter") : t("firstRun.brain.apiTitle")}
          </h2>
          <div className={styles.tiles} role="group" aria-label={t("firstRun.brain.apiTitle")}>
            {API_GROUPS.filter((g) => g !== "gemini").map((g) => {
              const on = group === g;
              const badge = g === "ollama" && ollamaFound
                ? t("firstRun.brain.providers.ollama.found")
                : t(`firstRun.brain.providers.${g}.badge`);
              return (
                <button
                  key={g}
                  type="button"
                  className={`${styles.card} ${styles.tile} ${on ? styles.cardOn : ""}`}
                  aria-pressed={on}
                  disabled={busy}
                  data-testid={`fr-api-${g}`}
                  onClick={() => pickApi(g)}
                >
                  <span className={styles.tileHead}>
                    <BrandIcon name={g === "openai" ? "openai" : g} bare size={20} />
                    {badge ? <span className={styles.badge}>{badge}</span> : null}
                  </span>
                  <span className={styles.cardTitle}>{t(`firstRun.brain.providers.${g}.name`)}</span>
                  <span className={styles.cardSub}>{t(`firstRun.brain.providers.${g}.sub`)}</span>
                </button>
              );
            })}
          </div>
        </section>
      ) : null}

      {group === "ollama" && !connected ? (
        <>
          <LocalInstallCard />
          <LocalModelsAdvice />
        </>
      ) : null}

      {otherLocal.length > 0 && !connected ? (
        <section className={styles.block} aria-labelledby="fr-local">
          <h2 id="fr-local" className={styles.eyebrow}>{t("firstRun.brain.localTitle")}</h2>
          <div className={styles.cards} role="group" aria-label={t("firstRun.brain.localTitle")}>
            {otherLocal.map((s) => (
              <button
                key={s.id}
                type="button"
                className={`${styles.card} ${styles.cliCard}`}
                disabled={busy}
                data-testid={`fr-local-${s.id}`}
                onClick={() => connectLocal(s)}
              >
                <span className={styles.cardText}>
                  <span className={styles.cardTitle}>{t("firstRun.brain.useCli", { name: t(`firstRun.brain.local.${s.id}`) })}</span>
                  <span className={styles.cardSub}>{t(s.models.length === 1 ? "firstRun.brain.localSubOne" : "firstRun.brain.localSubMany", { count: s.models.length })}</span>
                  <span className={styles.cardSub} data-testid={`fr-local-url-${s.id}`}>{s.baseUrl}</span>
                </span>
              </button>
            ))}
          </div>
        </section>
      ) : null}

      {keyOpen && group ? (
        <section className={styles.panel} aria-label={t("firstRun.brain.keyLabel", { name: groupName })}>
          <div className={styles.panelHead}>
            <label className={styles.panelLabel} htmlFor="fr-key">
              {t("firstRun.brain.keyLabel", { name: groupName })}
            </label>
            {group === "openai" ? (
              <span className={styles.compat} role="group" aria-label={groupName}>
                {COMPAT_IDS.map((id) => {
                  const active = (compat ?? providerFor(group, key)) === id;
                  return (
                    <button
                      key={id}
                      type="button"
                      className={`${styles.chip} ${active ? styles.chipOn : ""}`}
                      aria-pressed={active}
                      onClick={() => setCompat(id)}
                    >
                      {providerBrand(id).label}
                    </button>
                  );
                })}
              </span>
            ) : null}
            <Button
              size="sm"
              variant="ghost"
              data-testid="fr-get-key"
              onClick={() => openExternal(group === "openai" ? COMPAT_PAGES[providerFor(group, key)] ?? KEY_PAGES.openai : KEY_PAGES[group])}
            >
              {group === "gemini" ? t("firstRun.brain.getFreeKey") : t("firstRun.brain.getKey")}
              <ExternalLink aria-hidden="true" />
            </Button>
          </div>
          <div className={`${styles.keyRow} ${error ? styles.keyRowBad : ""}`}>
            <input
              id="fr-key"
              ref={keyInput}
              className={styles.keyInput}
              type={shown ? "text" : "password"}
              name="praxi-first-run-key"
              autoComplete="off"
              autoCorrect="off"
              autoCapitalize="off"
              spellCheck={false}
              placeholder={t("firstRun.brain.keyPlaceholder")}
              value={key}
              disabled={busy}
              aria-invalid={error ? true : undefined}
              aria-describedby="fr-key-status"
              data-testid="fr-key"
              onChange={(e) => setKey(e.target.value)}
              onPaste={(e) => {
                const text = e.clipboardData.getData("text").trim();
                if (text) {
                  e.preventDefault();
                  setKey(text);
                  submitKey(text);
                }
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  submitKey();
                }
              }}
            />
            <button
              type="button"
              className={styles.iconBtn}
              aria-label={shown ? t("firstRun.brain.hide") : t("firstRun.brain.show")} title={shown ? t("firstRun.brain.hide") : t("firstRun.brain.show")}
              aria-pressed={shown}
              onClick={() => setShown((v) => !v)}
            >
              {shown ? <EyeOff aria-hidden="true" /> : <Eye aria-hidden="true" />}
            </button>
            <Button size="sm" variant="secondary" disabled={busy} onClick={() => void paste()} data-testid="fr-paste">
              <ClipboardPaste aria-hidden="true" />
              {t("firstRun.brain.paste")}
            </Button>
            <Button
              size="sm"
              variant="primary"
              disabled={busy || !looksLikeKey(key)}
              onClick={() => submitKey()}
              data-testid="fr-connect"
            >
              {t("firstRun.brain.connect")}
            </Button>
          </div>
          <p id="fr-key-status" className={error ? styles.statusBad : styles.status} role={error ? "alert" : "status"}>
            {busy ? (
              <>
                <LoaderCircle className={styles.spin} aria-hidden="true" />
                {t("firstRun.brain.checking")}
              </>
            ) : (
              error ?? (keyProblem ? t(`firstRun.brain.keyProblem.${keyProblem}`) : t("firstRun.brain.keyHint"))
            )}
          </p>
        </section>
      ) : null}

      {(group === "ollama" || (!group && !state.choice)) && busy ? (
        <p className={styles.status} role="status">
          <LoaderCircle className={styles.spin} aria-hidden="true" />
          {t("firstRun.brain.checking")}
        </p>
      ) : null}

      {connected ? (
        <section className={`${styles.panel} ${styles.done}`} data-testid="fr-connected" aria-live="polite">
          <p className={styles.okLine}>
            <Check aria-hidden="true" />
            {note}
          </p>
          <div className={styles.chipRow}>
            <span className={styles.modelLabel}>{t("firstRun.brain.model")}</span>
            <span className={styles.modelChip} data-testid="fr-model">
              <BrandMark name={chosenBrand} size={16} />
              {chosenName}
              {state.model === autoRef ? <span className={styles.badge}>{t("firstRun.brain.recommended")}</span> : null}
            </span>
            <ModelPicker
              groups={groups}
              value={state.model}
              onPick={(ref) => dispatch({ type: "setModel", model: ref })}
              reviewOpen={false}
              placement="bottom-start"
              testId="fr-change-model"
              changeLink={t("firstRun.brain.change")}
            />
            <button type="button" className={styles.linkBtn} onClick={() => dispatch({ type: "changeBrain" })}>
              {t("firstRun.brain.another")}
            </button>
          </div>
        </section>
      ) : null}

      {!connected && !busy ? (
        <p className={styles.later} data-testid="fr-later-note">
          {t("firstRun.brain.laterNote")}
        </p>
      ) : null}

      {!keyOpen && error ? (
        <p className={styles.statusBad} role="alert" data-testid="fr-error">
          {error}
        </p>
      ) : null}
    </div>
  );
}
