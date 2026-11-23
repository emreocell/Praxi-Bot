import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { olderSide } from "../lib/hostState";
import { LOCAL_COMPUTER, useDaemon } from "../state/DaemonContext";
import styles from "./HostStateBanner.module.css";

/**
 * Says what is going on with the connection to the active host, in plain words:
 * connecting, host off, no longer paired, version mismatch (both versions) or no
 * model key on the host yet. Silent while this computer is active or all is well.
 */
export function HostStateBanner() {
  const { t } = useTranslation();
  const { hostState, activeHostName, versions, home, workLocalNow, setWorkLocalNow, activeComputerId, bots } =
    useDaemon();
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
  if (activeComputerId === LOCAL_COMPUTER || !hostState || hostState.kind === "connected") {
    return null;
  }
  // With no bots the empty state in the chat area already says "no model" and "no bots".
  if (hostState.kind === "no_model_key" && bots.length === 0) {
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
      text = t("home.banner.offline");
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
        const side = hostState.blocking ? olderSide(local, host) : "unknown";
        // Name the side to update: the one that is behind.
        const key = !hostState.blocking
          ? "hostState.versionWarning"
          : side === "app"
            ? "hostState.updateApp"
            : side === "host"
              ? "hostState.updateHost"
              : "hostState.versionBlocking";
        text = t(key, { name, local, host });
      }
      break;
    case "starting":
    case "degraded":
      return null;
    case "no_model_key":
      tone = "warn";
      text = t("hostState.noModelKey", { name });
      break;
  }
  return (
    <div
      className={styles.root}
      data-tone={tone}
      data-kind={hostState.kind}
      role="status"
      data-testid="host-state-banner"
    >
      <i className={styles.dot} aria-hidden="true" />
      <span className={styles.text}>{text}</span>
      <Button
        size="sm"
        variant="ghost"
        onClick={() => setWorkLocalNow(true)}
      >
        {t("home.banner.workHere")}
      </Button>
    </div>
  );
}
