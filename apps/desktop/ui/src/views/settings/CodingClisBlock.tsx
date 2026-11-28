import { ExternalLink, MoreHorizontal, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import type { ApiClient } from "../../api/client";
import type { CliEngineDto, CliInstallApproval, ProvidersDto } from "../../api/types";
import { ApprovalCard, useCountdown } from "../../components/ChatCards";
import { BrandIcon } from "../../components/BrandIcon";
import { MoreInfo } from "../../components/ChoiceList";
import { Popover } from "../../components/Popover";
import { RunOnServer } from "../../components/RunOnServer";
import { ServerSignInModal } from "../../components/ServerSignInModal";
import { useToast } from "../../components/Toast";
import { configuredCliIds } from "../../lib/detectedAccounts";
import { cliBrandKey, cliStatus, displayPath, needsProviderReload, shortVersion } from "../../lib/cliCards";
import { openExternal } from "../../lib/externalLink";
import { humanLine } from "../../lib/humanError";
import { redactDetails } from "../../lib/redactDetails";
import { mergeCodingClis, localCanToggle, placeLabel, type UnifiedCliRow } from "../../lib/unifiedCli";
import { stepCommand } from "../../lib/serverSignIn";
import { useDaemon, useLocalDaemonClient } from "../../state/DaemonContext";
import { notifyProvidersChanged, useProviders } from "../../state/useProviders";
import { useCodingCliQuota } from "../../state/useCodingCliQuota";
import { HostAdminAlert, InstallFeedback, useHostAdmin, useServerInstall } from "./HomeCards";
import styles from "./AiPage.module.css";

function QuotaHint({ engineId }: { engineId: string }) {
  const { t } = useTranslation();
  const { quota, fetchedAtMs } = useCodingCliQuota(engineId);
  if (!quota) {
    return null;
  }
  const hours = fetchedAtMs ? Math.max(0, Math.round((Date.now() - fetchedAtMs) / 3_600_000)) : null;
  const age =
    hours === null
      ? null
      : hours < 1
        ? t("settingsUi.codingClis.quotaFresh")
        : t("settingsUi.codingClis.quotaAge", { hours });
  return (
    <span className={styles.quotaHint}>
      {t("settingsUi.codingClis.quotaLeft", { pct: quota.remainingPct })}
      {age ? ` · ${age}` : null}
    </span>
  );
}

function CodingCliRow({
  row,
  showServer,
  serverName,
  environment,
  localClient,
  hostApi,
  installBusy,
  onInstallLocal,
  onReload
}: {
  row: UnifiedCliRow;
  showServer: boolean;
  serverName: string;
  environment: ReturnType<typeof useHostAdmin>["environment"];
  localClient: ApiClient | null;
  /** The server's own channel: server actions exist only with it, never through this computer's daemon. */
  hostApi: ApiClient | null;
  installBusy: boolean;
  onInstallLocal: (id: string) => void;
  onReload: () => void;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const shared = useProviders();
  const [menuOpen, setMenuOpen] = useState(false);
  const menuBtn = useRef<HTMLButtonElement | null>(null);
  const [signingIn, setSigningIn] = useState(false);
  const [enabling, setEnabling] = useState(false);
  const serverEngine = row.server;
  const setup = useServerInstall(
    serverEngine ?? {
      id: row.id,
      name: row.name,
      installed: false,
      can_install: false,
      login: "",
      login_flow: "manual",
      docs_url: ""
    },
    hostApi,
    serverEngine ? onReload : undefined
  );
  const admin = showServer && hostApi !== null && serverEngine !== null ? serverEngine : null;

  const local = row.local;
  const localInstall = local && !local.detected.installed && local.install?.executable ? local.install : null;
  const meta = [
    ...new Set(
      [
        local?.detected.account_label,
        shortVersion(local?.detected.version ?? serverEngine?.version)
      ].filter(Boolean)
    )
  ].join(" · ");
  const note =
    local && cliStatus(local) === "unsupported"
      ? t("settingsModal.ai.detected.noteUnsupported")
      : local?.native && !local.detected.runnable
        ? t("settingsModal.ai.detected.noteNative")
        : null;

  /** Leaves the menu and puts focus back on its button, so a keyboard user keeps their place. */
  const closeMenu = () => {
    setMenuOpen(false);
    menuBtn.current?.focus();
  };
  const runAction = (fn: () => void) => {
    closeMenu();
    fn();
  };

  const toggleLocal = async () => {
    if (!local || !localClient) {
      return;
    }
    setEnabling(true);
    try {
      if (local.enabled) {
        await localClient.disableCliProvider(row.id);
      } else {
        await localClient.enableCliProvider(row.id);
        toast.show(t("settingsModal.ai.detected.enabledToast", { name: row.name }), "success");
      }
      await shared.reload();
      notifyProvidersChanged();
      onReload();
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setEnabling(false);
    }
  };

  return (
    <li className={styles.cliRow} data-testid={`coding-cli-${row.id}`}>
      <div className={styles.cliMain}>
        <BrandIcon name={cliBrandKey(row.id)} bare size={22} />
        <div className={styles.cliName}>
          <span className={styles.cliTitle}>
            {row.name}
            {local?.native?.support === "experimental" ? (
              <span className={styles.miniChip}>{t("settingsModal.ai.detected.experimentalChip")}</span>
            ) : null}
          </span>
          <span className={`${styles.sub} ${styles.cliPlaces}`} data-testid={`coding-cli-places-${row.id}`}>
            {placeLabel(t, row, serverName, showServer)}
          </span>
          {meta ? <span className={styles.sub}>{meta}</span> : null}
          {local?.path ? (
            <span className={styles.cliPath} data-testid={`coding-cli-path-${row.id}`}>
              {t("settingsModal.ai.detected.pathLabel")}: {displayPath(local.path)}
            </span>
          ) : null}
          {showServer && serverEngine?.installed && serverEngine.signed_in === true && serverEngine.account ? (
            <span className={styles.sub} data-testid={`engine-account-${row.id}`}>
              {t("settingsUi.codingClis.accountOnServer", { name: serverName, account: serverEngine.account })}
            </span>
          ) : null}
          {showServer && serverEngine?.installed && serverEngine.path ? (
            <span className={styles.cliPath} data-testid={`engine-path-${row.id}`}>
              {t("settingsUi.codingClis.pathOnServer", { name: serverName })}: {serverEngine.path}
            </span>
          ) : null}
          {localInstall && !localInstall.verified ? (
            <span className={styles.sub}>{t("settingsModal.ai.detected.noteUnverified")}</span>
          ) : null}
          {note ? <span className={styles.sub}>{note}</span> : null}
          <QuotaHint engineId={row.id} />
          {admin ? (
            <div className={styles.cliStep}>
              <InstallFeedback state={setup} />
            </div>
          ) : null}
        </div>
        <CliActionsButton
          btnRef={menuBtn}
          open={menuOpen}
          field={`cli-${row.id}`}
          onToggle={() => setMenuOpen((o) => !o)}
          label={t("settingsUi.codingClis.actions", { name: row.name })}
        />
      </div>
      <Popover
        open={menuOpen}
        anchor={menuBtn}
        onClose={() => setMenuOpen(false)}
        placement="bottom-end"
        label={t("settingsUi.codingClis.actions", { name: row.name })}
        width={260}
      >
        <div className={styles.cliMenu} role="none">
          {local && localClient && localCanToggle(row) ? (
            <button
              type="button"
              role="menuitem"
              className={styles.cliMenuItem}
              disabled={enabling}
              onClick={() => runAction(() => void toggleLocal())}
            >
              {local.enabled ? t("settingsUi.codingClis.turnOff") : t("settingsUi.codingClis.turnOn")}
            </button>
          ) : null}
          {localInstall && localClient ? (
            <button
              type="button"
              role="menuitem"
              className={styles.cliMenuItem}
              disabled={installBusy}
              data-testid={`install-${row.id}`}
              onClick={() => runAction(() => onInstallLocal(row.id))}
            >
              {t("settingsModal.ai.detected.install")}
            </button>
          ) : null}
          {localInstall ? (
            <MenuCopy
              label={t("settingsUi.codingClis.copyInstallLocal")}
              text={localInstall.command}
              onCopied={() => toast.show(t("settingsModal.ai.detected.copied"), "success")}
              onDone={closeMenu}
            />
          ) : null}
          {local?.login ? (
            <MenuCopy
              label={t("settingsUi.codingClis.copyLoginLocal")}
              text={local.login}
              onCopied={() => toast.show(t("settingsModal.ai.detected.copied"), "success")}
              onDone={closeMenu}
            />
          ) : null}
          {admin && !admin.installed && admin.can_install ? (
            <button
              type="button"
              role="menuitem"
              className={styles.cliMenuItem}
              disabled={setup.installing}
              data-testid={`engine-install-${row.id}`}
              onClick={() => runAction(() => void setup.install())}
            >
              {setup.installing ? t("home.engines.installing") : t("home.engines.installOnServer")}
            </button>
          ) : null}
          {admin?.installed && admin.signed_in !== true ? (
            <button
              type="button"
              role="menuitem"
              className={styles.cliMenuItem}
              data-testid={`engine-signin-${row.id}`}
              onClick={() => runAction(() => setSigningIn(true))}
            >
              {t("home.engines.signInOnServer", { server: serverName })}
            </button>
          ) : null}
          {admin && !admin.installed && admin.install_command ? (
            <MenuCopy
              label={t("settingsUi.codingClis.copyInstallServer")}
              hint={environment?.user ? t("home.engines.runAsUser", { user: environment.user }) : t("home.engines.runOnServer")}
              text={stepCommand(environment, "install", admin)}
              runCommand={admin.install_command}
              onCopied={() => toast.show(t("errors.copied"), "success")}
              onDone={closeMenu}
            />
          ) : null}
          {admin?.installed && admin.login ? (
            <MenuCopy
              label={t("settingsUi.codingClis.copyLoginServer")}
              hint={environment?.user ? t("home.engines.signInAsUser", { user: environment.user }) : t("home.engines.signIn")}
              text={stepCommand(environment, "login", admin)}
              runCommand={admin.login}
              onCopied={() => toast.show(t("errors.copied"), "success")}
              onDone={closeMenu}
            />
          ) : null}
          {admin?.installed && admin.can_install ? (
            <button
              type="button"
              role="menuitem"
              className={styles.cliMenuItem}
              disabled={setup.installing}
              data-testid={`engine-reinstall-${row.id}`}
              onClick={() => runAction(() => void setup.install())}
            >
              {setup.installing ? t("home.engines.installing") : t("home.engines.reinstall")}
            </button>
          ) : null}
          {local?.docs_url || serverEngine?.docs_url ? (
            <button
              type="button"
              role="menuitem"
              className={`${styles.cliMenuItem} ${styles.cliMenuLink}`}
              onClick={() => runAction(() => void openExternal(local?.docs_url || serverEngine?.docs_url || ""))}
            >
              <ExternalLink size={14} aria-hidden="true" />
              {t("settingsUi.codingClis.howToInstall")}
            </button>
          ) : null}
        </div>
      </Popover>
      {signingIn && admin ? (
        <ServerSignInModal
          open={signingIn}
          engine={admin}
          client={hostApi}
          serverName={serverName}
          onClose={() => setSigningIn(false)}
          onDone={() => {
            setSigningIn(false);
            onReload();
          }}
        />
      ) : null}
    </li>
  );
}

function CliActionsButton({
  btnRef,
  open,
  field,
  onToggle,
  label
}: {
  btnRef: RefObject<HTMLButtonElement | null>;
  open: boolean;
  /** The deep-link target ("Set up" in the model picker) that lands focus here. */
  field: string;
  onToggle: () => void;
  label: string;
}) {
  return (
    <button
      ref={btnRef}
      type="button"
      className={styles.detailsBtn}
      aria-haspopup="menu"
      aria-expanded={open}
      aria-label={label}
      title={label}
      data-settings-field={field}
      onClick={onToggle}
    >
      <MoreHorizontal size={16} aria-hidden="true" />
    </button>
  );
}

/**
 * A command in the menu: copy it, and (for a server command, where this desktop has a terminal on
 * that server) type it into the server terminal. `text` is what a person logged in over SSH runs;
 * `runCommand` is what the in-app terminal gets, which already runs as the service user.
 */
function MenuCopy({
  label,
  hint,
  text,
  runCommand,
  onCopied,
  onDone
}: {
  label: string;
  /** Who runs it and where (the server's user), above the command. */
  hint?: string;
  text: string;
  runCommand?: string;
  onCopied: () => void;
  onDone: () => void;
}) {
  return (
    <div role="none">
      <button
        type="button"
        role="menuitem"
        className={styles.cliMenuItem}
        onClick={() => {
          navigator.clipboard
            ?.writeText(text)
            .then(onCopied)
            .catch(() => {});
          onDone();
        }}
      >
        {label}
        {hint ? <span className={styles.sub}>{hint}</span> : null}
        <code className={styles.cliCommand}>{text}</code>
      </button>
      {runCommand ? <RunOnServer command={runCommand} role="menuitem" className={styles.cliMenuItem} beforeOpen={onDone} /> : null}
    </div>
  );
}

/** One list of coding CLIs with per-place status and a row menu for every action. */
export function CodingClisBlock() {
  const { t } = useTranslation();
  const toast = useToast();
  const { workLocalNow } = useDaemon();
  const localClient = useLocalDaemonClient();
  const hostAdmin = useHostAdmin();
  const { reload: reloadHost, api: hostApi, status: serverStatus } = hostAdmin;
  const showServer = hostAdmin.onHome;
  const shared = useProviders();

  const [localEngines, setLocalEngines] = useState<CliEngineDto[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [approval, setApproval] = useState<CliInstallApproval | null>(null);
  const approvalRemaining = useCountdown(approval?.expires_at_ms);
  const approvalBox = useRef<HTMLDivElement | null>(null);
  const [installing, setInstalling] = useState(false);
  /** Installer output stays out of the page; it is only copied on request. */
  const installOutput = useRef("");
  const [problem, setProblem] = useState<{ message: string; details: string } | null>(null);
  const [localDto, setLocalDto] = useState<ProvidersDto | null>(null);

  const enabledCli = useMemo(() => configuredCliIds(showServer ? localDto : shared.dto), [showServer, localDto, shared.dto]);

  const reloadProviders = useCallback(async () => {
    if (showServer && localClient) {
      setLocalDto(await localClient.providers().catch(() => null));
    }
    await shared.reload();
  }, [showServer, localClient, shared]);

  const loadLocal = useCallback(
    async (refresh: boolean) => {
      if (!localClient) {
        setLocalEngines([]);
        setLoadError(null);
        return;
      }
      setLoading(true);
      setLoadError(null);
      try {
        setLocalEngines(await localClient.cliEngines(refresh));
        if (showServer) {
          setLocalDto(await localClient.providers().catch(() => null));
        }
      } catch (e) {
        setLocalEngines([]);
        setLoadError(redactDetails(e instanceof Error && e.message ? e.message : "detect_failed"));
      } finally {
        setLoading(false);
      }
    },
    [localClient, showServer]
  );

  // `reloadHost` is stable: a callback that changed on every render would re-run the install effect
  // of every row once its install finished, and reload forever.
  const reloadAll = useCallback(() => {
    void loadLocal(true);
    if (showServer) {
      reloadHost();
    }
  }, [loadLocal, showServer, reloadHost]);

  useEffect(() => {
    void loadLocal(false);
  }, [loadLocal]);

  // The daemon switches a detected, signed-in engine on the first time it sees it;
  // pick that up in the provider list once.
  useEffect(() => {
    if (needsProviderReload(localEngines, enabledCli)) {
      void reloadProviders();
      notifyProvidersChanged();
    }
    // Only a fresh engine list can change the answer.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [localEngines]);

  // The approval card sits below the list: bring it into view when an install asks for it.
  useEffect(() => {
    if (approval) {
      approvalBox.current?.scrollIntoView?.({ block: "nearest" });
    }
  }, [approval]);

  const localDown = !localClient || loadError !== null;
  const rows = useMemo(
    () =>
      mergeCodingClis(localDown ? [] : localEngines, showServer ? hostAdmin.engines : null, {
        serverStatus: showServer ? serverStatus : "ready",
        localDown: showServer && localDown
      }),
    [localDown, localEngines, showServer, hostAdmin.engines, serverStatus]
  );

  const fail = (error: unknown) => {
    setProblem({
      message: t("settingsModal.ai.detected.problem"),
      details: redactDetails(error instanceof Error ? error.message : String(error))
    });
  };

  const copyText = async (text: string, done: string) => {
    try {
      await navigator.clipboard.writeText(text);
      toast.show(done, "success");
    } catch (error) {
      fail(error);
    }
  };

  /** Local installs always ask first: the owner reads the command and approves it on the card. */
  const previewInstall = async (id: string) => {
    if (!localClient) {
      return;
    }
    setProblem(null);
    installOutput.current = "";
    try {
      setApproval(await localClient.previewCliInstall(id));
    } catch (error) {
      fail(error);
    }
  };

  const decideInstall = async (decision: "approve" | "deny") => {
    if (!localClient || !approval) {
      return;
    }
    setInstalling(true);
    try {
      await localClient.decideCliInstall(approval.approval_id, decision, (text) => {
        installOutput.current = (installOutput.current + text).slice(-65536);
      });
      setApproval(null);
      if (decision === "approve") {
        toast.show(t("settingsModal.ai.detected.installDone"), "success");
      }
    } catch (error) {
      setApproval(null);
      const code = error instanceof Error ? error.message : "install_failed";
      setProblem({
        message: t(`settingsModal.ai.detected.installErrors.${code}`, {
          defaultValue: t("settingsModal.ai.detected.installErrors.install_failed")
        }),
        details: redactDetails(installOutput.current || code)
      });
    } finally {
      setInstalling(false);
      reloadAll();
    }
  };

  const who = hostAdmin.name || t("computerPicker.host");
  const serverGap = showServer && serverStatus === "unreachable";

  return (
    <section className={styles.sec} data-testid="coding-clis-block" aria-labelledby="coding-clis-h">
      <div className={styles.head}>
        <h3 id="coding-clis-h">{t("settingsUi.codingClis.title")}</h3>
        <span className={styles.grow} />
        <button type="button" className={styles.textBtn} disabled={(!localClient && !hostApi) || loading} onClick={reloadAll}>
          <RefreshCw size={14} aria-hidden="true" className={loading ? styles.spin : undefined} />
          {t("settingsModal.ai.detected.refresh")}
        </button>
      </div>
      {showServer ? <HostAdminAlert name={who} state={hostAdmin.state} problem={hostAdmin.problem} /> : null}
      {serverGap ? (
        <p className={styles.hint} role="status" data-testid="coding-clis-server-unreachable">
          {t(workLocalNow ? "settingsUi.codingClis.serverNotConnected" : "settingsUi.codingClis.serverUnreachable", { name: who })}
        </p>
      ) : null}
      <div className={styles.card}>
        {!localClient ? (
          <p className={styles.errorRow} role="alert">
            {t("settingsModal.ai.detected.offline")}
          </p>
        ) : loadError ? (
          <div className={styles.errorRow} role="alert">
            <span>{t("settingsModal.ai.detected.detectError")}</span>
            <span className={styles.grow} />
            <button type="button" className={styles.textBtn} onClick={() => void copyText(loadError, t("settingsModal.ai.detected.detailsCopied"))}>
              {t("settingsModal.ai.detected.copyDetails")}
            </button>
            <button type="button" className={styles.pill} disabled={loading} onClick={() => void loadLocal(true)}>
              {t("settingsModal.ai.detected.retry")}
            </button>
          </div>
        ) : null}
        {rows.length > 0 ? (
          <ul className={styles.list}>
            {rows.map((row) => (
              <CodingCliRow
                key={row.id}
                row={row}
                showServer={showServer}
                serverName={who}
                environment={hostAdmin.environment}
                localClient={localClient}
                hostApi={hostApi}
                installBusy={installing || approval !== null}
                onInstallLocal={(id) => void previewInstall(id)}
                onReload={reloadAll}
              />
            ))}
          </ul>
        ) : !localDown ? (
          <p className={styles.hint}>
            {showServer && serverStatus === "loading"
              ? t("home.engines.loading")
              : showServer && serverStatus === "ready"
                ? t("home.engines.none")
                : t("settingsModal.ai.detected.detectError")}
          </p>
        ) : null}
      </div>
      {approval ? (
        <div className={styles.cliApproval} ref={approvalBox}>
          <pre className={styles.cliCommand}>{approval.command}</pre>
          <p className={styles.hint}>{t("settingsModal.ai.detected.installApproval")}</p>
          <ApprovalCard
            key={approval.approval_id}
            expiresAtMs={approval.expires_at_ms}
            tool={{
              kind: "tool",
              key: approval.approval_id,
              callId: approval.approval_id,
              name: "cli_install",
              input: { command: approval.command },
              status: "awaiting_approval",
              requestedMs: Date.now(),
              gated: true,
              summary: t("settingsModal.ai.detected.installSummary", { engine: approval.engine_id }),
              details: { command: approval.command }
            }}
            onDecide={decideInstall}
          />
          {approvalRemaining === 0 && !installing ? (
            <button type="button" className={styles.textBtn} onClick={() => setApproval(null)}>
              {t("settingsModal.ai.detected.dismissExpired")}
            </button>
          ) : null}
        </div>
      ) : null}
      {installing ? (
        <p role="status" className={styles.hint}>
          {t("settingsModal.ai.detected.installRunning")}
        </p>
      ) : null}
      <MoreInfo testId="coding-clis-more">
        <p>{t("settingsUi.codingClis.note")}</p>
        {showServer ? <p>{t("home.engines.note", { name: who })}</p> : null}
        {showServer ? <p>{t("home.admin.byDesign", { name: who })}</p> : null}
        {showServer ? <p>{t("home.engines.signInNote")}</p> : null}
        {showServer && hostAdmin.environment?.user ? (
          <p data-testid="host-engines-user">
            {t(hostAdmin.environment.is_root ? "home.engines.runsAsRoot" : "home.engines.runsAs", {
              name: who,
              user: hostAdmin.environment.user,
              home: hostAdmin.environment.home ?? "?"
            })}
          </p>
        ) : null}
        <p>{t("settingsModal.ai.detected.termsNotice")}</p>
      </MoreInfo>
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
