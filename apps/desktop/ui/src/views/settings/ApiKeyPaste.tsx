import { Check, KeyRound } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { MOCK_BASE_URL } from "../../api/mockMode";
import { BrandIcon } from "../../components/BrandIcon";
import { useToast } from "../../components/Toast";
import {
  detectKeyProvider,
  keyTargets,
  looksLikeKey,
  resolveKeyTarget,
  scrubSecret
} from "../../lib/apiKey";
import { canManageProviders } from "../../lib/providerConnection";
import { isTauriRuntime } from "../../overlay/events";
import { useDaemon } from "../../state/DaemonContext";
import { notifyProvidersChanged, useProviders } from "../../state/useProviders";
import styles from "./AiPage.module.css";
import { humanLine } from "../../lib/humanError";
import { useBotsHome } from "../../state/useBotsHome";
import { useActiveHost } from "../../state/useActiveHost";
import { HomeHint } from "../../components/HomeHint";
import { useServiceState } from "../../state/ServiceState";
import { ConnectionTestButton } from "../../components/ConnectionTest";

/** Open the collapsed "advanced providers" block (custom OpenAI-compatible servers). */
function openAdvanced() {
  const d = document.getElementById("ai-advanced-providers");
  if (d instanceof HTMLDetailsElement) {
    d.open = true;
    const instant =
      typeof window !== "undefined" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    d.scrollIntoView({ block: "nearest", behavior: instant ? "instant" : "smooth" });
  }
}

/**
 * One paste box for an API key.  The provider is read off the key prefix;
 * when that is ambiguous or unknown the user picks one of the chips.
 * Write-only: the key is held in this component until it is saved through
 * the native provider setup command, then dropped. That command also writes
 * the provider's secret reference and configuration. It is never shown again,
 * logged, or put in a URL.
 */
export function ApiKeyPaste() {
  const { t } = useTranslation();
  const toast = useToast();
  const { client, conn } = useDaemon();
  const serviceBlocked = useServiceState().blocked;
  const mock = client?.baseUrl === MOCK_BASE_URL;
  const providerTarget = { connection: conn, baseUrl: client?.baseUrl };
  const home = useBotsHome();
  const active = useActiveHost();
  const serverName = active.name || t("computerPicker.host");
  const canConfigure = mock || home.target === "host" || (isTauriRuntime() && canManageProviders(providerTarget));
  const { dto, reload } = useProviders();
  const [value, setValue] = useState("");
  const [picked, setPicked] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const targets = useMemo(() => keyTargets(dto?.catalog ?? []), [dto]);
  const detected = detectKeyProvider(value);
  const target = resolveKeyTarget(targets, detected, picked);
  const ready = looksLikeKey(value);
  const hasKey = (id: string) =>
    dto?.configured.some((p) => p.id === id && p.has_key) === true;

  /** Providers with a stored key (or a custom endpoint): each can be tested on the daemon. */
  const testable = (dto?.configured ?? []).filter((p) => p.has_key || p.custom);

  const save = async () => {
    if (!client || !canConfigure || !target || !ready || busy) {
      return;
    }
    const secret = value.trim();
    setBusy(true);
    setError(null);
    try {
      // The bots' home decides where the key goes (the host, or this computer's keychain).
      await home.saveProvider(target.id, { apiKey: secret, keyEnv: target.keyEnv });
      setValue("");
      setPicked(null);
      await reload();
      notifyProvidersChanged();
      toast.show(
        home.target === "host"
          ? t("settingsModal.ai.keys.savedOn", { provider: target.id, name: serverName })
          : t("settingsModal.ai.keys.saved", { provider: target.id }),
        "success"
      );
    } catch (e) {
      const msg = scrubSecret(humanLine(e), secret);
      setError(msg);
      toast.show(msg, "danger");
    } finally {
      setBusy(false);
    }
  };

  const lead = target ? (
    <BrandIcon name={target.id} bare size={20} />
  ) : (
    <KeyRound aria-hidden="true" />
  );

  return (
    <section className={styles.sec} data-testid="api-key-paste" aria-labelledby="ai-key-h">
      <div className={styles.head}>
        <h3 id="ai-key-h">{t("settingsModal.ai.keys.title")}</h3>
        <HomeHint />
      </div>
      <form
        className={`${styles.card} ${styles.paste}`}
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <span className={styles.lead}>{lead}</span>
        <input
          className={styles.keyInput}
          type="password"
          name="praxi-api-key"
          autoComplete="off"
          autoCorrect="off"
          autoCapitalize="off"
          spellCheck={false}
          aria-label={t("settingsModal.ai.keys.pasteLabel")}
          placeholder={t("settingsModal.ai.keys.placeholder")}
          value={value}
          disabled={!client || !canConfigure}
          onChange={(e) => {
            setValue(e.target.value);
            if (e.target.value.trim() === "") {
              setPicked(null);
            }
            setError(null);
          }}
        />
        {ready ? (
          <button
            type="submit"
            className={`${styles.pill} ${styles.pillPri}`}
            disabled={!target || busy || !client || !canConfigure || serviceBlocked}
          >
            {target
              ? t("settingsModal.ai.keys.addTo", { provider: target.id })
              : t("settingsModal.ai.keys.add")}
          </button>
        ) : null}
      </form>
      {!canConfigure ? <p className={styles.sub}>{t("settings.providers.localDaemonOnly")}</p> : null}

      <div className={styles.provs} role="group" aria-label={t("settingsModal.ai.keys.providers")}>
        {targets.map((p) => {
          const on = target?.id === p.id;
          const stored = hasKey(p.id);
          return (
            <button
              key={p.id}
              type="button"
              aria-pressed={on}
              data-testid={`key-provider-${p.id}`}
              className={`${styles.chip} ${on ? styles.chipOn : ""}`}
              onClick={() => setPicked((cur) => (cur === p.id ? null : p.id))}
            >
              <BrandIcon name={p.id} bare size={14} />
              {p.id}
              {stored ? (
                <Check
                  size={12}
                  className={styles.chipDone}
                  aria-label={t("settingsModal.ai.keys.stored")}
                />
              ) : null}
            </button>
          );
        })}
        <button type="button" className={styles.chip} onClick={openAdvanced}>
          {t("settingsModal.ai.keys.compatible")}
        </button>
      </div>

      {testable.length > 0 && !active.isHost ? (
        <div className={styles.provs} role="group" aria-label={t("connectionTest.savedKeys")} data-testid="key-tests">
          {testable.map((p) => (
            <ConnectionTestButton
              key={p.id}
              testId={`key-test-${p.id}`}
              label={t("connectionTest.testProvider", { provider: p.id })}
              run={() => (client ? client.testConnection("provider", { id: p.id }) : Promise.reject(new Error("daemon not connected")))}
              disabled={!client}
            />
          ))}
        </div>
      ) : null}

      {ready && !target ? (
        <p className={styles.hint} data-testid="key-unknown">
          {t("settingsModal.ai.keys.unknown")}
        </p>
      ) : (
        <p className={styles.hint} data-testid="key-where">
          {home.target === "host"
            ? t("settingsModal.ai.keys.hostDescription", { name: serverName })
            : t("settingsModal.ai.keys.description")}
        </p>
      )}
      {error ? (
        <p className={styles.error} role="alert">
          {error}
        </p>
      ) : null}
    </section>
  );
}
