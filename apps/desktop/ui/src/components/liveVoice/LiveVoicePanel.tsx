import { AudioLines, Mic, MicOff, PhoneOff, Square } from "lucide-react";
import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { isOpen, liveNoticeNeedsSettings, type LivePhase } from "../../lib/liveVoice/machine";
import { openSettingsTarget, VOICE_KEY_TARGET } from "../../lib/settingsLink";
import type { LiveVoice } from "./useLiveVoice";
import styles from "./LiveVoicePanel.module.css";

const BARS = 21;

/** The composer's "Talk" button: opens the live voice view. */
export function TalkButton({ live, name, className }: { live: LiveVoice; name: string; className?: string }) {
  const { t } = useTranslation();
  const open = live.state.phase !== "idle";
  const blocked = !open && live.blockedReason !== null;
  return (
    <Button
      variant="ghost"
      size="sm"
      className={className}
      title={blocked ? t(live.blockedReason as string) : t("voice.live.talkTitle", { name })}
      aria-label={t("voice.live.talk")}
      aria-disabled={blocked ? true : undefined}
      aria-expanded={open}
      aria-controls={open ? "live-voice-panel" : undefined}
      data-testid="live-voice-start"
      onClick={() => {
        if (blocked) return;
        if (open) document.getElementById("live-voice-panel")?.focus();
        else live.start();
      }}
    >
      <AudioLines aria-hidden="true" />
    </Button>
  );
}

/**
 * The live voice view (docs/design/voice-live.md): a calm panel over the conversation with a
 * breathing orb that follows the microphone and the voice model's speech, the words as they are
 * heard, mute, stop and end. It covers only the message list: the approval card and the composer
 * below stay in reach, because an approval is only ever a click.
 */
