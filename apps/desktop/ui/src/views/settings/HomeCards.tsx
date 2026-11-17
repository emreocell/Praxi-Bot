import { Check, Copy, Info, Server } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { AdminCliEngine, AdminCliInstall, AdminState } from "../../api/types";
import { humanError } from "../../lib/humanError";
import { useDaemon } from "../../state/DaemonContext";
import { RunOnServer } from "../../components/RunOnServer";
import { ServerSignInModal } from "../../components/ServerSignInModal";
import { DetectedAccountsBlock } from "./DetectedAccountsBlock";
import styles from "./AiPage.module.css";

/** A command to run on the server, one click to copy. */
export function CopyBlock({ text }: { text: string }) {
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
      <RunOnServer command={text} className={styles.textBtn} />
    </span>
  );
}

function useHostAdmin() {
  const { client, home, workLocalNow, activeComputerId } = useDaemon();
  const onHome = home.kind === "host" && !workLocalNow && activeComputerId === home.hostId;
  const [state, setState] = useState<AdminState | null>(null);
  const [engines, setEngines] = useState<AdminCliEngine[] | null>(null);
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
    return () => {
      live = false;
    };
  }, [onHome, client, tick]);
  return { onHome, name: home.kind === "host" ? home.name : "", state, engines, problem, reload };
}

/**
 * What the server still needs from its owner before this desktop can set it up, and what it keeps
 * to itself by design. Shown only while the home is a host.
 */
export function HostAdminNote() {
  const { t } = useTranslation();
  const { onHome, name, state, problem } = useHostAdmin();
  if (!onHome) {
    return null;
  }
  const who = name || t("computerPicker.host");
  return (
    <section className={styles.sec} data-testid="host-admin-note">
      <div className={`${styles.card} ${styles.item}`}>
        <Server size={18} aria-hidden="true" />
        <div className={styles.text}>
          <span>{t("home.admin.title")}</span>
          <span className={styles.sub}>{t("home.admin.byDesign", { name: who })}</span>
          {problem ? <span className={`${styles.sub} ${styles.subWarn}`}>{problem}</span> : null}
          {state && !state.granted ? (
            <>
              <span className={`${styles.sub} ${styles.subWarn}`}>{t("home.admin.notGranted", { name: who })}</span>
              {state.confirm_command ? <CopyBlock text={state.confirm_command} /> : null}
            </>
          ) : null}
          {state && !state.secrets_available ? (
            <>
              <span className={`${styles.sub} ${styles.subWarn}`}>{t("home.admin.noSecrets", { name: who })}</span>
              {state.secrets_hint ? <span className={styles.sub}>{state.secrets_hint}</span> : null}
            </>
          ) : null}
        </div>
      </div>
    </section>
  );
}

/**
 * Install and sign-in steps of one coding CLI on the server. The command is the registry's, shown
 * for copying; where the owner's desktop may administer the server a button runs it THERE. A row
 * never shows an empty command: without a verified installer it points to the official page.
 */
export function EngineSetup({ engine, onChanged }: { engine: AdminCliEngine; onChanged?: () => void }) {
  const { t } = useTranslation();
  const { client, home } = useDaemon();
  const serverName = home.kind === "host" ? home.name : t("computerPicker.host");
  const [signingIn, setSigningIn] = useState(false);
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
  const installed = engine.installed || run?.installed === true;
  const signedIn = run?.signed_in ?? engine.signed_in;
  return (
    <>
      {!installed ? (
        <>
          {engine.install_command ? (
            <>
              <span className={styles.sub}>{t("home.engines.runOnServer")}</span>
              <CopyBlock text={engine.install_command} />
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
          <span className={styles.sub}>{t("home.engines.signIn")}</span>
          <CopyBlock text={engine.login} />
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

/** The server's coding CLIs, as installed THERE, with install and sign-in steps for each. */
function HostEnginesBlock() {
  const { t } = useTranslation();
  const { name, engines, reload } = useHostAdmin();
  const who = name || t("computerPicker.host");
  return (
    <section className={styles.sec} aria-labelledby="home-engines-h" data-testid="host-engines">
      <div className={styles.head}>
        <h3 id="home-engines-h">{t("home.engines.title", { name: who })}</h3>
      </div>
      <div className={styles.card}>
        {engines === null ? (
          <p className={styles.hint}>{t("home.engines.loading")}</p>
        ) : engines.length === 0 ? (
          <p className={styles.hint}>{t("home.engines.none")}</p>
        ) : (
          <ul className={styles.list}>
            {engines.map((e) => (
              <li key={e.id} className={styles.item} data-testid={`host-engine-${e.id}`}>
                <div className={styles.text}>
                  <span>{e.name}</span>
                  <span className={styles.sub}>
                    {e.installed
                      ? e.signed_in === true
                        ? t("home.engines.ready")
                        : t("home.engines.installedUnknown")
                      : t("home.engines.notInstalled")}
                    {e.installed && e.signed_in !== false && e.version ? ` · ${e.version}` : ""}
                  </span>
                  <EngineSetup engine={e} onChanged={reload} />
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>
      <p className={styles.hint}>
        <Info size={12} aria-hidden="true" /> {t("home.engines.note", { name: who })}
      </p>
    </section>
  );
}

/** "Found on this computer" locally; the server's own engines when the home is a host. */
export function EnginesCard() {
  const { home, workLocalNow, activeComputerId } = useDaemon();
  const onHome = home.kind === "host" && !workLocalNow && activeComputerId === home.hostId;
  return onHome ? (
    <>
      <HostAdminNote />
      <HostEnginesBlock />
    </>
  ) : (
    <DetectedAccountsBlock />
  );
}
