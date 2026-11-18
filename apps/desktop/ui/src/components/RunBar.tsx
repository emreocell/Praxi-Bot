import {
  Expand,
  Hand,
  LoaderCircle,
  OctagonAlert,
  Pause,
  Play,
  ShieldQuestion,
  Square,
  Undo2,
  X
} from "lucide-react";
import { type ReactNode, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { formatElapsed, type RunBarModel, type RunStopUi, runBarVisible } from "../lib/runBar";
import { appLedger, requestStop, useStopStatus } from "../state/appStop";
import { stopDetail, stopTitleKey } from "../state/stopStatus";
import { STOP_ICONS } from "./StopStatusNotice";
import styles from "./RunBar.module.css";

/** Keeps the last non-null value for `ms` after `value` goes null (closing animation). */
function useLingering<T>(value: T | null, ms: number): T | null {
  const [held, setHeld] = useState<T | null>(value);
  useEffect(() => {
    if (value !== null) {
      setHeld(value);
      return undefined;
    }
    const id = window.setTimeout(() => setHeld(null), ms);
    return () => window.clearTimeout(id);
  }, [value, ms]);
  return value ?? held;
}

function useNow(active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) {
      return undefined;
    }
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [active]);
  return now;
}

export interface RunBarProps {
  model: RunBarModel | null;
  /** Bot name, shown only when the run belongs to another bot than this chat. */
  botName?: string;
  /** What the bot is doing right now ("Searching the web"). */
  detail?: string | null;
  /** When the run began (epoch ms); drives the elapsed time. */
  startedMs?: number | null;
  /** Stop progress of this chat's run, from the run ledger (a single-run Stop). */
  runStop?: RunStopUi | null;
  /** Whether the desktop control is currently the human's. */
  humanDriving: boolean;
  onStop: () => void;
  onPause: () => void;
  onResume: () => void;
  onWheel: () => void;
  onBigView: () => void;
  /** `dock`: sits on the composer. `strip`: full-width row on screens without a composer. */
  variant?: "dock" | "strip";
}

/**
 * THE run-control surface (244-D): one compact bar with the status, elapsed
 * time and Stop / Pause / Take the wheel / Open big view. The Stop status of
 * the run ledger (stopping, stopped, could not confirm) is shown in the same
 * bar, so there is never a second Stop or a second status somewhere else.
 *
 * It lives in a grid row whose height animates between 0 and its content, so
 * starting or ending a run never makes the page jump.
 */
