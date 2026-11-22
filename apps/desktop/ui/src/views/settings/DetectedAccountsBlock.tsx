import { ChevronDown, Info, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { CliEngineDto, CliInstallApproval, ProvidersDto } from "../../api/types";
import { ApprovalCard, useCountdown } from "../../components/ChatCards";
import { BrandIcon } from "../../components/BrandIcon";
import { Switch } from "../../components/SettingsUi";
import { useToast } from "../../components/Toast";
import { configuredCliIds } from "../../lib/detectedAccounts";
import {
  brandOf,
  cliStatus,
  displayPath,
  hasSwitch,
  needsProviderReload,
  shortVersion,
  splitEngines,
  type CliStatus
} from "../../lib/cliCards";
import { openExternal } from "../../lib/externalLink";
import { useDaemon } from "../../state/DaemonContext";
import { notifyProvidersChanged, useProviders } from "../../state/useProviders";
import styles from "./AiPage.module.css";

const STATUS_KEY: Record<CliStatus, string> = {
  ready: "ready",
  sign_in: "signIn",
  outdated: "outdated",
  not_installed: "notInstalled",
  unsupported: "unsupported"
};

/**
 * "Found on this computer": coding CLIs the daemon found (`GET /v1/providers/cli`).
 * One compact row per engine (icon, name, one status chip, account and version, switch);
 * path, install command, sign-in and docs sit behind "Details". The daemon switches a
 * detected, signed-in engine on the first time it sees it; the switch writes the owner's
 * choice back (`POST /v1/providers/cli/enable|disable`), and an engine switched off stays off.
 * Engines that are not installed are one collapsed list at the bottom.
 */
export function DetectedAccountsBlock({ local = false }: { local?: boolean } = {}) {
  const { t } = useTranslation();
  const toast = useToast();
  const { client: activeClient, localApi } = useDaemon();
  // `local`: this computer's own CLIs while the bots' home is a server (the active client is the server's).
  const client = local ? localApi : activeClient;
  const shared = useProviders();
  const [localDto, setLocalDto] = useState<ProvidersDto | null>(null);
  const reloadLocal = useCallback(async () => {
    setLocalDto(client ? await client.providers().catch(() => null) : null);
  }, [client]);
  useEffect(() => {
    if (local) {
      void reloadLocal();
    }
  }, [local, reloadLocal]);
  const dto = local ? localDto : shared.dto;
  const reloadProviders = local ? reloadLocal : shared.reload;
  const [engines, setEngines] = useState<CliEngineDto[]>([]);
  const [approval, setApproval] = useState<CliInstallApproval | null>(null);
  const approvalRemaining = useCountdown(approval?.expires_at_ms);
  const [installing, setInstalling] = useState(false);
  /** Installer output stays out of the page; it is only copied on request. */
  const installOutput = useRef("");
  const [problem, setProblem] = useState<{ message: string; details: string } | null>(null);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [enabling, setEnabling] = useState<string | null>(null);
  const [openId, setOpenId] = useState<string | null>(null);

  const enabledCli = useMemo(() => configuredCliIds(dto), [dto]);

  const loadDetected = useCallback(
    async (refresh: boolean) => {
      if (!client) {
        setEngines([]);
        setLoadError(null);
        return;
      }
      setLoading(true);
      setLoadError(null);
      try {
        setEngines(await client.cliEngines(refresh));
      } catch (e) {
        setLoadError(e instanceof Error && e.message ? e.message : "detect_failed");
      } finally {
        setLoading(false);
      }
    },
    [client]
  );

  useEffect(() => {
    void loadDetected(false);
  }, [loadDetected]);

  // The daemon switches a detected, signed-in engine on the first time it sees it;
  // pick that up in the provider list once.
  useEffect(() => {
    if (needsProviderReload(engines, enabledCli)) {
      void reloadProviders();
      notifyProvidersChanged();
    }
    // Only a fresh engine list can change the answer.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [engines]);

  const fail = (error: unknown) => {
    setProblem({
      message: t("settingsModal.ai.detected.problem"),
      details: error instanceof Error ? error.message : String(error)
    });
  };

  const previewInstall = async (id: string) => {
    if (!client) { return; }
    setProblem(null);
    installOutput.current = "";
    try { setApproval(await client.previewCliInstall(id)); }
    catch (error) { fail(error); }
  };

  const decideInstall = async (decision: "approve" | "deny") => {
    if (!client || !approval) { return; }
    setInstalling(true);
    try {
      await client.decideCliInstall(approval.approval_id, decision, (text) => {
        installOutput.current = (installOutput.current + text).slice(-65536);
      });
      setApproval(null);
      if (decision === "approve") { toast.show(t("settingsModal.ai.detected.installDone"), "success"); }
    } catch (error) {
      setApproval(null);
      const code = error instanceof Error ? error.message : "install_failed";
      setProblem({
        message: t(`settingsModal.ai.detected.installErrors.${code}`, {
          defaultValue: t("settingsModal.ai.detected.installErrors.install_failed")
        }),
        details: installOutput.current || code
      });
    } finally { setInstalling(false); await loadDetected(true); }
  };

  const copyText = async (text: string, done: string) => {
    try { await navigator.clipboard.writeText(text); toast.show(done, "success"); }
    catch (error) { fail(error); }
  };

  const onToggle = async (engine: CliEngineDto) => {
    if (!client) {
      return;
    }
    const row = engine.detected;
    setEnabling(row.id);
    setProblem(null);
    try {
      if (engine.enabled) {
        await client.disableCliProvider(row.id);
      } else {
        await client.enableCliProvider(row.id);
        toast.show(t("settingsModal.ai.detected.enabledToast", { name: row.display_name }), "success");
      }
      await reloadProviders();
      notifyProvidersChanged();
      await loadDetected(false);
    } catch (e) {
      fail(e);
    } finally {
      setEnabling(null);
    }
  };

  const { installed, available } = splitEngines(engines);

  /** One compact row; everything else is behind "Details". */
  const renderRow = (engine: CliEngineDto) => {
    const row = engine.detected;
    const status = cliStatus(engine);
    const open = openId === row.id;
    const install = engine.install;
    const meta = [row.account_label, shortVersion(row.version)].filter(Boolean).join(" · ");
    const note = status === "unsupported"
      ? t("settingsModal.ai.detected.noteUnsupported")
      : engine.native && !row.runnable ? t("settingsModal.ai.detected.noteNative") : null;
    return (
      <li key={row.id} className={styles.cliRow} data-testid={`detected-${row.id}`}>
        <div className={styles.cliMain}>
          <BrandIcon name={brandOf(row.id)} bare size={22} />
          <div className={styles.cliName}>
            <span className={styles.cliTitle}>
              {row.display_name}
              <span className={`${styles.statusChip} ${styles[`status_${status}`]}`}>
                {t(`settingsModal.ai.detected.status.${STATUS_KEY[status]}`)}
              </span>
              {engine.native?.support === "experimental" ? (
                <span className={styles.miniChip}>{t("settingsModal.ai.detected.experimentalChip")}</span>
              ) : null}
            </span>
            {meta ? <span className={styles.sub}>{meta}</span> : null}
          </div>
          <div className={styles.ctl}>
            {status === "not_installed" && install?.executable ? (
              <button type="button" className={styles.pill} disabled={installing || approval !== null} onClick={() => void previewInstall(row.id)}>
                {t("settingsModal.ai.detected.install")}
              </button>
            ) : null}
            {hasSwitch(engine) ? (
              <Switch
                checked={engine.enabled}
                disabled={enabling === row.id}
                label={t("settingsModal.ai.detected.toggle", { name: row.display_name })}
                onChange={() => void onToggle(engine)}
              />
            ) : null}
            <button
              type="button"
              className={styles.detailsBtn}
              aria-expanded={open}
              aria-label={`${t("settingsModal.ai.detected.detailsToggle")}: ${row.display_name}`} title={`${t("settingsModal.ai.detected.detailsToggle")}: ${row.display_name}`}
              onClick={() => setOpenId(open ? null : row.id)}
            >
              <ChevronDown size={14} aria-hidden="true" className={open ? styles.chevOpen : undefined} />
            </button>
          </div>
        </div>
        {open ? (
          <dl className={styles.cliDetails}>
            {engine.path ? <><dt>{t("settingsModal.ai.detected.pathLabel")}</dt><dd className={styles.cliPath}>{displayPath(engine.path)}</dd></> : null}
            {install ? <>
              <dt>{t("settingsModal.ai.detected.installLabel")}</dt>
              <dd>
                <code className={styles.cliCommand}>{install.command}</code>{" "}
                <button type="button" className={styles.textBtn} onClick={() => void copyText(install.command, t("settingsModal.ai.detected.copied"))}>
                  {t("settingsModal.ai.detected.copyCommand")}
                </button>
                {!install.verified ? <span className={styles.sub}> {t("settingsModal.ai.detected.noteUnverified")}</span> : null}
              </dd>
            </> : null}
            <dt>{t("settingsModal.ai.detected.loginLabel")}</dt>
            <dd><code className={styles.code}>{engine.login}</code></dd>
            {engine.docs_url ? <>
              <dt>{t("settingsModal.ai.detected.docs")}</dt>
              <dd>
                <button type="button" className={styles.textBtn} onClick={() => void openExternal(engine.docs_url)}>
                  {engine.docs_url.replace(/^https?:\/\//, "")}
                </button>
              </dd>
            </> : null}
            {note ? <dd className={styles.cliNote}>{note}</dd> : null}
          </dl>
        ) : null}
      </li>
    );
  };

  return (
    <section className={styles.sec} data-testid={local ? "detected-accounts-local" : "detected-accounts-block"} aria-labelledby="ai-detected-h">
      <div className={styles.head}>
        <h3 id="ai-detected-h">{local ? t("settingsModal.ai.detected.onThisComputer") : t("settingsModal.ai.detected.title")}</h3>
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
          <p className={styles.errorRow} role="alert">{t("settingsModal.ai.detected.offline")}</p>
        ) : loadError ? (
          <div className={styles.errorRow} role="alert">
            <span>{t("settingsModal.ai.detected.detectError")}</span>
            <span className={styles.grow} />
            <button type="button" className={styles.textBtn} onClick={() => void copyText(loadError, t("settingsModal.ai.detected.detailsCopied"))}>
              {t("settingsModal.ai.detected.copyDetails")}
            </button>
            <button type="button" className={styles.pill} disabled={loading} onClick={() => void loadDetected(true)}>
              {t("settingsModal.ai.detected.retry")}
            </button>
          </div>
        ) : (
          <>
            <ul className={styles.list}>{installed.map(renderRow)}</ul>
            {available.length > 0 ? (
              <details className={styles.moreEngines}>
                <summary>{t("settingsModal.ai.detected.moreEngines", { count: available.length })}</summary>
                <ul className={styles.list}>{available.map(renderRow)}</ul>
              </details>
            ) : null}
          </>
        )}
      </div>

      {approval ? <div className={styles.cliApproval}>
        <pre className={styles.cliCommand}>{approval.command}</pre>
        <p className={styles.hint}>{t("settingsModal.ai.detected.installApproval")}</p>
        <ApprovalCard key={approval.approval_id} expiresAtMs={approval.expires_at_ms} tool={{
          kind: "tool", key: approval.approval_id, callId: approval.approval_id,
          name: "cli_install", input: { command: approval.command },
          status: "awaiting_approval", requestedMs: Date.now(), gated: true,
          summary: t("settingsModal.ai.detected.installSummary", { engine: approval.engine_id }),
          details: { command: approval.command }
        }} onDecide={decideInstall} />
        {approvalRemaining === 0 && !installing ? <button type="button" className={styles.textBtn} onClick={() => setApproval(null)}>{t("settingsModal.ai.detected.dismissExpired")}</button> : null}
      </div> : null}
      {installing ? <p role="status" className={styles.hint}>{t("settingsModal.ai.detected.installRunning")}</p> : null}
      <p className={styles.hint}>
        <Info size={12} aria-hidden="true" /> {t("settingsModal.ai.detected.description")}{" "}
        {t("settingsModal.ai.detected.termsNotice")}
      </p>
      {problem ? (
        <p className={styles.error} role="alert">
          {problem.message}{" "}
          <button type="button" className={styles.textBtn} onClick={() => void copyText(problem.details, t("settingsModal.ai.detected.detailsCopied"))}>
            {t("settingsModal.ai.detected.copyDetails")}
          </button>
        </p>
      ) : null}
    </section>
  );
}
