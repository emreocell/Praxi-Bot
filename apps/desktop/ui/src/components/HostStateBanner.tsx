import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { ApiClient, ApiError } from "../api/client";
import { LOCAL_COMPUTER, useDaemon } from "../state/DaemonContext";
import styles from "./HostStateBanner.module.css";
import { UpdateServerModal } from "./UpdateServerModal";

/**
 * Says what is going on with the connection to the active host, in plain words:
 * connecting, host off, no longer paired, version mismatch (both versions) or no
 * model key on the host yet. Silent while this computer is active or all is well.
 */
export function HostStateBanner() {
  const { t } = useTranslation();
  const { hostState, activeHostName, versions, home, workLocalNow, setWorkLocalNow, activeComputerId, bots, hostConn, hostList, health } =
    useDaemon();
  const [fallback, setFallback] = useState<"update" | "commands" | null>(null);
  const [starting, setStarting] = useState(false);
  const [updateError, setUpdateError] = useState<"denied" | "refused" | null>(null);
  const managed = health?.host_update;
  const target = hostList?.local_release_tag;
  const canUpdate = hostState?.kind === "version_mismatch" &&
    (hostState.update === "host" || (hostState.update === "unknown" && hostState.blocking && Boolean(target)));
  const updateMessage = managed && managed.state !== "idle" && managed.state !== "updated" ? managed.state : null;

  const update = async () => {
    if (!managed || !hostConn || !target) {
      setFallback("update");
      return;
    }
    const server = new ApiClient(hostConn);
    setStarting(true);
    setUpdateError(null);
    try {
      await server.startManagedHostUpdate(target);
    } catch (e) {
      setUpdateError(e instanceof ApiError && e.status === 403 ? "denied" : "refused");
    } finally {
      setStarting(false);
      server.dispose();
    }
  };
  // The explicit, temporary mode keeps a visible way back to the bots' computer.
  if (home.kind === "host" && workLocalNow) {
    return (
      <div className={styles.root} data-tone="warn" data-kind="temporary" role="status" data-testid="host-state-banner">
        <i className={styles.dot} aria-hidden="true" />
        <span className={styles.text}>{t("home.banner.temporary", { name: home.name || t("computerPicker.host") })}</span>
        <Button size="sm" variant="ghost" onClick={() => setWorkLocalNow(false)}>
          {t("home.banner.back", { name: home.name || t("computerPicker.host") })}
        </Button>
      </div>
    );
  }
  if (activeComputerId === LOCAL_COMPUTER || !hostState) {
    return null;
  }
  // With no bots the empty state in the chat area already says "no model" and "no bots".
  if (hostState.kind === "no_model_key" && bots.length === 0 && !updateMessage) {
    return null;
  }
  const name = activeHostName ?? t("computerPicker.host");
  let tone: "wait" | "warn" | "bad" = "warn";
  let text: string;
  switch (hostState.kind) {
    case "connecting":
      tone = "wait";
      text = t("hostState.connecting", { name });
      break;
    case "offline":
      tone = "bad";
      text = t("hostState.offline", { name });
      break;
    case "not_paired":
      tone = "bad";
      text = t("hostState.notPaired", { name });
      break;
    case "version_mismatch":
      tone = hostState.blocking ? "bad" : "warn";
      {
        const local = hostState.local || versions.local || "?";
        const host = hostState.host || versions.host || "?";
        const key = hostState.unidentified ? "serverConnectionStatus.unidentified"
          : hostState.blocking && host === local ? "serverConnectionStatus.protocolMismatch"
          : hostState.update === "host" ? "hostState.updateHost"
          : hostState.update === "app" && hostState.blocking ? "hostState.updateApp"
          : hostState.blocking ? "hostState.versionBlocking" : "hostState.versionWarning";
        text = t(key, { name, local, host });
      }
      break;
    case "starting":
    case "degraded":
    case "connected":
      if (!updateMessage && !fallback) return null;
      text = "";
      break;
    case "no_model_key":
      tone = "warn";
      text = t("hostState.noModelKey", { name });
      break;
  }
  if (updateError) {
    text = t(`serverConnectionStatus.${updateError}`, { name });
    tone = "bad";
  } else if (updateMessage && (updateMessage !== "available" || !text)) {
    text = t(`serverConnectionStatus.${updateMessage}`, { name, tag: managed?.tag });
    tone = updateMessage === "running" ? "wait" : updateMessage === "rollback_failed" ? "bad" : "warn";
  }
  return (
    <>
      {text ? (
        <div
          className={styles.root}
          data-tone={tone}
          data-kind={hostState.kind}
          role="status"
          data-testid="host-state-banner"
        >
          <i className={styles.dot} aria-hidden="true" />
          <span className={styles.text}>{text}</span>
          {canUpdate ? (
            <Button size="sm" variant="primary" disabled={starting || managed?.state === "running"} onClick={() => void update()}>
              {t("updateServer.title")}
            </Button>
          ) : null}
          {updateError || updateMessage === "failed" || updateMessage === "rollback_failed" ? (
            <Button size="sm" variant="ghost" onClick={() => setFallback("commands")}>
              {t("updateServer.showCommands")}
            </Button>
          ) : null}
          <Button size="sm" variant="ghost" onClick={() => setWorkLocalNow(true)}>
            {t("home.banner.workHere")}
          </Button>
        </div>
      ) : null}
      {fallback ? <UpdateServerModal open hostId={activeComputerId} showCommands={fallback === "commands"} onClose={() => setFallback(null)} /> : null}
    </>
  );
}
