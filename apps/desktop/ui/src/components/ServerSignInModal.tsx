import { Check, Copy, ExternalLink } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { ApiClient } from "../api/client";
import type { AdminCliEngine, AdminCliLogin } from "../api/types";
import { humanError } from "../lib/humanError";
import { openExternal } from "../lib/externalLink";
import { signInPhase, apiKeyProvider, needsManualFallback, pageAddress, waitingFor, type SignInPhase } from "../lib/serverSignIn";
import { useDaemon } from "../state/DaemonContext";
import { Modal } from "./Modal";
import { RunOnServer } from "./RunOnServer";
import styles from "./ServerSignInModal.module.css";

const ACTIVE = new Set<SignInPhase>(["starting", "needs_browser", "needs_code", "verifying"]);

/**
 * "Sign in on <server>": the engine's own login runs ON THE SERVER (host-admin routes), the page
 * to open and the code to type (or the code to paste back) come through here. Credentials are
 * never copied from this computer; a pasted code only goes to the waiting server process.
 */
export function ServerSignInModal({
  open,
  engine,
  client,
  serverName,
  onClose,
  onDone
}: {
  open: boolean;
  engine: AdminCliEngine;
  /** The server's own channel; never this computer's daemon (it would sign in on the wrong machine). */
  client: ApiClient | null;
  serverName: string;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation();
  return (
    <Modal open={open} onClose={onClose} size="sm" label={t("serverSignIn.title", { name: engine.name, server: serverName })}>
      {open ? <Content engine={engine} client={client} serverName={serverName} onClose={onClose} onDone={onDone} /> : null}
    </Modal>
  );
}

function Content({
  engine,
  client,
  serverName,
  onClose,
  onDone
}: {
  engine: AdminCliEngine;
  client: ApiClient | null;
  serverName: string;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation();
  const { refreshHome } = useDaemon();
  const [job, setJob] = useState<AdminCliLogin | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [manual, setManual] = useState<string | null>(null);
  const [pasted, setPasted] = useState("");
  const [sending, setSending] = useState(false);
  const [copied, setCopied] = useState(false);
  const [keyOpen, setKeyOpen] = useState(false);
  const [apiKey, setApiKey] = useState("");
  const [keySaved, setKeySaved] = useState(false);
  const started = useRef(false);
  const phase = signInPhase(job);
  const jobId = job?.job_id ?? null;

  const start = useCallback(async () => {
    if (!client) {
      return;
    }
    setProblem(null);
    setManual(null);
    setPasted("");
    try {
      setJob(await client.adminCliEngineLogin(engine.id));
    } catch (e) {
      const h = humanError(e);
      if (h.code === "login_manual") {
        setManual(engine.login);
      } else {
        setProblem(h.hint ? `${h.title} ${h.hint}` : h.title);
      }
    }
  }, [client, engine.id, engine.login]);

  useEffect(() => {
    if (started.current) {
      return;
    }
    started.current = true;
    void start();
  }, [start]);

  // Live status while the sign-in is in progress.
  useEffect(() => {
    if (!client || !jobId || !ACTIVE.has(phase)) {
      return undefined;
    }
    const timer = window.setInterval(() => {
      void client.adminCliEngineLoginStatus(engine.id, jobId).then(setJob, () => {});
    }, 1500);
    return () => window.clearInterval(timer);
  }, [client, engine.id, phase, jobId]);

  // Signed in: the engine flips to Ready and its models appear without a reload.
  useEffect(() => {
    if (phase === "done") {
      void refreshHome();
      onDone();
    }
  }, [phase, refreshHome, onDone]);

  // Closing while a sign-in runs stops it (the server kills the whole process tree).
  const close = () => {
    if (client && jobId && ACTIVE.has(phase)) {
      void client.adminCliEngineLoginCancel(engine.id, jobId).catch(() => {});
    }
    onClose();
  };

  const sendCode = async () => {
    if (!client || !jobId || sending) {
      return;
    }
    setSending(true);
    setProblem(null);
    try {
      setJob(await client.adminCliEngineLoginCode(engine.id, jobId, pasted.trim()));
      setPasted("");
    } catch (e) {
      setProblem(humanError(e).title);
    } finally {
      setSending(false);
    }
  };

  const copy = (text: string) => {
    void navigator.clipboard
      ?.writeText(text)
      .then(() => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1500);
      })
      .catch(() => {});
  };

  const keyProvider = apiKeyProvider(engine.id);
  const saveKey = async () => {
    if (!client || !keyProvider || !apiKey.trim()) {
      return;
    }
    setProblem(null);
    try {
      await client.adminPutProvider(keyProvider, { api_key: apiKey.trim() });
      setApiKey("");
      setKeySaved(true);
      void refreshHome();
    } catch (e) {
      setProblem(humanError(e).title);
    }
  };

  const url = job?.url ?? null;
  // The page the person is about to open, as the browser reads it. Nothing that is not https is
  // offered, whatever the server sent.
  const address = pageAddress(url);
  // The CLI showed no sign-in page (its prompt was not the expected one): offer the plain command.
  const startFailed = needsManualFallback(job);
  return (
    <div className={styles.root} data-testid="server-signin" data-phase={phase}>
      <header>
        <h2 className={styles.title}>{t("serverSignIn.title", { name: engine.name, server: serverName })}</h2>
        <p className={styles.lead}>{t("serverSignIn.lead", { server: serverName })}</p>
      </header>

      {manual || startFailed ? (
        <div className={styles.notice} data-testid="signin-manual">
          <p>
            {t(startFailed ? "serverSignIn.manual.fallback" : engine.id === "opencode" ? "serverSignIn.manual.opencode" : "serverSignIn.manual.generic", {
              name: engine.name,
              server: serverName
            })}
          </p>
          <code className={styles.command}>{manual ?? engine.login}</code>
          <RunOnServer command={manual ?? engine.login} beforeOpen={onClose} />
        </div>
      ) : null}

      {phase === "starting" ? <p role="status">{t("serverSignIn.starting", { server: serverName })}</p> : null}

      {phase === "needs_browser" || phase === "needs_code" ? (
        <ol className={styles.steps}>
          <li>
            <b>{t("serverSignIn.step1")}</b>
            <div className={styles.row}>
              <Button variant="primary" size="sm" data-testid="signin-open" disabled={!address} onClick={() => url && address && openExternal(url)}>
                <ExternalLink aria-hidden="true" />
                {t("serverSignIn.openPage")}
              </Button>
              <Button variant="ghost" size="sm" data-testid="signin-copy" disabled={!address} onClick={() => url && address && copy(url)}>
                {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
                {copied ? t("errors.copied") : t("serverSignIn.copyLink")}
              </Button>
            </div>
            {address ? (
              <code className={styles.address} data-testid="signin-address" data-host={address.host} title={url ?? undefined}>
                <b>{address.host}</b>
                {address.shown.slice(address.host.length)}
              </code>
            ) : null}
            {job?.code ? (
              <div className={styles.codeBox}>
                <span className={styles.codeLabel}>{t("serverSignIn.codeLabel")}</span>
                <span className={styles.code} data-testid="signin-code">{job.code}</span>
                <span className={styles.hint}>{t("serverSignIn.typeCode")}</span>
              </div>
            ) : null}
          </li>
          {phase === "needs_code" ? (
            <li>
              <b>{t("serverSignIn.step2")}</b>
              <div className={styles.row}>
                <input
                  className={styles.field}
                  type="text"
                  inputMode="text"
                  autoComplete="off"
                  spellCheck={false}
                  value={pasted}
                  data-testid="signin-paste"
                  placeholder={t("serverSignIn.pastePlaceholder")}
                  aria-label={t("serverSignIn.pasteLabel")}
                  onChange={(e) => setPasted(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      void sendCode();
                    }
                  }}
                />
                <Button variant="primary" size="sm" disabled={sending || pasted.trim().length < 8} data-testid="signin-send" onClick={() => void sendCode()}>
                  {t("serverSignIn.send")}
                </Button>
              </div>
            </li>
          ) : (
            <li className={styles.hint} role="status">{t(waitingFor(job) === "device" ? "serverSignIn.waitingDevice" : "serverSignIn.waitingBrowser")}</li>
          )}
        </ol>
      ) : null}

      {phase === "verifying" ? <p role="status" data-testid="signin-verifying">{t("serverSignIn.verifying")}</p> : null}

      {phase === "done" ? (
        <p role="status" data-testid="signin-done">
          <Check aria-hidden="true" />{" "}
          {job?.account
            ? t("serverSignIn.doneAs", { name: engine.name, server: serverName, account: job.account })
            : t("serverSignIn.done", { name: engine.name, server: serverName })}
        </p>
      ) : null}

      {phase === "failed" || phase === "timed_out" || phase === "cancelled" ? (
        <div className={styles.notice} role="alert" data-testid="signin-failed">
          <p>
            {t(`serverSignIn.error.${phase === "failed" ? (job?.error ?? "login_failed") : phase}`, {
              defaultValue: t("serverSignIn.error.login_failed"),
              name: engine.name
            })}
          </p>
          <Button variant="secondary" size="sm" data-testid="signin-retry" onClick={() => void start()}>
            {t("serverSignIn.retry")}
          </Button>
        </div>
      ) : null}

      {problem ? <p role="alert" className={styles.problem}>{problem}</p> : null}

      {keyProvider ? (
        <div className={styles.alt}>
          {keySaved ? (
            <p role="status">{t("serverSignIn.apiKey.saved", { server: serverName })}</p>
          ) : keyOpen ? (
            <div className={styles.row}>
              <input
                className={styles.field}
                type="password"
                autoComplete="off"
                spellCheck={false}
                value={apiKey}
                data-testid="signin-apikey"
                placeholder={t("serverSignIn.apiKey.placeholder")}
                aria-label={t("serverSignIn.apiKey.label", { provider: keyProvider })}
                onChange={(e) => setApiKey(e.target.value)}
              />
              <Button variant="secondary" size="sm" disabled={!apiKey.trim()} data-testid="signin-apikey-save" onClick={() => void saveKey()}>
                {t("serverSignIn.apiKey.save")}
              </Button>
            </div>
          ) : (
            <Button variant="ghost" size="sm" data-testid="signin-apikey-open" onClick={() => setKeyOpen(true)}>
              {t("serverSignIn.apiKey.open")}
            </Button>
          )}
          <p className={styles.hint}>{t("serverSignIn.apiKey.note", { server: serverName })}</p>
        </div>
      ) : null}

      <p className={styles.hint}>{t("serverSignIn.note")}</p>
      <div className={styles.actions}>
        <Button variant="ghost" onClick={close}>
          {phase === "done" ? t("common.close") : t("common.cancel")}
        </Button>
      </div>
    </div>
  );
}