export function LiveVoicePanel({ live, botName }: { live: LiveVoice; botName: string }) {
  const { t } = useTranslation();
  const { state } = live;
  const panel = useRef<HTMLDivElement>(null);
  const orb = useRef<HTMLDivElement>(null);
  const bars = useRef<(HTMLSpanElement | null)[]>([]);
  const lines = useRef<HTMLDivElement>(null);
  const opener = useRef<HTMLElement | null>(null);
  const wasOpen = useRef(false);
  const phase = state.phase;

  // Focus moves into the panel when it opens and back to the control that opened it when it goes.
  useEffect(() => {
    const open = phase !== "idle";
    if (open && !wasOpen.current) {
      opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      panel.current?.focus();
    }
    if (!open && wasOpen.current && opener.current?.isConnected) {
      opener.current.focus({ preventScroll: true });
    }
    wasOpen.current = open;
  }, [phase]);

  // The orb and the bars follow the loudness. Written straight to the elements: no re-render per frame.
  useEffect(() => {
    if (!isOpen(phase)) {
      return;
    }
    const reduce = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
    let mic = 0;
    let bot = 0;
    let frame = 0;
    const tick = (now: number) => {
      mic += (live.levels.current.mic - mic) * 0.25;
      bot += (live.levels.current.bot - bot) * 0.25;
      const energy = Math.max(mic, bot);
      orb.current?.style.setProperty("--energy", energy.toFixed(3));
      orb.current?.style.setProperty("--mic", mic.toFixed(3));
      orb.current?.style.setProperty("--bot", bot.toFixed(3));
      bars.current.forEach((bar, i) => {
        if (!bar) return;
        // Without motion the bars stay flat; the orb's brightness still shows the level.
        const wave = reduce ? 0 : 0.5 + 0.5 * Math.sin(now / 160 + i * 0.7);
        bar.style.setProperty("--h", (0.08 + energy * (0.35 + 0.65 * wave)).toFixed(3));
      });
      frame = window.requestAnimationFrame(tick);
    };
    frame = window.requestAnimationFrame(tick);
    return () => window.cancelAnimationFrame(frame);
  }, [phase, live.levels]);

  // New words scroll into view.
  const last = state.lines[state.lines.length - 1];
  useEffect(() => {
    lines.current?.scrollTo?.({ top: lines.current.scrollHeight });
  }, [state.lines.length, last?.text]);

  if (phase === "idle") {
    return null;
  }
  const failed = phase === "failed";
  const ended = phase === "ended";
  const open = isOpen(phase);
  const status = open
    ? state.reconnecting
      ? t("voice.live.phase.reconnecting")
      : t(`voice.live.phase.${phase satisfies LivePhase}`, { name: botName })
    : t(`voice.live.phase.${phase}`);
  const tone = failed ? "error" : phase === "approval" ? "wait" : phase === "working" ? "work" : phase === "speaking" ? "speak" : "listen";

  return (
    <div
      ref={panel}
      id="live-voice-panel"
      className={styles.panel}
      role="region"
      aria-label={t("voice.live.title")}
      tabIndex={-1}
      data-testid="live-voice"
      data-phase={phase}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          if (open || phase === "starting") live.end();
          else live.dismiss();
        }
      }}
    >
      <div className={styles.top}>
        {open ? (
          <span className={styles.mic} data-testid="live-voice-indicator">
            <span className={`${styles.dot} ${state.muted ? styles.dotMuted : ""}`} aria-hidden="true" />
            {t(state.muted ? "voice.live.micMuted" : "voice.live.micOn")}
          </span>
        ) : (
          <span />
        )}
        <span className={styles.name}>{botName}</span>
      </div>

      <div className={styles.stage}>
        <div ref={orb} className={styles.orb} data-tone={tone} data-active={open ? "true" : "false"} aria-hidden="true">
          <span className={styles.halo} />
          <span className={styles.core} />
        </div>
        <div className={styles.bars} aria-hidden="true" data-tone={tone}>
          {Array.from({ length: BARS }, (_, i) => (
            <span key={i} ref={(el) => void (bars.current[i] = el)} className={styles.bar} />
          ))}
        </div>
      </div>

      <p className={styles.status} role="status" aria-live="polite" data-testid="live-voice-status">
        {status}
      </p>
      {state.notice ? (
        <p className={`${styles.notice} ${failed ? styles.noticeError : ""}`} role={failed ? "alert" : undefined}>
          {t(state.notice)}
        </p>
      ) : null}
      {phase === "approval" ? <p className={styles.notice}>{t("voice.live.approvalHint")}</p> : null}

      {open || ended ? (
        <div className={styles.lines} ref={lines} role="log" aria-live="off" aria-label={t("voice.live.transcript")} data-testid="live-voice-lines">
          {state.lines.length === 0 ? (
            <p className={styles.empty}>{t("voice.live.empty")}</p>
          ) : (
            state.lines.map((line) => (
              <p key={line.id} className={`${styles.line} ${line.role === "user" ? styles.you : styles.bot}`} data-final={line.final ? "true" : "false"}>
                <span className={styles.who}>{line.role === "user" ? t("voice.live.you") : botName}</span>
                {line.text}
              </p>
            ))
          )}
        </div>
      ) : null}

      <div className={styles.controls}>
        {open ? (
          <>
            <Button
              variant="ghost"
              aria-pressed={state.muted}
              title={t(state.muted ? "voice.live.unmute" : "voice.live.mute")}
              aria-label={t(state.muted ? "voice.live.unmute" : "voice.live.mute")}
              data-testid="live-voice-mute"
              onClick={live.toggleMute}
            >
              {state.muted ? <MicOff aria-hidden="true" /> : <Mic aria-hidden="true" />}
            </Button>
            {state.botSpeaking ? (
              <Button variant="secondary" onClick={live.stopTalking} data-testid="live-voice-stop">
                <Square aria-hidden="true" />
                {t("voice.live.stopTalking")}
              </Button>
            ) : null}
            <Button variant="danger" onClick={live.end} data-testid="live-voice-end">
              <PhoneOff aria-hidden="true" />
              {t("voice.live.end")}
            </Button>
          </>
        ) : phase === "starting" ? (
          <Button variant="danger" onClick={live.end} data-testid="live-voice-end">
            {t("voice.live.cancel")}
          </Button>
        ) : (
          <>
            {failed && liveNoticeNeedsSettings(state.notice) ? (
              <Button
                variant="primary"
                data-autofocus
                data-testid="live-voice-open-settings"
                onClick={() => {
                  live.dismiss();
                  openSettingsTarget(VOICE_KEY_TARGET);
                }}
              >
                {t("voice.live.openSettings")}
              </Button>
            ) : null}
            <Button variant={failed && liveNoticeNeedsSettings(state.notice) ? "secondary" : "primary"} onClick={live.start} data-testid="live-voice-retry">
              {t(failed ? "voice.live.retry" : "voice.live.talkAgain")}
            </Button>
            <Button variant="ghost" onClick={live.dismiss} data-testid="live-voice-close">
              {t("voice.live.close")}
            </Button>
          </>
        )}
      </div>
      {open ? <p className={styles.hint}>{t("voice.live.speakHint")}</p> : null}
    </div>
  );
}
