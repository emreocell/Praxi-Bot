import { Check, ChevronDown, Copy, RefreshCw, TriangleAlert } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { AdminCliEngine, AdminCliEnvironment, AdminCliInstall, AdminState } from "../../api/types";
import { humanError } from "../../lib/humanError";
import { useDaemon } from "../../state/DaemonContext";
import { useActiveHost } from "../../state/useActiveHost";
import { BrandIcon } from "../../components/BrandIcon";
import { MoreInfo } from "../../components/ChoiceList";
import { RunOnServer } from "../../components/RunOnServer";
import { brandOf, shortVersion } from "../../lib/cliCards";
import { engineStatus, stepCommand } from "../../lib/serverSignIn";
import { ServerSignInModal } from "../../components/ServerSignInModal";
import { DetectedAccountsBlock } from "./DetectedAccountsBlock";
import styles from "./AiPage.module.css";

/**
 * A command to run on the server, one click to copy. `inApp` is what the in-app terminal gets
 * when it differs: that terminal already runs as the service user, so it must not receive the
 * `sudo ...` form a person logged in over SSH needs.
 */
export function CopyBlock({ text, inApp }: { text: string; inApp?: string }) {
  const { t } = useTranslation();
  const [done, setDone] = useState(false);
  return (
    <span className={styles.ctl}>
      <code className={styles.code}>{text}</code>
      <button
        type="button"
        className={styles.textBtn}
        onClick={() => {
          void navigator.clipboard
            ?.writeText(text)
            .then(() => {
              setDone(true);
              window.setTimeout(() => setDone(false), 1500);
            })
            .catch(() => {});
        }}
      >
        {done ? <Check size={12} aria-hidden="true" /> : <Copy size={12} aria-hidden="true" />}
        {done ? t("errors.copied") : t("home.copy")}
      </button>
      <RunOnServer command={inApp ?? text} className={styles.textBtn} />
    </span>
  );
}

function useHostAdmin() {
  const { client } = useDaemon();
  const active = useActiveHost();
  const onHome = active.isHost;
  const [state, setState] = useState<AdminState | null>(null);
  const [engines, setEngines] = useState<AdminCliEngine[] | null>(null);
  // Older hosts have no such answer: the lines that name the user are then simply left out.
  const [environment, setEnvironment] = useState<AdminCliEnvironment | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  const reload = useCallback(() => setTick((n) => n + 1), []);
  useEffect(() => {
    if (!onHome || !client) {
      return undefined;
    }
    let live = true;
    client.adminState().then(
      (s) => live && setState(s),
      (e) => live && setProblem(humanError(e).title)
    );
    client.adminCliEngines().then(
      (s) => live && setEngines(s),
      () => live && setEngines([])
    );
    client.adminCliEnvironment().then(
      (e) => live && setEnvironment(e),
      () => live && setEnvironment(null)
    );
    return () => {
      live = false;
    };
  }, [onHome, client, tick]);
  return { onHome, name: active.name, state, engines, environment, problem, reload };
}

/**
 * What the server still needs from its owner before this desktop can set it up. Shown only when
 * something is missing; what the server keeps to itself by design is under "How it works".
 */
function HostAdminAlert({ name, state, problem }: Pick<ReturnType<typeof useHostAdmin>, "state" | "problem"> & { name: string }) {
  const { t } = useTranslation();
  const notGranted = state !== null && !state.granted;
  const noSecrets = state !== null && !state.secrets_available;
  if (!problem && !notGranted && !noSecrets) {
    return null;
  }
  return (
    <div className={styles.hostAlert} role="status" data-testid="host-admin-note">
      <TriangleAlert size={16} aria-hidden="true" />
      <div className={styles.text}>
        {problem ? <span className={styles.sub}>{problem}</span> : null}
        {notGranted ? (
          <>
            <span className={styles.sub}>{t("home.admin.notGranted", { name })}</span>
            {state?.confirm_command ? <CopyBlock text={state.confirm_command} /> : null}
          </>
        ) : null}
        {noSecrets ? (
          <>
            <span className={styles.sub}>{t("home.admin.noSecrets", { name })}</span>
            {state?.secrets_hint ? <span className={styles.sub}>{state.secrets_hint}</span> : null}
          </>
        ) : null}
      </div>
    </div>
  );
}

