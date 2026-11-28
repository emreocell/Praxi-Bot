import { Check, Copy, TriangleAlert } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { ApiClient } from "../../api/client";
import { isMockMode } from "../../api/mockMode";
import { useTranslation } from "react-i18next";
import type { AdminCliEngine, AdminCliEnvironment, AdminCliInstall, AdminState } from "../../api/types";
import { humanError } from "../../lib/humanError";
import type { ServerListStatus } from "../../lib/unifiedCli";
import { useDaemon } from "../../state/DaemonContext";
import { useActiveHost } from "../../state/useActiveHost";
import { RunOnServer } from "../../components/RunOnServer";
import { CodingClisBlock } from "./CodingClisBlock";
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

/**
 * The server's administration: its state, its coding CLIs and the user it runs as. Every call goes
 * through the server's own channel (`api`): the active client while a server is selected, else a
 * channel of its own. With no channel there is no `api`, and nothing is ever sent to this
 * computer's daemon instead: it would answer for the wrong machine.
 */
export function useHostAdmin() {
  const { client, home, hostConn, workLocalNow, hostError } = useDaemon();
  const active = useActiveHost();
  const onHome = home.kind === "host" || active.isHost;
  const [own, setOwn] = useState<ApiClient | null>(null);
  const ownsChannel = onHome && !active.isHost && hostConn !== null;
  useEffect(() => {
    if (!ownsChannel || !hostConn) {
      return undefined;
    }
    const made = new ApiClient(hostConn);
    setOwn(made);
    return () => {
      made.dispose();
      setOwn(null);
    };
  }, [ownsChannel, hostConn]);
  const api = !onHome ? null : active.isHost ? client : ownsChannel ? own : null;
  const [state, setState] = useState<AdminState | null>(null);
  const [engines, setEngines] = useState<AdminCliEngine[] | null>(null);
  const [listStatus, setListStatus] = useState<ServerListStatus>("loading");
  // Older hosts have no such answer: the lines that name the user are then simply left out.
  const [environment, setEnvironment] = useState<AdminCliEnvironment | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  const reload = useCallback(() => setTick((n) => n + 1), []);
  // Another channel is another server: nothing read from the last one is shown for it.
  useEffect(() => {
    setState(null);
    setEngines(null);
    setEnvironment(null);
    setProblem(null);
    setListStatus("loading");
  }, [api]);
  useEffect(() => {
    if (!api) {
      return undefined;
    }
    let live = true;
    setProblem(null);
    api.adminState().then(
      (s) => {
        if (live) {
          setState(s);
          setProblem(null);
        }
      },
      (e) => live && setProblem(humanError(e).title)
    );
    api.adminCliEngines().then(
      (list) => {
        if (live) {
          setEngines(list);
          setListStatus("ready");
        }
      },
      () => {
        if (live) {
          setEngines(null);
          setListStatus("unreachable");
        }
      }
    );
    api.adminCliEnvironment().then(
      (e) => live && setEnvironment(e),
      () => live && setEnvironment(null)
    );
    return () => {
      live = false;
    };
  }, [api, tick]);
  // No channel: still opening it, or the owner chose to work on this computer / it did not open
  // (the mock has none without `?admin=`).
  const status: ServerListStatus = !onHome ? "loading" : api ? listStatus : workLocalNow || hostError || isMockMode() ? "unreachable" : "loading";
  const name = home.kind === "host" ? home.name : active.name;
  return { onHome, name, api, state, engines, environment, problem, status, reload };
}

/**
 * What the server still needs from its owner before this desktop can set it up. Shown only when
 * something is missing; what the server keeps to itself by design is under "How it works".
 */
export function HostAdminAlert({ name, state, problem }: Pick<ReturnType<typeof useHostAdmin>, "state" | "problem"> & { name: string }) {
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
export function useServerInstall(engine: AdminCliEngine, client: ApiClient | null, onChanged?: () => void) {
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
  // The latest callback, called once per finished run: an unstable one must not re-run the effect.
  const changed = useRef(onChanged);
  changed.current = onChanged;
  useEffect(() => {
    if (finished) {
      changed.current?.();
    }
  }, [finished]);
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
export function InstallFeedback({ state }: { state: ReturnType<typeof useServerInstall> }) {
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

/** Coding CLIs on this computer and on the bots' home server, in one list. */
export function EnginesCard() {
  return <CodingClisBlock />;
}