export function RunBar({
  model,
  botName,
  detail,
  startedMs,
  runStop,
  humanDriving,
  onStop,
  onPause,
  onResume,
  onWheel,
  onBigView,
  variant = "dock"
}: RunBarProps) {
  const { t } = useTranslation();
  const { status, dismiss } = useStopStatus();
  const open = runBarVisible(model, status !== null);
  const shownModel = useLingering(model, 260);
  const shownStatus = useLingering(status, 260);
  const now = useNow(open && startedMs != null);
  const lastStart = useRef(startedMs);
  if (startedMs != null) {
    lastStart.current = startedMs;
  }
  const elapsed =
    lastStart.current != null ? formatElapsed(now - lastStart.current) : null;

  let body: ReactNode = null;
  if (shownStatus) {
    const Icon = STOP_ICONS[shownStatus.kind];
    const d = stopDetail(shownStatus);
    const progress =
      shownStatus.total > 0 ? shownStatus.stopped / shownStatus.total : 0;
    body = (
      <div
        className={styles.bar}
        data-tone={shownStatus.tone}
        data-kind={shownStatus.kind}
        data-testid="run-bar-stop-status"
      >
        <span
          className={styles.lead}
          data-spin={shownStatus.kind === "stopping" ? "true" : undefined}
        >
          <Icon aria-hidden="true" />
        </span>
        <span
          className={styles.text}
          role={shownStatus.tone === "danger" ? "alert" : "status"}
          aria-live="polite"
        >
          <strong className={styles.title}>{t(stopTitleKey(shownStatus))}</strong>
          <span className={styles.detail}>{t(d.key, d.params)}</span>
        </span>
        {shownStatus.total > 0 && shownStatus.kind !== "stopped" ? (
          <span
            className={styles.meter}
            role="progressbar"
            aria-label={t("stopStatus.progressLabel")}
            aria-valuemin={0}
            aria-valuemax={shownStatus.total}
            aria-valuenow={shownStatus.stopped}
          >
            <span
              className={styles.meterFill}
              style={{ inlineSize: `${Math.round(progress * 100)}%` }}
            />
          </span>
        ) : null}
        <span className={styles.actions}>
          {shownStatus.recoverable ? (
            <>
              <Button
                variant="danger"
                size="sm"
                data-testid="stop-status-stop-all"
                onClick={() => requestStop("banner")}
              >
                {t("stopStatus.stopAllNow")}
              </Button>
              <Button
                variant="secondary"
                size="sm"
                data-testid="stop-status-release"
                onClick={() => appLedger().releaseUnconfirmed(shownStatus.stopId)}
              >
                {t("stopStatus.letContinue")}
              </Button>
            </>
          ) : null}
          {shownStatus.dismissible || shownStatus.recoverable ? (
            <button
              type="button"
              className={styles.iconButton}
              aria-label={t("stopStatus.dismiss")}
              title={t("stopStatus.dismiss")}
              onClick={dismiss}
            >
              <X aria-hidden="true" />
            </button>
          ) : null}
        </span>
      </div>
    );
  } else if (shownModel) {
    const m = shownModel;
    const stopping = runStop?.phase === "stopping" || runStop?.phase === "unconfirmed";
    const unconfirmed = runStop?.phase === "unconfirmed";
    const stopFailed = runStop?.phase === "failed";
    const title = stopping
      ? t("stopStatus.title.stopping")
      : stopFailed
        ? t("stopStatus.title.partial")
        : m.kind === "waiting"
        ? t("chat.runBar.waiting")
        : m.kind === "paused"
          ? t("chat.runBar.paused")
          : m.kind === "human"
            ? t("chat.runBar.human")
            : t("chat.working");
    const LeadIcon = stopping
      ? LoaderCircle
      : stopFailed
        ? OctagonAlert
        : m.kind === "waiting"
        ? ShieldQuestion
        : m.kind === "paused"
          ? Pause
          : m.kind === "human"
            ? Hand
            : LoaderCircle;
    body = (
      <div
        className={styles.bar}
        data-kind={m.kind}
        data-tone={stopFailed ? "danger" : stopping ? "warning" : undefined}
        data-testid="run-bar"
      >
        <span
          className={styles.lead}
          data-spin={m.kind === "running" || stopping ? "true" : undefined}
        >
          <LeadIcon aria-hidden="true" />
        </span>
        <span className={styles.text} role="status" aria-live="polite">
          <strong className={styles.title}>
            {botName ? `${botName} · ${title}` : title}
          </strong>
          {stopFailed && runStop?.error ? (
            <span className={styles.detail} title={runStop.error}>
              {runStop.error}
            </span>
          ) : stopping ? (
            <span className={styles.detail}>
              {t(
                runStop?.unreachable
                  ? "chat.runBar.stopUnreachable"
                  : unconfirmed
                    ? "chat.runBar.stopUnconfirmed"
                    : "chat.runBar.stoppingDetail"
              )}
            </span>
          ) : detail ? (
            <span className={styles.detail}>{detail}</span>
          ) : null}
        </span>
        {elapsed ? (
          <span
            className={styles.elapsed}
            title={t("chat.runBar.elapsed", { time: elapsed })}
            aria-hidden="true"
          >
            {elapsed}
          </span>
        ) : null}
        <span className={styles.actions}>
          {m.canResume ? (
            <Button variant="secondary" size="sm" onClick={onResume}>
              <Play aria-hidden="true" />
              <span className={styles.label}>{t("widget.resume")}</span>
            </Button>
          ) : null}
          {m.canPause ? (
            <Button
              variant="secondary"
              size="sm"
              onClick={onPause}
              title={t("widget.pause")}
              aria-label={t("widget.pause")}
            >
              <Pause aria-hidden="true" />
              <span className={styles.label}>{t("widget.pause")}</span>
            </Button>
          ) : null}
          {m.canWheel ? (
            <Button
              variant={humanDriving ? "primary" : "secondary"}
              size="sm"
              onClick={onWheel}
              title={humanDriving ? t("widget.giveBack") : t("widget.takeTheWheel")}
              aria-label={humanDriving ? t("widget.giveBack") : t("widget.takeTheWheel")}
            >
              {humanDriving ? <Undo2 aria-hidden="true" /> : <Hand aria-hidden="true" />}
              <span className={styles.label}>
                {humanDriving ? t("widget.giveBack") : t("widget.takeTheWheel")}
              </span>
            </Button>
          ) : null}
          {m.canBigView ? (
            <button
              type="button"
              className={styles.iconButton}
              onClick={onBigView}
              title={t("liveView.expand")}
              aria-label={t("liveView.expand")}
            >
              <Expand aria-hidden="true" />
            </button>
          ) : null}
          {/* While a Stop is unconfirmed the same control stays: pressing it again is the hard stop. */}
          <Button
            variant="danger"
            size="sm"
            onClick={onStop}
            data-testid="run-bar-stop"
          >
            <Square aria-hidden="true" />
            <span>{t(stopping || stopFailed ? "chat.runBar.forceStop" : "chat.stop")}</span>
          </Button>
        </span>
      </div>
    );
  }

  return (
    <div
      className={styles.dock}
      data-open={open ? "true" : "false"}
      data-variant={variant}
      role="group"
      aria-label={t("chat.runBar.label")}
      hidden={!open && !body ? true : undefined}
    >
      <div className={styles.dockInner}>{body}</div>
    </div>
  );
}