/** Installing a coding CLI on the server: start it, follow it, and say what went wrong. */
function useServerInstall(engine: AdminCliEngine, onChanged?: () => void) {
  const { client } = useDaemon();
  const [run, setRun] = useState<AdminCliInstall | null>(null);
  const [problem, setProblem] = useState<{ code: string; text: string } | null>(null);
  const installing = run?.state === "installing";
  const finished = run?.state === "done" || run?.state === "failed";
  useEffect(() => {
    if (!installing || !client) {
      return undefined;
    }
    const timer = window.setInterval(() => {
      void client.adminCliEngineInstallStatus(engine.id).then(setRun, () => {});
    }, 2000);
    return () => window.clearInterval(timer);
  }, [installing, client, engine.id]);
  useEffect(() => {
    if (finished) {
      onChanged?.();
    }
  }, [finished, onChanged]);
  const install = async () => {
    if (!client) {
      return;
    }
    setProblem(null);
    try {
      setRun(await client.adminCliEngineInstall(engine.id));
    } catch (e) {
      const h = humanError(e);
      setProblem({ code: h.code, text: h.title });
    }
  };
  return {
    run,
    problem,
    installing,
    install,
    installed: engine.installed || run?.installed === true,
    signedIn: run?.signed_in ?? engine.signed_in
  };
}

/** The installer's output and what went wrong, under an install button. */
function InstallFeedback({ state }: { state: ReturnType<typeof useServerInstall> }) {
  const { t } = useTranslation();
  const { run, problem, installing } = state;
  return (
    <>
      {installing || (run?.output && run.state !== "idle") ? <pre className={styles.code}>{run?.output}</pre> : null}
      {run?.state === "failed" ? (
        <span className={`${styles.sub} ${styles.subWarn}`}>{t("home.engines.installFailed", { code: run.error ?? "exit" })}</span>
      ) : null}
      {problem ? (
        <span className={`${styles.sub} ${styles.subWarn}`}>
          {problem.code === "prerequisite_missing" ? t("home.engines.needNode") : problem.text}
          {problem.code === "prerequisite_missing" ? (
            <a href="https://nodejs.org/en/download" target="_blank" rel="noreferrer">
              nodejs.org
            </a>
          ) : null}
        </span>
      ) : null}
    </>
  );
}

/**
 * Install and sign-in steps of one coding CLI on the server. The command is the registry's, shown
 * for copying; where the owner's desktop may administer the server a button runs it THERE. A row
 * never shows an empty command: without a verified installer it points to the official page.
 */
export function EngineSetup({
  engine,
  environment,
  onChanged
}: {
  engine: AdminCliEngine;
  environment?: AdminCliEnvironment | null;
  onChanged?: () => void;
}) {
  const { t } = useTranslation();
  const serverName = useActiveHost().name || t("computerPicker.host");
  const [signingIn, setSigningIn] = useState(false);
  const setup = useServerInstall(engine, onChanged);
  const { installing, install, installed, signedIn } = setup;
  return (
    <>
      {!installed ? (
        <>
          {engine.install_command ? (
            <>
              <span className={styles.sub}>
                {environment?.user ? t("home.engines.runAsUser", { user: environment.user }) : t("home.engines.runOnServer")}
              </span>
              <CopyBlock text={stepCommand(environment, "install", engine)} inApp={engine.install_command} />
            </>
          ) : (
            <a className={styles.sub} href={engine.docs_url} target="_blank" rel="noreferrer" data-testid={`engine-docs-${engine.id}`}>
              {t("home.engines.officialPage", { name: engine.name })}
            </a>
          )}
          {engine.can_install ? (
            <button
              type="button"
              className={styles.textBtn}
              disabled={installing}
              data-testid={`engine-install-${engine.id}`}
              onClick={() => void install()}
            >
              {installing ? t("home.engines.installing") : t("home.engines.installOnServer")}
            </button>
          ) : null}
          <InstallFeedback state={setup} />
        </>
      ) : signedIn !== true ? (
        <>
          <button
            type="button"
            className={styles.textBtn}
            data-testid={`engine-signin-${engine.id}`}
            onClick={() => setSigningIn(true)}
          >
            {t("home.engines.signInOnServer", { server: serverName })}
          </button>
          {engine.can_install ? (
            <button
              type="button"
              className={styles.textBtn}
              disabled={installing}
              data-testid={`engine-reinstall-${engine.id}`}
              onClick={() => void install()}
            >
              {installing ? t("home.engines.installing") : t("home.engines.reinstall")}
            </button>
          ) : null}
          <span className={styles.sub}>
            {environment?.user ? t("home.engines.signInAsUser", { user: environment.user }) : t("home.engines.signIn")}
          </span>
          <CopyBlock text={stepCommand(environment, "login", engine)} inApp={engine.login} />
          <span className={styles.sub}>{t("home.engines.signInNote")}</span>
          <ServerSignInModal
            open={signingIn}
            engine={engine}
            serverName={serverName}
            onClose={() => setSigningIn(false)}
            onDone={() => {
              setSigningIn(false);
              onChanged?.();
            }}
          />
        </>
      ) : null}
    </>
  );
}

