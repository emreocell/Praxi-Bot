import { Info, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { DetectedProviderDto } from "../../api/types";
import { BrandIcon } from "../../components/BrandIcon";
import { Switch } from "../../components/SettingsUi";
import { useToast } from "../../components/Toast";
import {
  configuredCliIds,
  detectedRowAction,
  signInCommand
} from "../../lib/detectedAccounts";
import { openExternal } from "../../lib/externalLink";
import { useDaemon } from "../../state/DaemonContext";
import { notifyProvidersChanged, useProviders } from "../../state/useProviders";
import styles from "./AiPage.module.css";

/** Vendor pages for "Install" (opened in the browser; the app downloads nothing). */
const INSTALL_URLS: Readonly<Record<string, string>> = {
  claude: "https://docs.anthropic.com/en/docs/claude-code/setup",
  codex: "https://github.com/openai/codex"
};

function brandOf(row: DetectedProviderDto): string {
  return row.id === "chatgpt-plan" ? "codex" : row.id;
}

/**
 * "Bu bilgisayarda bulunanlar": coding CLIs the daemon found
 * (`GET /v1/providers/detected`); runnable ones are turned on with one switch
 * (`POST /v1/providers/cli/enable`).  The daemon has no "disable" route, so a
 * switch that is on stays on here.
 */
export function DetectedAccountsBlock() {
  const { t } = useTranslation();
  const toast = useToast();
  const { client } = useDaemon();
  const { dto, reload: reloadProviders } = useProviders();
  const [rows, setRows] = useState<DetectedProviderDto[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [enabling, setEnabling] = useState<string | null>(null);
  const [enableError, setEnableError] = useState<string | null>(null);
  const [showSignIn, setShowSignIn] = useState<string | null>(null);

  const enabledCli = useMemo(() => configuredCliIds(dto), [dto]);

  const loadDetected = useCallback(
    async (refresh: boolean) => {
      if (!client) {
        setRows([]);
        setLoadError(null);
        return;
      }
      setLoading(true);
      setLoadError(null);
      try {
        setRows(await client.detectedProviders(refresh));
      } catch (e) {
        setLoadError(
          e instanceof Error && e.message
            ? e.message
            : t("settingsModal.ai.detected.detectError")
        );
      } finally {
        setLoading(false);
      }
    },
    [client, t]
  );

  useEffect(() => {
    void loadDetected(false);
  }, [loadDetected]);

  const onEnable = async (row: DetectedProviderDto) => {
    if (!client) {
      return;
    }
    setEnabling(row.id);
    setEnableError(null);
    try {
      await client.enableCliProvider(row.id);
      await reloadProviders();
      notifyProvidersChanged();
      toast.show(
        t("settingsModal.ai.detected.enabledToast", { name: row.display_name }),
        "success"
      );
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setEnableError(msg);
      toast.show(msg, "danger");
    } finally {
      setEnabling(null);
    }
  };

  const subtitle = (row: DetectedProviderDto, kind: string) => {
    const meta = [row.account_label, row.installed ? row.version : null]
      .filter(Boolean)
      .join(" · ");
    switch (kind) {
      case "enabled":
        return [meta, t("settingsModal.ai.detected.ready")].filter(Boolean).join(" · ");
      case "enable":
        return [meta, t("settingsModal.ai.detected.authSignedIn")].filter(Boolean).join(" · ");
      case "sign_in_hint":
        return t("settingsModal.ai.detected.foundSignInNeeded");
      case "not_installed":
        return t("settingsModal.ai.detected.notInstalled");
      case "codex_unconfinable":
        return t("settingsModal.ai.detected.codexUnconfinable");
      default:
        return t("settingsModal.ai.detected.notRunnable");
    }
  };

  return (
    <section className={styles.sec} data-testid="detected-accounts-block" aria-labelledby="ai-detected-h">
      <div className={styles.head}>
        <h3 id="ai-detected-h">{t("settingsModal.ai.detected.title")}</h3>
        <span className={styles.grow} />
        <button
          type="button"
          className={styles.textBtn}
          disabled={!client || loading}
          onClick={() => void loadDetected(true)}
        >
          <RefreshCw size={14} aria-hidden="true" className={loading ? styles.spin : undefined} />
          {t("settingsModal.ai.detected.refresh")}
        </button>
      </div>

      <div className={styles.card}>
        {!client ? (
          <p className={styles.errorRow}>{t("settingsModal.ai.detected.offline")}</p>
        ) : loadError ? (
          <div className={styles.errorRow} role="alert">
            <span>
              {t("settingsModal.ai.detected.detectError")} ({loadError})
            </span>
            <button type="button" className={styles.pill} disabled={loading} onClick={() => void loadDetected(true)}>
              {t("settingsModal.ai.detected.retry")}
            </button>
          </div>
        ) : (
          <ul className={styles.list}>
            {rows.map((row) => {
              const action = detectedRowAction(row, enabledCli);
              const installUrl = INSTALL_URLS[row.id];
              return (
                <li key={row.id} className={styles.item} data-testid={`detected-${row.id}`}>
                  <BrandIcon name={brandOf(row)} bare size={22} />
                  <div className={styles.text}>
                    <span>{row.display_name}</span>
                    <span className={`${styles.sub} ${action.kind === "codex_unconfinable" ? styles.subWarn : ""}`}>
                      {subtitle(row, action.kind)}
                    </span>
                    {showSignIn === row.id ? (
                      <span className={styles.sub}>
                        {t("settingsModal.ai.detected.signInIn")}{" "}
                        <code className={styles.code}>{signInCommand(row.id)}</code>
                      </span>
                    ) : null}
                  </div>
                  <div className={styles.ctl}>
                    {action.kind === "enable" || action.kind === "enabled" ? (
                      <Switch
                        checked={action.kind === "enabled"}
                        disabled={action.kind === "enabled" || enabling === row.id}
                        label={t("settingsModal.ai.detected.toggle", { name: row.display_name })}
                        onChange={() => void onEnable(row)}
                      />
                    ) : action.kind === "sign_in_hint" ? (
                      <button
                        type="button"
                        className={styles.pill}
                        aria-expanded={showSignIn === row.id}
                        onClick={() => setShowSignIn((s) => (s === row.id ? null : row.id))}
                      >
                        {t("settingsModal.ai.detected.signIn")}
                      </button>
                    ) : action.kind === "not_installed" && installUrl ? (
                      <button type="button" className={styles.pill} onClick={() => void openExternal(installUrl)}>
                        {t("settingsModal.ai.detected.install")}
                      </button>
                    ) : null}
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </div>

      <p className={styles.hint}>
        <Info size={12} aria-hidden="true" /> {t("settingsModal.ai.detected.description")}{" "}
        {t("settingsModal.ai.detected.termsNotice")}
      </p>
      {enableError ? (
        <p className={styles.error} role="alert">
          {enableError}
        </p>
      ) : null}
    </section>
  );
}
