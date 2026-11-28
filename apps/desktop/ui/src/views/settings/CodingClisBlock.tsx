import { ExternalLink, MoreHorizontal, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type RefObject } from "react";
import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";
import type { CliEngineDto, CliInstallApproval, ProvidersDto } from "../../api/types";
import { ApprovalCard, useCountdown } from "../../components/ChatCards";
import { BrandIcon } from "../../components/BrandIcon";
import { MoreInfo } from "../../components/ChoiceList";
import { Popover } from "../../components/Popover";
import { ServerSignInModal } from "../../components/ServerSignInModal";
import { useToast } from "../../components/Toast";
import { configuredCliIds } from "../../lib/detectedAccounts";
import { cliBrandKey, displayPath, needsProviderReload, shortVersion, type CliStatus } from "../../lib/cliCards";
import { openExternal } from "../../lib/externalLink";
import { humanLine } from "../../lib/humanError";
import { redactDetails } from "../../lib/redactDetails";
import { mergeCodingClis, localCanToggle, type UnifiedCliRow } from "../../lib/unifiedCli";
import { stepCommand } from "../../lib/serverSignIn";
import { useDaemon } from "../../state/DaemonContext";
import { notifyProvidersChanged, useProviders } from "../../state/useProviders";
import { useCodingCliQuota } from "../../state/useCodingCliQuota";
import { HostAdminAlert, useHostAdmin, useServerInstall } from "./HomeCards";
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

const STATUS_I18N: Record<CliStatus, string> = {
  ready: "ready",
  sign_in: "signIn",
  outdated: "outdated",
  not_installed: "notInstalled",
  unsupported: "unsupported"
};

function placeLabel(t: TFunction, row: UnifiedCliRow, serverName: string, showServer: boolean) {
  const localKey = `settingsUi.codingClis.local.${row.localPlace}`;
  const parts = [t("settingsUi.codingClis.thisComputer"), t(localKey)];
  if (row.localDetail !== "ready" && row.localDetail !== "na" && row.local) {
    parts.push(t(`settingsModal.ai.detected.status.${STATUS_I18N[row.localDetail]}`));
  }
  if (showServer) {
    parts.push(" · ", t("settingsUi.codingClis.myServer", { name: serverName }), " · ", t(`settingsUi.codingClis.server.${row.serverPlace}`));
  }
  return parts.join("");
}

