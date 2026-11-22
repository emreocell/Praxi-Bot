import {
  ArrowLeft,
  Check,
  CircleCheck,
  CircleDashed,
  CircleX,
  Cloud,
  Copy,
  House,
  KeyRound,
  Loader2,
  Monitor,
  Server,
  TriangleAlert
} from "lucide-react";
import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { isMockMode } from "../api/mockMode";
import type { HostSetupJob, SshKeyHint, StartHostSetupInput } from "../api/types";
import {
  afterKeyAnswer,
  copyDetails,
  isActive,
  STAGES,
  stageStates,
  wizardView
} from "../lib/hostSetup";
import {
  checkForm,
  checklist,
  diagnose,
  initialWizard,
  logLines,
  ORACLE_STEPS,
  progressPercent,
  stepNumber,
  toInput,
  wizardReducer,
  type CheckState,
  type WizardState,
  type FieldProblem
} from "../lib/serverWizard";
import { useDaemon } from "../state/DaemonContext";
import styles from "./AddServerModal.module.css";
import dialog from "./Dialog.module.css";
import { Callout, DialogFooter, DialogHeader, Field } from "./DialogParts";
import form from "./Form.module.css";
import { Modal } from "./Modal";
import { useToast } from "./Toast";
import { humanLine } from "../lib/humanError";

const POLL_MS = 800;

/** The running job survives this dialog being rebuilt (the app re-renders around it). */
let resumeJobId: string | null = null;

/**
 * "Add server": connect a rented server over SSH and pair it, with no ticket to
 * copy. The SSH work runs in this computer's daemon; this modal only collects the
 * address, shows honest progress and asks the one question that must be a human's
 * (is this the right server? the host key fingerprint on first use).
 */
export function AddServerModal({
  open,
  onClose,
  onManual
}: {
  open: boolean;
  onClose: () => void;
  /** Open the old "paste a ticket" dialog instead. */
  onManual: () => void;
}) {
  const { t } = useTranslation();
  return (
    <Modal open={open} onClose={onClose} size="md" label={t("addServer.title")}>
      {open ? <Content onClose={onClose} onManual={onManual} /> : null}
    </Modal>
  );
}