const ENGINE_CHIP: Record<string, { status: string; label: string }> = {
  "home.engines.notInstalled": { status: "not_installed", label: "settingsModal.ai.detected.status.notInstalled" },
  "home.engines.ready": { status: "ready", label: "settingsModal.ai.detected.status.ready" },
  "home.engines.readyAs": { status: "ready", label: "settingsModal.ai.detected.status.ready" },
  "home.engines.needsSignIn": { status: "sign_in", label: "settingsModal.ai.detected.status.signIn" }
};

/**
 * One coding CLI on the server, in the same compact row as "Found on this computer": icon, name,
 * one status chip, account and version, the one next step (Install / Sign in) and a chevron. The
 * row's details hold everything else for THIS CLI: where it is, the install command (copy, or run
 * it in the server terminal), reinstall, the sign-in command and the docs.
 */
function HostEngineRow({
  engine,
  environment,
  open,
  onToggle,
  onChanged
}: {
  engine: AdminCliEngine;
  environment: AdminCliEnvironment | null;
  open: boolean;
  onToggle: () => void;
  onChanged: () => void;
}) {
  const { t } = useTranslation();
  const serverName = useActiveHost().name || t("computerPicker.host");
  const [signingIn, setSigningIn] = useState(false);
  const setup = useServerInstall(engine, onChanged);
  const { installing, install, installed, signedIn } = setup;
  const status = engineStatus({ ...engine, installed, signed_in: signedIn });
  const chip = ENGINE_CHIP[status.key] ?? { status: "outdated", label: "settingsUi.serverCli.unknown" };
  const version = installed ? shortVersion(engine.version) : null;
  const meta = [signedIn === true ? engine.account : null, version].filter(Boolean).join(" · ");
  const needsSignIn = installed && signedIn !== true;
  const detailsId = `host-engine-details-${engine.id}`;
  return (
    <li className={styles.cliRow} data-testid={`host-engine-${engine.id}`}>
      <div className={styles.cliMain}>
        <BrandIcon name={brandOf(engine.id)} bare size={22} />
        <div className={styles.cliName}>
          <span className={styles.cliTitle}>
            {engine.name}
            <span className={`${styles.statusChip} ${styles[`status_${chip.status}`]}`}>{t(chip.label)}</span>
          </span>
          {meta ? <span className={styles.sub}>{meta}</span> : null}
        </div>
        <div className={styles.ctl}>
          {!installed && engine.can_install ? (
            <button
              type="button"
              className={styles.pill}
              disabled={installing}
              data-testid={`engine-install-${engine.id}`}
              onClick={() => void install()}
            >
              {installing ? t("home.engines.installing") : t("home.engines.installOnServer")}
            </button>
          ) : null}
          {needsSignIn ? (
            <button
              type="button"
              className={styles.pill}
              data-testid={`engine-signin-${engine.id}`}
              onClick={() => setSigningIn(true)}
            >
              {t("settingsUi.serverCli.signIn")}
            </button>
          ) : null}
          <button
            type="button"
            className={styles.detailsBtn}
            aria-expanded={open}
            aria-controls={open ? detailsId : undefined}
            aria-label={`${t("settingsModal.ai.detected.detailsToggle")}: ${engine.name}`}
            title={`${t("settingsModal.ai.detected.detailsToggle")}: ${engine.name}`}
            onClick={onToggle}
          >
            <ChevronDown size={14} aria-hidden="true" className={open ? styles.chevOpen : undefined} />
          </button>
        </div>
      </div>
      {open ? (
        <dl className={styles.cliDetails} id={detailsId}>
          {installed && engine.path ? (
            <>
              <dt>{t("settingsModal.ai.detected.pathLabel")}</dt>
              <dd className={styles.cliPath} data-testid={`engine-path-${engine.id}`}>{engine.path}</dd>
            </>
          ) : null}
          {!installed ? (
            <>
              <dt>{t("settingsModal.ai.detected.installLabel")}</dt>
              <dd className={styles.cliStep}>
                {engine.install_command ? (
                  <>
                    <span className={styles.sub}>
                      {environment?.user ? t("home.engines.runAsUser", { user: environment.user }) : t("home.engines.runOnServer")}
                    </span>
                    <CopyBlock text={stepCommand(environment, "install", engine)} inApp={engine.install_command} />
                  </>
                ) : (
                  <a className={styles.sub} href={engine.docs_url} target="_blank" rel="noreferrer" data-testid={`engine-docs-${engine.id}`}>
                    {t("home.engines.officialPage", { name: engine.name })}
                  </a>
                )}
                <InstallFeedback state={setup} />
              </dd>
            </>
          ) : null}
          {needsSignIn ? (
            <>
              <dt>{t("settingsModal.ai.detected.loginLabel")}</dt>
              <dd className={styles.cliStep}>
                <span className={styles.sub}>
                  {environment?.user ? t("home.engines.signInAsUser", { user: environment.user }) : t("home.engines.signIn")}
                </span>
                <CopyBlock text={stepCommand(environment, "login", engine)} inApp={engine.login} />
                <span className={styles.sub}>{t("home.engines.signInNote")}</span>
              </dd>
            </>
          ) : null}
          {installed && engine.can_install ? (
            <>
              <dt>{t("settingsUi.serverCli.repair")}</dt>
              <dd className={styles.cliStep}>
                <button
                  type="button"
                  className={styles.textBtn}
                  disabled={installing}
                  data-testid={`engine-reinstall-${engine.id}`}
                  onClick={() => void install()}
                >
                  {installing ? t("home.engines.installing") : t("home.engines.reinstall")}
                </button>
                <InstallFeedback state={setup} />
              </dd>
            </>
          ) : null}
          {engine.docs_url ? (
            <>
              <dt>{t("settingsModal.ai.detected.docs")}</dt>
              <dd>
                <a className={styles.textBtn} href={engine.docs_url} target="_blank" rel="noreferrer">
                  {engine.docs_url.replace(/^https?:\/\//, "")}
                </a>
              </dd>
            </>
          ) : null}
        </dl>
      ) : null}
      {needsSignIn ? (
        <ServerSignInModal
          open={signingIn}
          engine={engine}
          serverName={serverName}
          onClose={() => setSigningIn(false)}
          onDone={() => {
            setSigningIn(false);
            onChanged();
          }}
        />
      ) : null}
    </li>
  );
}

/**
 * The server's coding CLIs, as installed THERE: one row per CLI, its install and sign-in steps in
 * its own details. What still needs the owner shows on top only when something is missing.
 */
function HostEnginesBlock() {
  const { t } = useTranslation();
  const { name, state, problem, engines, environment, reload } = useHostAdmin();
  const [openId, setOpenId] = useState<string | null>(null);
  const who = name || t("computerPicker.host");
  return (
    <section className={styles.sec} aria-labelledby="home-engines-h" data-testid="host-engines">
      <div className={styles.head}>
        <h3 id="home-engines-h">{t("home.engines.title", { name: who })}</h3>
        <span className={styles.grow} />
        <button type="button" className={styles.textBtn} onClick={reload}>
          <RefreshCw size={14} aria-hidden="true" />
          {t("settingsModal.ai.detected.refresh")}
        </button>
      </div>
      <HostAdminAlert name={who} state={state} problem={problem} />
      <div className={styles.card}>
        {engines === null ? (
          <p className={styles.hint}>{t("home.engines.loading")}</p>
        ) : engines.length === 0 ? (
          <p className={styles.hint}>{t("home.engines.none")}</p>
        ) : (
          <ul className={styles.list}>
            {engines.map((e) => (
              <HostEngineRow
                key={e.id}
                engine={e}
                environment={environment}
                open={openId === e.id}
                onToggle={() => setOpenId(openId === e.id ? null : e.id)}
                onChanged={reload}
              />
            ))}
          </ul>
        )}
      </div>
      <MoreInfo testId="host-engines-more">
        <p>{t("home.engines.note", { name: who })}</p>
        <p>{t("home.admin.byDesign", { name: who })}</p>
        {environment?.user ? (
          <p data-testid="host-engines-user">
            {t(environment.is_root ? "home.engines.runsAsRoot" : "home.engines.runsAs", {
              name: who,
              user: environment.user,
              home: environment.home ?? "?"
            })}
          </p>
        ) : null}
      </MoreInfo>
    </section>
  );
}

/** "Found on this computer" locally; the server's own coding CLIs first when the home is a host. */
export function EnginesCard() {
  const onHome = useActiveHost().isHost;
  // Even when the bots live on a server, this computer's own CLIs are listed too: sign in and
  // use them on Windows (bots that run "on this computer" use them). Each list is clearly titled,
  // and every CLI's steps live in its own row.
  return onHome ? (
    <>
      <HostEnginesBlock />
      <DetectedAccountsBlock local />
    </>
  ) : (
    <DetectedAccountsBlock />
  );
}