function CodingCliRow({
  row,
  showServer,
  serverName,
  environment,
  localClient,
  onReload
}: {
  row: UnifiedCliRow;
  showServer: boolean;
  serverName: string;
  environment: ReturnType<typeof useHostAdmin>["environment"];
  localClient: NonNullable<ReturnType<typeof useDaemon>["localApi"]>;
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
    serverEngine ? onReload : undefined
  );

  const local = row.local;
  const meta = [
    local?.detected.account_label,
    shortVersion(local?.detected.version ?? serverEngine?.version)
  ]
    .filter(Boolean)
    .join(" · ");

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
      setMenuOpen(false);
    }
  };

  const copyLoginLocal = async () => {
    if (!local?.login) {
      return;
    }
    try {
      await navigator.clipboard.writeText(local.login);
      toast.show(t("settingsModal.ai.detected.copied"), "success");
    } catch {
      /* ignore */
    }
    setMenuOpen(false);
  };

  const runAction = (fn: () => void) => {
    setMenuOpen(false);
    fn();
  };

  return (
    <li
      className={styles.cliRow}
      data-testid={`coding-cli-${row.id}`}
      data-settings-field={`cli-${row.id}`}
    >
      <div className={styles.cliMain}>
        <BrandIcon name={cliBrandKey(row.id)} bare size={22} />
        <div className={styles.cliName}>
          <span className={styles.cliTitle}>{row.name}</span>
          <span className={styles.sub} data-testid={`coding-cli-places-${row.id}`}>
            {placeLabel(t, row, serverName, showServer)}
          </span>
          {meta ? <span className={styles.sub}>{meta}</span> : null}
          <QuotaHint engineId={row.id} />
        </div>
        <CliActionsButton btnRef={menuBtn} open={menuOpen} onToggle={() => setMenuOpen((o) => !o)} label={t("settingsUi.codingClis.actions", { name: row.name })} />
      </div>
      <Popover
        open={menuOpen}
        anchor={menuBtn}
        onClose={() => setMenuOpen(false)}
        placement="bottom-end"
        label={t("settingsUi.codingClis.actions", { name: row.name })}
        width={260}
      >
        <div className={styles.cliMenu}>
          {localCanToggle(row) ? (
            <button type="button" role="menuitem" className={styles.cliMenuItem} disabled={enabling} onClick={() => void toggleLocal()}>
              {local?.enabled ? t("settingsUi.codingClis.turnOff") : t("settingsUi.codingClis.turnOn")}
            </button>
          ) : null}
          {local && !local.detected.installed && local.install?.executable ? (
            <RowInstallLocal row={row} client={localClient} onDone={onReload} onClose={() => setMenuOpen(false)} />
          ) : null}
          {showServer && serverEngine && !serverEngine.installed && serverEngine.can_install ? (
            <button
              type="button"
              role="menuitem"
              className={styles.cliMenuItem}
              disabled={setup.installing}
              onClick={() => runAction(() => void setup.install())}
            >
              {setup.installing ? t("home.engines.installing") : t("home.engines.installOnServer")}
            </button>
          ) : null}
          {showServer && serverEngine?.installed && serverEngine.signed_in !== true ? (
            <button type="button" role="menuitem" className={styles.cliMenuItem} onClick={() => runAction(() => setSigningIn(true))}>
              {t("home.engines.signInOnServer", { server: serverName })}
            </button>
          ) : null}
          {showServer && serverEngine?.install_command ? (
            <MenuCopy label={t("settingsUi.codingClis.copyInstallServer")} text={stepCommand(environment, "install", serverEngine)} />
          ) : null}
          {showServer && serverEngine?.installed && serverEngine.login ? (
            <MenuCopy label={t("settingsUi.codingClis.copyLoginServer")} text={stepCommand(environment, "login", serverEngine)} inApp={serverEngine.login} />
          ) : null}
          {local?.login ? (
            <button type="button" role="menuitem" className={styles.cliMenuItem} onClick={() => void copyLoginLocal()}>
              {t("settingsUi.codingClis.copyLoginLocal")}
            </button>
          ) : null}
          {local?.path ? (
            <span className={styles.cliMenuNote} role="presentation">
              {t("settingsModal.ai.detected.pathLabel")}: {displayPath(local.path)}
            </span>
          ) : null}
          {showServer && serverEngine?.installed && serverEngine.can_install ? (
            <button
              type="button"
              role="menuitem"
              className={styles.cliMenuItem}
              disabled={setup.installing}
              onClick={() => runAction(() => void setup.install())}
            >
              {t("home.engines.reinstall")}
            </button>
          ) : null}
          {(local?.docs_url || serverEngine?.docs_url) ? (
            <button
              type="button"
              role="menuitem"
              className={styles.cliMenuItem}
              onClick={() =>
                runAction(() => void openExternal(local?.docs_url || serverEngine?.docs_url || ""))
              }
            >
              <ExternalLink size={14} aria-hidden="true" />
              {t("settingsUi.codingClis.howToInstall")}
            </button>
          ) : null}
        </div>
      </Popover>
      {signingIn && serverEngine ? (
        <ServerSignInModal
          open={signingIn}
          engine={serverEngine}
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
  onToggle,
  label
}: {
  btnRef: RefObject<HTMLButtonElement | null>;
  open: boolean;
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
      onClick={onToggle}
    >
      <MoreHorizontal size={16} aria-hidden="true" />
    </button>
  );
}

function MenuCopy({ label, text }: { label: string; text: string; inApp?: string }) {
  const { t } = useTranslation();
  const toast = useToast();
  return (
    <button
      type="button"
      role="menuitem"
      className={styles.cliMenuItem}
      onClick={() => {
        void navigator.clipboard?.writeText(text).then(() => toast.show(t("errors.copied"), "success"));
      }}
    >
      {label}
      <code className={styles.cliCommand}>{text}</code>
    </button>
  );
}

function RowInstallLocal({
  row,
  client,
  onDone,
  onClose
}: {
  row: UnifiedCliRow;
  client: NonNullable<ReturnType<typeof useDaemon>["localApi"]>;
  onDone: () => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const install = async () => {
    setBusy(true);
    onClose();
    try {
      const approval = await client.previewCliInstall(row.id);
      await client.decideCliInstall(approval.approval_id, "approve", () => {});
      toast.show(t("settingsModal.ai.detected.installDone"), "success");
      onDone();
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setBusy(false);
    }
  };
  return (
    <button type="button" role="menuitem" className={styles.cliMenuItem} disabled={busy} onClick={() => void install()}>
      {t("settingsModal.ai.detected.install")}
    </button>
  );
}

/** One list of coding CLIs with per-place status and a row menu for every action. */
export function CodingClisBlock() {
  const { t } = useTranslation();
  const toast = useToast();
  const { client: activeClient, localApi, home } = useDaemon();
  const showServer = home.kind === "host";
  const serverName = home.kind === "host" ? home.name : "";
  const hostAdmin = useHostAdmin();
  const shared = useProviders();
  const localClient = showServer ? localApi : activeClient;

  const [localEngines, setLocalEngines] = useState<CliEngineDto[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [approval, setApproval] = useState<CliInstallApproval | null>(null);
  const approvalRemaining = useCountdown(approval?.expires_at_ms);
  const [installing, setInstalling] = useState(false);
  const installOutput = useRef("");
  const [localDto, setLocalDto] = useState<ProvidersDto | null>(null);

  const enabledCli = useMemo(() => configuredCliIds(localDto ?? shared.dto), [localDto, shared.dto]);

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
        setLoadError(redactDetails(e instanceof Error && e.message ? e.message : "detect_failed"));
      } finally {
        setLoading(false);
      }
    },
    [localClient, showServer]
  );

  const reloadAll = useCallback(() => {
    void loadLocal(true);
    if (showServer) {
      hostAdmin.reload();
    }
  }, [loadLocal, showServer, hostAdmin]);

  useEffect(() => {
    void loadLocal(false);
  }, [loadLocal]);

  useEffect(() => {
    if (needsProviderReload(localEngines, enabledCli)) {
      void reloadProviders();
      notifyProvidersChanged();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [localEngines]);

  const rows = useMemo(
    () => mergeCodingClis(localEngines, showServer ? hostAdmin.engines : null),
    [localEngines, showServer, hostAdmin.engines]
  );

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
    } finally {
      setInstalling(false);
      reloadAll();
    }
  };

  const who = serverName || t("computerPicker.host");

  return (
    <section className={styles.sec} data-testid="coding-clis-block" aria-labelledby="coding-clis-h">
      <div className={styles.head}>
        <h3 id="coding-clis-h">{t("settingsUi.codingClis.title")}</h3>
        <span className={styles.grow} />
        <button type="button" className={styles.textBtn} disabled={!localClient || loading} onClick={reloadAll}>
          <RefreshCw size={14} aria-hidden="true" className={loading ? styles.spin : undefined} />
          {t("settingsModal.ai.detected.refresh")}
        </button>
      </div>
      {showServer ? <HostAdminAlert name={who} state={hostAdmin.state} problem={hostAdmin.problem} /> : null}
      <div className={styles.card}>
        {!localClient ? (
          <p className={styles.errorRow} role="alert">
            {t("settingsModal.ai.detected.offline")}
          </p>
        ) : loadError ? (
          <div className={styles.errorRow} role="alert">
            <span>{t("settingsModal.ai.detected.detectError")}</span>
            <button type="button" className={styles.pill} disabled={loading} onClick={() => void loadLocal(true)}>
              {t("settingsModal.ai.detected.retry")}
            </button>
          </div>
        ) : rows.length === 0 ? (
          <p className={styles.hint}>{showServer ? t("home.engines.loading") : t("settingsModal.ai.detected.detectError")}</p>
        ) : (
          <ul className={styles.list}>
            {rows.map((row) => (
              <CodingCliRow
                key={row.id}
                row={row}
                showServer={showServer}
                serverName={who}
                environment={hostAdmin.environment}
                localClient={localClient}
                onReload={reloadAll}
              />
            ))}
          </ul>
        )}
      </div>
      {approval ? (
        <div className={styles.cliApproval}>
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
      <MoreInfo testId="coding-clis-more">
        <p>{t("settingsUi.codingClis.note")}</p>
        {showServer ? <p>{t("home.engines.note", { name: who })}</p> : null}
        <p>{t("settingsModal.ai.detected.termsNotice")}</p>
      </MoreInfo>
    </section>
  );
}