function Content({ onClose, onManual }: { onClose: () => void; onManual: () => void }) {
  const { t } = useTranslation();
  const toast = useToast();
  const { client, localApi, refreshHosts, selectComputer } = useDaemon();
  // The wizard always talks to THIS computer's daemon, never to a selected host.
  const api = isMockMode() ? client : localApi;

  const [w, dispatch] = useReducer(wizardReducer, initialWizard);
  const [keys, setKeys] = useState<SshKeyHint[]>([]);
  const [job, setJob] = useState<HostSetupJob | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [touched, setTouched] = useState(false);
  const input = useRef<StartHostSetupInput | null>(null);
  const jobId = job?.id ?? null;
  const active = job ? isActive(job) : false;
  resumeJobId = jobId && active ? jobId : jobId ? null : resumeJobId;

  // Whatever happens to the job, the form is never left in "starting".
  useEffect(() => {
    if (job) setBusy(false);
  }, [job]);

  // Picked up again after a rebuild: show the job that is still waiting or running.
  useEffect(() => {
    const id = resumeJobId;
    if (!api || !id) return undefined;
    let live = true;
    api
      .getHostSetup(id)
      .then((j) => {
        if (live) {
          setJob(j);
          dispatch({ type: "run" });
        }
      })
      .catch(() => {
        resumeJobId = null;
      });
    return () => {
      live = false;
    };
  }, [api]);

  useEffect(() => {
    let live = true;
    api
      ?.listSshKeys()
      .then((k) => {
        if (live) {
          setKeys(k);
          if (k[0]) dispatch({ type: "set", field: "keyPath", value: k[0].path });
        }
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [api]);

  // Poll while the job is active.
  useEffect(() => {
    if (!api || !jobId || !active) {
      return undefined;
    }
    let live = true;
    const id = window.setInterval(() => {
      api
        .getHostSetup(jobId)
        .then((j) => {
          if (live) setJob(j);
        })
        .catch(() => undefined);
    }, POLL_MS);
    return () => {
      live = false;
      window.clearInterval(id);
    };
  }, [api, jobId, active]);

  // A finished setup: the new server shows up in the computer list.
  const succeeded = job?.state === "succeeded";
  useEffect(() => {
    if (succeeded) void refreshHosts();
  }, [succeeded, refreshHosts]);

  const start = useCallback(
    async (body: StartHostSetupInput) => {
      if (!api) {
        setError(t("addServer.noDaemon"));
        return;
      }
      setBusy(true);
      setError(null);
      try {
        input.current = body;
        setJob(await api.startHostSetup(body));
        dispatch({ type: "run" });
      } catch (e) {
        setError(humanLine(e));
      } finally {
        setBusy(false);
      }
    },
    [api, t]
  );

  const check = checkForm(w);
  const submit = () => {
    setTouched(true);
    if (!check.ok) return;
    void start(toInput(w));
  };

  const answerKey = (accept: boolean) => {
    if (!api || !jobId) return;
    setError(null);
    // Show the answer at once; the poll confirms it.
    setJob((j) => (j ? afterKeyAnswer(j, accept) : j));
    api
      .answerHostSetupKey(jobId, accept)
      .then(setJob)
      .catch(() => {
        setError(t("addServer.answerFailed"));
        api
          .getHostSetup(jobId)
          .then(setJob)
          .catch(() => setJob(null));
      });
  };

  const cancelAndClose = () => {
    resumeJobId = null;
    if (api && jobId && active) {
      void api.cancelHostSetup(jobId).catch(() => undefined);
    }
    onClose();
  };

  const view = w.page === "run" ? wizardView(job) : "form";
  const hasScreen = (job?.view_targets ?? 0) > 0;
  const n = stepNumber(w.page);
  const subtitle = job && w.page === "run" ? job.target : n ? t("addServer.stepOf", { n, total: 3 }) : t("addServer.lead");
  const back = (
    <Button variant="ghost" onClick={() => dispatch({ type: "back" })} data-testid="add-server-back">
      <ArrowLeft aria-hidden="true" />
      {t("common.back")}
    </Button>
  );
  const manualLink = (
    <button type="button" className={styles.link} onClick={onManual}>
      {t("addServer.manual")}
    </button>
  );

  return (
    <div className={dialog.root} data-testid="add-server" data-view={view} data-page={w.page}>
      <DialogHeader
        title={t("addServer.title")}
        subtitle={subtitle}
        leading={<Server aria-hidden="true" className={styles.headIcon} />}
        onClose={cancelAndClose}
      />

      {w.page === "intro" ? (
        <>
          <div className={dialog.body}>
            <h3 className={styles.pageTitle}>{t("addServer.intro.title")}</h3>
            <p className={styles.para}>{t("addServer.intro.what")}</p>
            <Callout tone="info" title={t("addServer.intro.optionalTitle")}>
              {t("addServer.intro.optional")}
            </Callout>
          </div>
          <DialogFooter note={manualLink}>
            <Button variant="ghost" onClick={onClose} data-testid="add-server-skip">
              {t("addServer.intro.skip")}
            </Button>
            <Button variant="primary" onClick={() => dispatch({ type: "next" })} data-testid="add-server-next">
              {t("addServer.intro.go")}
            </Button>
          </DialogFooter>
        </>
      ) : null}

      {w.page === "choose" ? (
        <>
          <div className={dialog.body}>
            <h3 className={styles.pageTitle}>{t("addServer.choose.title")}</h3>
            <div className={styles.choices} role="group" aria-label={t("addServer.choose.title")}>
              {(
                [
                  ["ssh", <KeyRound key="k" aria-hidden="true" />],
                  ["oracle", <Cloud key="c" aria-hidden="true" />],
                  ["home", <House key="h" aria-hidden="true" />]
                ] as const
              ).map(([id, icon]) => (
                <button
                  key={id}
                  type="button"
                  className={styles.choice}
                  data-testid={`choose-${id}`}
                  onClick={() => dispatch({ type: "choose", path: id })}
                >
                  <span className={styles.choiceIcon}>{icon}</span>
                  <span className={styles.stageText}>
                    <b>{t(`addServer.choose.${id}`)}</b>
                    <small>{t(`addServer.choose.${id}Hint`)}</small>
                  </span>
                </button>
              ))}
            </div>
          </div>
          <DialogFooter note={manualLink}>{back}</DialogFooter>
        </>
      ) : null}

      {w.page === "oracle" ? (
        <>
          <div className={dialog.body}>
            <h3 className={styles.pageTitle}>{t("addServer.oracle.title")}</h3>
            <p className={styles.para}>{t("addServer.oracle.lead")}</p>
            <ol className={styles.guide}>
              {ORACLE_STEPS.map((id) => {
                const on = w.oracleDone.includes(id);
                return (
                  <li key={id} className={styles.guideItem} data-on={on}>
                    <label className={styles.guideLabel}>
                      <input
                        type="checkbox"
                        checked={on}
                        onChange={() => dispatch({ type: "tick", id })}
                        data-testid={`oracle-${id}`}
                      />
                      <span className={styles.stageText}>
                        <b>{t(`addServer.oracle.step.${id}.title`)}</b>
                        <small>{t(`addServer.oracle.step.${id}.body`)}</small>
                      </span>
                    </label>
                  </li>
                );
              })}
            </ol>
            <Callout tone="info">{t("addServer.oracle.privacy")}</Callout>
          </div>
          <DialogFooter>
            {back}
            <Button variant="primary" onClick={() => dispatch({ type: "next" })} data-testid="add-server-next">
              {w.oracleDone.length === ORACLE_STEPS.length ? t("addServer.oracle.ready") : t("addServer.oracle.haveIt")}
            </Button>
          </DialogFooter>
        </>
      ) : null}

      {w.page === "home" ? (
        <>
          <div className={dialog.body}>
            <h3 className={styles.pageTitle}>{t("addServer.home.title")}</h3>
            <p className={styles.para}>{t("addServer.home.lead")}</p>
            <ol className={styles.numbered}>
              {(["one", "two", "three"] as const).map((k) => (
                <li key={k}>{t(`addServer.home.${k}`)}</li>
              ))}
            </ol>
          </div>
          <DialogFooter>
            {back}
            <Button variant="primary" onClick={() => dispatch({ type: "choose", path: "ssh" })}>
              {t("addServer.home.go")}
            </Button>
          </DialogFooter>
        </>
      ) : null}

      {w.page === "details" ? (
        <>
          <div className={dialog.body}>
            <div className={styles.row}>
              <Field
                label={t("addServer.host")}
                htmlFor="as-host"
                hint={t("addServer.hostHint")}
                error={touched || w.host ? problemText(t, check.host) : undefined}
              >
                <input
                  id="as-host"
                  className={form.input}
                  value={w.host}
                  placeholder="203.0.113.7"
                  autoComplete="off"
                  spellCheck={false}
                  data-autofocus
                  data-testid="add-server-host"
                  aria-invalid={check.host && (touched || w.host) ? true : undefined}
                  onChange={(e) => dispatch({ type: "set", field: "host", value: e.target.value })}
                />
              </Field>
              <Field
                label={t("addServer.user")}
                htmlFor="as-user"
                hint={t("addServer.userHint")}
                error={problemText(t, check.user)}
              >
                <input
                  id="as-user"
                  className={form.input}
                  value={w.user}
                  autoComplete="off"
                  spellCheck={false}
                  onChange={(e) => dispatch({ type: "set", field: "user", value: e.target.value })}
                />
              </Field>
            </div>
            <Field label={t("addServer.auth")}>
              <div className={styles.seg} role="radiogroup" aria-label={t("addServer.auth")}>
                {(["key", "agent"] as const).map((a) => (
                  <button
                    key={a}
                    type="button"
                    role="radio"
                    aria-checked={w.auth === a}
                    className={styles.segItem}
                    data-on={w.auth === a}
                    onClick={() => dispatch({ type: "auth", auth: a })}
                  >
                    {t(`addServer.auth_${a}`)}
                  </button>
                ))}
              </div>
            </Field>
            {w.auth === "key" ? (
              <Field
                label={t("addServer.keyFile")}
                htmlFor="as-key"
                hint={t("addServer.keyHint")}
                error={touched || w.keyPath ? problemText(t, check.key) : undefined}
              >
                {keys.length > 0 ? (
                  <div className={styles.keys} role="radiogroup" aria-label={t("addServer.keyFile")}>
                    {keys.map((k) => (
                      <button
                        key={k.path}
                        type="button"
                        role="radio"
                        aria-checked={w.keyPath === k.path}
                        className={styles.keyItem}
                        data-on={w.keyPath === k.path}
                        title={k.path}
                        onClick={() => dispatch({ type: "set", field: "keyPath", value: k.path })}
                      >
                        {w.keyPath === k.path ? <Check aria-hidden="true" /> : null}
                        {k.name}
                      </button>
                    ))}
                  </div>
                ) : (
                  <p className={styles.note}>{t("addServer.noKeys")}</p>
                )}
                <input
                  id="as-key"
                  className={`${form.input} ${styles.mono}`}
                  value={w.keyPath}
                  placeholder="~/.ssh/id_ed25519"
                  autoComplete="off"
                  spellCheck={false}
                  onChange={(e) => dispatch({ type: "set", field: "keyPath", value: e.target.value })}
                />
              </Field>
            ) : (
              <Callout tone="info">{t("addServer.agentHint")}</Callout>
            )}
            <Callout tone="info">{t("addServer.passwordNote")}</Callout>
            <Callout tone="info">{t("addServer.releaseNote")}</Callout>
            <details className={styles.advanced}>
              <summary>{t("addServer.advanced")}</summary>
              <Field
                label={t("addServer.port")}
                htmlFor="as-port"
                hint={t("addServer.portHint")}
                error={problemText(t, check.port)}
              >
                <input
                  id="as-port"
                  className={form.input}
                  value={w.port}
                  inputMode="numeric"
                  autoComplete="off"
                  onChange={(e) => dispatch({ type: "set", field: "port", value: e.target.value })}
                />
              </Field>
            </details>
            {error ? (
              <p className={form.error} role="alert">
                {error}
              </p>
            ) : null}
          </div>
          <DialogFooter note={manualLink}>
            {back}
            <Button variant="primary" disabled={busy} onClick={submit} data-testid="add-server-start">
              {busy ? t("addServer.starting") : t("addServer.connect")}
            </Button>
          </DialogFooter>
        </>
      ) : null}

      {w.page === "run" && job ? (
        <>
          <div className={dialog.body}>
            {view === "progress" || view === "failed" ? <Checklist job={job} /> : null}
            {view === "progress" ? <Bar job={job} /> : null}
            {job.state === "awaiting_host_key" && job.host_key ? (
              <div className={styles.keyCard} role="group" aria-label={t("addServer.hostKeyTitle")}>
                <p className={styles.keyTitle}>{t("addServer.hostKeyTitle")}</p>
                <p className={styles.keyText}>{t("addServer.hostKeyBody", { host: job.host_key.host })}</p>
                <code className={styles.fingerprint} data-testid="host-key-fingerprint">
                  {job.host_key.fingerprint}
                </code>
                <div className={styles.keyActions}>
                  <Button variant="ghost" onClick={() => answerKey(false)}>
                    {t("addServer.hostKeyNo")}
                  </Button>
                  <Button variant="primary" onClick={() => answerKey(true)} data-testid="host-key-trust">
                    {t("addServer.hostKeyYes")}
                  </Button>
                </div>
              </div>
            ) : null}
            {error ? (
              <p className={form.error} role="alert">
                {error}
              </p>
            ) : null}
            {view === "failed" ? <Failure job={job} typed={w} /> : null}
            {view === "done" ? (
              <Success job={job} hasScreen={hasScreen} />
            ) : (
              <details className={styles.advanced} data-testid="add-server-log">
                <summary>{t("addServer.showLog")}</summary>
                <pre className={styles.log}>{logLines(job).join("\n") || t("addServer.logEmpty")}</pre>
              </details>
            )}
          </div>
          <DialogFooter note={view === "progress" && job.state === "running" ? t("addServer.cancelNote") : undefined}>
            {view === "progress" ? (
              <Button variant="ghost" onClick={cancelAndClose} data-testid="add-server-cancel">
                {t("common.cancel")}
              </Button>
            ) : null}
            {view === "failed" ? (
              <>
                <Button
                  variant="ghost"
                  onClick={() => {
                    void navigator.clipboard
                      ?.writeText(copyDetails(job))
                      .then(() => toast.show(t("addServer.copied"), "success"))
                      .catch(() => toast.show(t("addServer.copyFailed"), "danger"));
                  }}
                  data-testid="add-server-copy"
                >
                  <Copy aria-hidden="true" />
                  {t("addServer.copy")}
                </Button>
                <Button
                  variant="ghost"
                  onClick={() => {
                    setJob(null);
                    dispatch({ type: "edit" });
                  }}
                >
                  {t("addServer.edit")}
                </Button>
                <Button
                  variant="primary"
                  disabled={busy || !input.current}
                  onClick={() => input.current && void start(input.current)}
                  data-testid="add-server-retry"
                >
                  {t("addServer.retryResume")}
                </Button>
              </>
            ) : null}
            {view === "done" ? (
              <>
                <Button variant="ghost" onClick={onClose}>
                  {t("common.close")}
                </Button>
                <Button
                  variant="primary"
                  disabled={!job.host_id}
                  onClick={() => {
                    if (job.host_id) void selectComputer(job.host_id);
                    onClose();
                  }}
                  data-testid="add-server-use"
                >
                  {hasScreen ? <Monitor aria-hidden="true" /> : <Server aria-hidden="true" />}
                  {t("addServer.sayHello")}
                </Button>
              </>
            ) : null}
          </DialogFooter>
        </>
      ) : null}
    </div>
  );
}

type TFn = (key: string) => string;

function problemText(t: TFn, p: FieldProblem | null): string | undefined {
  return p ? t(`addServer.problem.${p}`) : undefined;
}

function CheckIcon({ state }: { state: CheckState }) {
  switch (state) {
    case "running":
      return <Loader2 aria-hidden="true" className={styles.spin} />;
    case "pass":
      return <CircleCheck aria-hidden="true" />;
    case "warn":
      return <TriangleAlert aria-hidden="true" />;
    case "fail":
      return <CircleX aria-hidden="true" />;
    default:
      return <CircleDashed aria-hidden="true" />;
  }
}

/** The friendly pre-flight list: one row per question, with the fix text under a problem. */
function Checklist({ job }: { job: HostSetupJob }) {
  const { t } = useTranslation();
  return (
    <ol className={styles.stages} aria-label={t("addServer.checks.title")} data-testid="add-server-checks">
      {checklist(job).map(({ id, state }) => (
        <li key={id} className={styles.stage} data-state={state === "pass" ? "done" : state === "pending" ? "pending" : state} data-testid={`check-${id}`}>
          <span className={styles.stageIcon}>
            <CheckIcon state={state} />
          </span>
          <span className={styles.stageText}>
            <b>{t(`addServer.checks.${id}.title`)}</b>
            <small>{t(state === "fail" || state === "warn" ? `addServer.checks.${id}.fix` : `addServer.checks.${id}.${state}`)}</small>
          </span>
        </li>
      ))}
    </ol>
  );
}

/** A real progress bar over the five engine rows. */
function Bar({ job }: { job: HostSetupJob }) {
  const { t } = useTranslation();
  const pct = progressPercent(job);
  const states = stageStates(job);
  const current = STAGES.find((s) => states[s] === "running" || states[s] === "waiting");
  return (
    <div className={styles.barBox}>
      <div
        className={styles.bar}
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={pct}
        aria-label={t("addServer.progress")}
        data-testid="add-server-bar"
      >
        <span className={styles.barFill} style={{ inlineSize: `${pct}%` }} />
      </div>
      <p className={styles.note}>
        {pct}% · {current ? t(`addServer.stage.${current}`) : t("addServer.state.pending")}
      </p>
    </div>
  );
}

function Success({ job, hasScreen }: { job: HostSetupJob; hasScreen: boolean }) {
  const { t } = useTranslation();
  return (
    <>
      <Callout tone="success" title={t("addServer.doneTitle", { name: job.host_name ?? job.target })}>
        {hasScreen ? t("addServer.doneScreen") : t("addServer.doneNoScreen")}
      </Callout>
      <dl className={styles.facts}>
        {(["where", "open", "stop"] as const).map((k) => (
          <div key={k}>
            <dt>{t(`addServer.done.${k}.title`)}</dt>
            <dd>{t(`addServer.done.${k}.body`)}</dd>
          </div>
        ))}
      </dl>
    </>
  );
}

function Failure({ job, typed }: { job: HostSetupJob; typed: WizardState }) {
  const { t } = useTranslation();
  const d = diagnose(job, typed);
  if (!d) {
    return (
      <Callout tone="warning" title={t("addServer.cancelled")} role="status">
        {t("addServer.cancelledBody")}
      </Callout>
    );
  }
  return (
    <Callout tone="danger" title={t(`addServer.fail.${d.code}.msg`)} role="alert">
      {t(`addServer.fail.${d.code}.fix`)}
    </Callout>
  );
}
