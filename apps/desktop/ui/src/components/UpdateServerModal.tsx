import { CircleCheck, CircleDashed, CircleX, Copy, Loader2, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { HostSetupJob } from "../api/types";
import { ApiError } from "../api/client";
import { isMockMode } from "../api/mockMode";
import { humanLine } from "../lib/humanError";
import { type RowState, UPDATE_ROWS, rowStates, updateCommands, updateFailure } from "../lib/serverUpdate";
import { useDaemon } from "../state/DaemonContext";
import addStyles from "./AddServerModal.module.css";
import dialog from "./Dialog.module.css";
import { Callout, DialogFooter, DialogHeader } from "./DialogParts";
import { Modal } from "./Modal";
import styles from "./UpdateServerModal.module.css";
import { useToast } from "./Toast";

const POLL_MS = 1500;

type Phase = "confirm" | "starting" | "running" | "done" | "failed" | "manual";

/**
 * "Update server": says in plain words what happens, runs the same verified upgrade as
 * `bot host upgrade` over the SSH details the "Add server" wizard stored (nothing new is saved),
 * shows live progress and, when it cannot run, hands over the exact commands to paste instead.
 */
export function UpdateServerModal({
  open,
  hostId,
  onClose,
  onDone,
  showCommands = false
}: {
  open: boolean;
  hostId: string;
  onClose: () => void;
  onDone?: () => void;
  showCommands?: boolean;
}) {
  const { t } = useTranslation();
  return (
    <Modal open={open} onClose={onClose} size="md" label={t("updateServer.title")}>
      {open ? <Content hostId={hostId} onClose={onClose} onDone={onDone} showCommands={showCommands} /> : null}
    </Modal>
  );
}

function RowIcon({ state }: { state: RowState }) {
  if (state === "running") return <Loader2 aria-hidden="true" className={addStyles.spin} />;
  if (state === "done") return <CircleCheck aria-hidden="true" />;
  if (state === "failed") return <CircleX aria-hidden="true" />;
  return <CircleDashed aria-hidden="true" />;
}

function logText(job: HostSetupJob | null): string {
  if (!job) return "";
  const lines = job.steps.map((s) => `[${s.status}] ${s.title}: ${s.detail}`);
  return [...lines, ...job.notes].join("\n");
}

function Content({ hostId, onClose, onDone, showCommands }: { hostId: string; onClose: () => void; onDone?: () => void; showCommands: boolean }) {
  const { t } = useTranslation();
  const toast = useToast();
  const { client, localApi, hostList, refreshHosts } = useDaemon();
  const api = isMockMode() ? client : localApi;
  const host = hostList?.hosts.find((h) => h.id === hostId);
  const name = host?.name ?? t("updateServer.thisServer");
  const [phase, setPhase] = useState<Phase>(showCommands ? "manual" : "confirm");
  const [job, setJob] = useState<HostSetupJob | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [noSsh, setNoSsh] = useState(false);
  const timer = useRef<number | null>(null);
  const commands = updateCommands(hostList?.local_release_tag);

  const stopPolling = useCallback(() => {
    if (timer.current !== null) window.clearInterval(timer.current);
    timer.current = null;
  }, []);
  useEffect(() => stopPolling, [stopPolling]);

  const poll = useCallback(
    (id: string) => {
      stopPolling();
      timer.current = window.setInterval(() => {
        if (!api) return;
        void api
          .getHostSetup(id)
          .then((j) => {
            setJob(j);
            if (j.state === "succeeded") {
              stopPolling();
              setPhase("done");
              void refreshHosts();
              onDone?.();
            } else if (j.state === "failed" || j.state === "cancelled") {
              stopPolling();
              setPhase("failed");
            }
          })
          .catch(() => {
            // A restart of this app's own daemon is not the server's problem: keep asking.
          });
      }, POLL_MS);
    },
    [api, onDone, refreshHosts, stopPolling]
  );

  const start = async () => {
    if (!api) return;
    setError(null);
    setPhase("starting");
    try {
      const j = await api.startHostUpdate(hostId);
      setJob(j);
      setPhase("running");
      poll(j.id);
    } catch (e) {
      if (e instanceof ApiError && e.status === 400) {
        setNoSsh(true);
        setPhase("manual");
        return;
      }
      setError(humanLine(e));
      setPhase("confirm");
    }
  };

  const answerKey = async (accept: boolean) => {
    if (!api || !job) return;
    try {
      setJob(await api.answerHostSetupKey(job.id, accept));
    } catch (e) {
      setError(humanLine(e));
    }
  };

  const copy = async () => {
    if (!commands) return;
    try {
      await navigator.clipboard.writeText(commands);
      toast.show(t("updateServer.copied"), "success");
    } catch {
      toast.show(t("updateServer.copyFailed"), "danger");
    }
  };

  const running = phase === "starting" || phase === "running";
  const states = job ? rowStates(job) : null;
  const failure = job ? updateFailure(job) : null;
  const installStarted = job?.steps.some((s) => s.id.startsWith("install")) ?? false;
  const shown = phase !== "confirm" && phase !== "manual";

  return (
    <div className={dialog.root} data-testid="update-server">
      <DialogHeader
        title={t("updateServer.titleFor", { name })}
        subtitle={t("updateServer.lead")}
        onClose={onClose}
        closeDisabled={running}
      />
      <div className={dialog.body}>
        {phase === "confirm" ? (
          <>
            <Callout tone="info" title={t("updateServer.whatHappens")}>
              <ul className={styles.list}>
                <li>{t("updateServer.p1")}</li>
                <li>{t("updateServer.p2")}</li>
                <li>{t("updateServer.p3")}</li>
                <li>{t("updateServer.p4")}</li>
                <li>{t("updateServer.p5")}</li>
              </ul>
            </Callout>
            <p className={addStyles.note}>{t("updateServer.safe")}</p>
            {error ? (
              <p className={dialog.error} role="alert">
                {error}
              </p>
            ) : null}
          </>
        ) : null}

        {shown ? (
          <ol className={addStyles.stages} aria-label={t("updateServer.progress")} data-testid="update-server-rows" aria-live="polite">
            {UPDATE_ROWS.map((row) => {
              const state = states ? states[row] : row === "connect" ? "running" : "pending";
              return (
                <li key={row} className={addStyles.stage} data-state={state} data-testid={`update-row-${row}`}>
                  <span className={addStyles.stageIcon}>
                    <RowIcon state={state} />
                  </span>
                  <span className={addStyles.stageText}>
                    <b>{t(`updateServer.rows.${row}.title`)}</b>
                    <small>{t(`updateServer.rows.${row}.${state}`)}</small>
                  </span>
                </li>
              );
            })}
          </ol>
        ) : null}

        {job?.state === "awaiting_host_key" && job.host_key ? (
          <div className={addStyles.keyCard} role="alert" data-testid="update-host-key">
            <p className={addStyles.keyTitle}>{t("updateServer.hostKeyTitle")}</p>
            <p className={addStyles.keyText}>{t("updateServer.hostKeyBody", { host: job.host_key.host })}</p>
            <code className={addStyles.fingerprint}>{job.host_key.fingerprint}</code>
            <div className={addStyles.keyActions}>
              <Button variant="ghost" onClick={() => void answerKey(false)}>
                {t("updateServer.hostKeyNo")}
              </Button>
              <Button variant="primary" onClick={() => void answerKey(true)}>
                {t("updateServer.hostKeyYes")}
              </Button>
            </div>
          </div>
        ) : null}

        {phase === "done" ? (
          <Callout tone="success" title={t("updateServer.doneTitle", { name })} role="status">
            <p className={addStyles.para}>{t("updateServer.doneBody")}</p>
          </Callout>
        ) : null}

        {phase === "failed" ? (
          failure ? (
            <Callout tone="danger" title={t(`updateServer.fail.${failure}.title`)} role="alert">
              <p className={addStyles.para}>{t(`updateServer.fail.${failure}.body`)}</p>
              <p className={addStyles.para}>{t(`updateServer.fail.${failure}.next`)}</p>
            </Callout>
          ) : (
            <Callout tone="warning" title={t("updateServer.stopped")} role="status" />
          )
        ) : null}

        {phase === "manual" ? (
          <>
            <Callout tone="info" title={noSsh ? t("updateServer.manualNoSsh") : t("updateServer.manualTitle")}>
              <p className={addStyles.para}>{t("updateServer.manualBody")}</p>
            </Callout>
            {commands ? (
              <>
                <pre className={styles.commands} tabIndex={0} aria-label={t("updateServer.commandsLabel")} data-testid="update-commands">
                  {commands}
                </pre>
                <p className={addStyles.note}>{t("updateServer.manualNote")}</p>
              </>
            ) : (
              <p className={addStyles.para}>{t("updateServer.noRelease")}</p>
            )}
          </>
        ) : null}

        {phase === "running" || phase === "failed" || phase === "done" ? (
          <details className={addStyles.advanced} data-testid="update-log">
            <summary>{t("updateServer.showLog")}</summary>
            <pre className={addStyles.log}>{logText(job) || t("updateServer.logEmpty")}</pre>
          </details>
        ) : null}
        {phase === "running" && installStarted ? <p className={addStyles.note}>{t("updateServer.dontClose")}</p> : null}
      </div>
      <DialogFooter>
        {phase === "confirm" ? (
          <>
            <Button variant="ghost" onClick={() => setPhase("manual")} data-testid="update-show-commands">
              {t("updateServer.showCommands")}
            </Button>
            <Button variant="ghost" onClick={onClose}>
              {t("common.cancel")}
            </Button>
            <Button variant="primary" onClick={() => void start()} disabled={!api} data-testid="update-confirm">
              <RefreshCw aria-hidden="true" />
              {t("updateServer.confirm")}
            </Button>
          </>
        ) : null}
        {phase === "manual" ? (
          <>
            <Button variant="ghost" onClick={onClose}>
              {t("common.close")}
            </Button>
            {commands ? (
              <Button variant="primary" onClick={() => void copy()} data-testid="update-copy">
                <Copy aria-hidden="true" />
                {t("updateServer.copy")}
              </Button>
            ) : null}
          </>
        ) : null}
        {phase === "failed" ? (
          <>
            <Button variant="ghost" onClick={() => setPhase("manual")}>
              {t("updateServer.showCommands")}
            </Button>
            <Button variant="primary" onClick={() => setPhase("confirm")} data-testid="update-retry">
              {t("updateServer.retry")}
            </Button>
          </>
        ) : null}
        {phase === "done" ? (
          <Button variant="primary" onClick={onClose}>
            {t("common.close")}
          </Button>
        ) : null}
      </DialogFooter>
    </div>
  );
}
