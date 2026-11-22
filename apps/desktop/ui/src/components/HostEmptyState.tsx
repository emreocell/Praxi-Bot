import { Check, Circle, Copy, MonitorPlay, Plus, ServerCog, Terminal } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import {
  HOST_MODEL_KEY_FROM_APP,
  HOST_MODEL_SETUP_COMMAND
} from "../lib/hostMode";
import { LOCAL_COMPUTER, useDaemon } from "../state/DaemonContext";
import { useLiveView } from "../state/LiveViewContext";
import { Modal } from "./Modal";
import { RunOnServer } from "./RunOnServer";
import styles from "./HostEmptyState.module.css";

/**
 * "Connect a model on this server". Nothing is sent to the host from this app yet
 * (owner approval pending): the dialog shows the command to run on the server. When
 * `HOST_MODEL_KEY_FROM_APP` is turned on, the key form belongs in this dialog.
 */
function HostModelDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { t } = useTranslation();
  const { activeHostName } = useDaemon();
  const [copied, setCopied] = useState(false);
  const name = activeHostName ?? t("computerPicker.host");
  return (
    <Modal open={open} onClose={onClose} size="sm" label={t("hostMode.model.title")}>
      <div className={styles.dialog} data-testid="host-model-dialog">
        <h2 className={styles.dialogTitle}>{t("hostMode.model.title")}</h2>
        <p className={styles.dialogText}>
          {HOST_MODEL_KEY_FROM_APP
            ? t("hostMode.model.fromApp", { name })
            : t("hostMode.model.onServer", { name })}
        </p>
        <div className={styles.command}>
          <Terminal aria-hidden="true" />
          <code>{HOST_MODEL_SETUP_COMMAND}</code>
          <button
            type="button"
            className={styles.copy}
            aria-label={t("hostMode.model.copy")} title={t("hostMode.model.copy")}
            onClick={() => {
              void navigator.clipboard?.writeText(HOST_MODEL_SETUP_COMMAND).then(
                () => setCopied(true),
                () => {}
              );
            }}
          >
            {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
          </button>
          <RunOnServer command={HOST_MODEL_SETUP_COMMAND} className={styles.copy} beforeOpen={onClose} />
        </div>
        <p className={styles.dialogHint}>{t("hostMode.model.afterwards")}</p>
        <div className={styles.dialogActions}>
          <Button variant="primary" onClick={onClose}>
            {t("common.close")}
          </Button>
        </div>
      </div>
    </Modal>
  );
}

function StatusRow({ ok, label }: { ok: boolean; label: string }) {
  return (
    <li className={styles.row} data-ok={ok ? "true" : "false"}>
      {ok ? <Check aria-hidden="true" /> : <Circle aria-hidden="true" />}
      <span>{label}</span>
    </li>
  );
}

/**
 * The calm center of the chat area while a paired host is selected and has no bots (or
 * no model). Replaces the local first-run wizard, which only fits this computer.
 */
export function HostEmptyState({ onNewBot, onConnectModel }: { onNewBot: () => void; onConnectModel?: () => void }) {
  const { t } = useTranslation();
  const { activeComputerId, activeHostName, hostState, bots, setWorkLocalNow } = useDaemon();
  const { targets, openViewer } = useLiveView();
  const [modelOpen, setModelOpen] = useState(false);
  if (activeComputerId === LOCAL_COMPUTER) {
    return null;
  }
  const name = activeHostName ?? t("computerPicker.host");
  const kind = hostState?.kind ?? "connecting";
  const noModel = kind === "no_model_key";
  const reachable = kind === "connected" || kind === "no_model_key";
  const stateKey =
    reachable
      ? "connected"
      : kind === "connecting"
        ? "connecting"
        : kind === "not_paired"
          ? "notPaired"
          : kind === "version_mismatch"
            ? "mismatch"
            : "offline";
  return (
    <div className={styles.pane} data-testid="host-empty-state" data-state={stateKey}>
      <div className={styles.card}>
        <span className={styles.icon}>
          <ServerCog aria-hidden="true" />
        </span>
        <h1 className={styles.title}>{name}</h1>
        <p className={styles.state} data-state={stateKey}>
          <i aria-hidden="true" />
          {t(`hostMode.state.${stateKey}`)}
        </p>
        {reachable ? (
          <ul className={styles.rows}>
            <StatusRow ok={!noModel} label={t(noModel ? "hostMode.missing.model" : "hostMode.have.model")} />
            <StatusRow
              ok={bots.length > 0}
              label={t(bots.length > 0 ? "hostMode.have.bots" : "hostMode.missing.bots")}
            />
          </ul>
        ) : null}
        <div className={styles.actions}>
          {reachable && noModel ? (
            <Button variant="primary" onClick={() => (onConnectModel ? onConnectModel() : setModelOpen(true))} data-testid="host-connect-model">
              {t("hostMode.model.action", { name })}
            </Button>
          ) : null}
          {reachable ? (
            <Button
              variant={noModel ? "secondary" : "primary"}
              onClick={onNewBot}
              data-testid="host-new-bot"
            >
              <Plus aria-hidden="true" />
              {t("sidebar.newBot")}
            </Button>
          ) : null}
          {reachable && targets.length > 0 ? (
            <Button variant="secondary" onClick={() => openViewer()} data-testid="host-view-screen">
              <MonitorPlay aria-hidden="true" />
              {t("hostMode.viewScreen")}
            </Button>
          ) : null}
          <Button variant="ghost" onClick={() => setWorkLocalNow(true)}>
            {t("home.banner.workHere")}
          </Button>
        </div>
        {reachable ? <p className={styles.note}>{t("hostMode.managedOnServer", { name })}</p> : null}
      </div>
      <HostModelDialog open={modelOpen} onClose={() => setModelOpen(false)} />
    </div>
  );
}
