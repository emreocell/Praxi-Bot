import { Captions, Hand, Mic, MicOff, PhoneOff, Sparkles, Square } from "lucide-react";
import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button, type BotAvatarState } from "ui";
import { isMockMode } from "../../api/mockMode";
import { openSettingsTarget, VOICE_KEY_TARGET } from "../../lib/settingsLink";
import { noticeNeedsSettings } from "../../lib/voiceCall/readiness";
import { Modal } from "../Modal";
import type { CallPhase } from "../../lib/voiceCall/callMachine";
import styles from "./VoiceCallPanel.module.css";
import type { VoiceCall } from "./useVoiceCall";

type Identity = { id: string; name: string; color?: string };

const AVATAR: Record<CallPhase, BotAvatarState> = {
  idle: "idle",
  starting: "thinking",
  listening: "idle",
  thinking: "thinking",
  speaking: "working",
  approval: "waiting-approval",
  failed: "error"
};

/**
 * The visible call (docs/design/voice-call-mode.md): bot avatar that follows the call state, a clear
 * "microphone on" indicator, mute / push-to-talk / captions / end. It is not modal, so the approval
 * card stays clickable (approvals are click-only, never by voice).
 */
export function VoiceCallPanel({ call, bot }: { call: VoiceCall; bot: Identity }) {
  const { t } = useTranslation();
  const { state } = call;
  const panel = useRef<HTMLDivElement>(null);
  const live = state.phase !== "idle" && state.phase !== "failed";
  const opener = useRef<HTMLElement | null>(null);
  const wasOpen = useRef(false);

  // Focus moves into the call when it opens so the keyboard works at once; when the call is gone
  // it returns to the control that opened it (the call button).
  useEffect(() => {
    const open = state.phase !== "idle";
    if (open && !wasOpen.current) {
      opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    }
    if (!open && wasOpen.current && opener.current?.isConnected) {
      opener.current.focus({ preventScroll: true });
    }
    wasOpen.current = open;
    if (state.phase === "starting") {
      panel.current?.focus();
    }
  }, [state.phase]);

  if (state.phase === "idle") {
    return null;
  }
  const failed = state.phase === "failed";
  // Needs the owner to act (e.g. add a key): a centered dialog above everything, with a trap,
  // Escape, and focus return. Live call states dock bottom-right but live outside the app
  // layers (<body>), so no side panel or composer can cover them.
  if (failed) {
    const goSettings = noticeNeedsSettings(state.notice);
    return (
      <Modal open onClose={call.end} label={t("voice.call.title")} size="sm">
        <div className={styles.card} data-testid="voice-call" data-phase="failed">
          <BotAvatar bot={bot} size="lg" state="error" decorative />
          <p className={styles.status} role="status" data-testid="voice-call-status">
            {t("voice.call.phase.failed")}
          </p>
          {state.notice ? <p className={`${styles.notice} ${styles.noticeError}`} role="alert">{t(state.notice)}</p> : null}
          <div className={styles.controls}>
            {goSettings ? (
              <Button
                variant="primary"
                data-autofocus
                data-testid="voice-call-open-settings"
                onClick={() => {
                  call.end();
                  openSettingsTarget(VOICE_KEY_TARGET);
                }}
              >
                {t("voice.call.openSettings")}
              </Button>
            ) : null}
            <Button variant={goSettings ? "secondary" : "primary"} data-autofocus={goSettings ? undefined : true} onClick={() => call.start({ fresh: true })} data-testid="voice-call-retry">
              {t("voice.call.retry")}
            </Button>
            <Button variant="ghost" onClick={call.end} data-testid="voice-call-close">
              {t("voice.call.close")}
            </Button>
          </div>
        </div>
      </Modal>
    );
  }
  const status = t(`voice.call.phase.${state.phase}`, { name: bot.name });
  const talkKey = (down: boolean) => (e: React.KeyboardEvent) => {
    if ((e.key === " " || e.key === "Enter") && !e.repeat) {
      e.preventDefault();
      if (down) {
        call.pushToTalkDown();
      } else {
        call.pushToTalkUp();
      }
    }
  };

  return createPortal(
    <div
      ref={panel}
      className={styles.panel}
      role="region"
      aria-label={t("voice.call.title")}
      tabIndex={-1}
      data-testid="voice-call"
      id="voice-call-panel"
      data-phase={state.phase}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          call.end();
        }
      }}
    >
      {live && state.phase !== "starting" ? (
        <span className={styles.live} data-testid="voice-call-indicator">
          <span className={`${styles.dot} ${state.muted ? styles.dotMuted : ""}`} aria-hidden="true" />
          {t(state.muted ? "voice.call.micMuted" : "voice.call.micOn")}
        </span>
      ) : null}
      <div className={styles.stage} style={{ ["--level" as string]: call.level.toFixed(2) }}>
        <span
          className={`${styles.ring} ${state.phase === "listening" && !state.muted ? "" : styles.ringIdle} ${state.phase === "approval" ? styles.ringWait : ""}`}
          aria-hidden="true"
        />
        <BotAvatar bot={bot} size="lg" state={AVATAR[state.phase]} live={state.phase === "listening" || state.phase === "speaking"} decorative />
      </div>
      <p className={styles.status} role="status" aria-live="polite" data-testid="voice-call-status">
        {state.phase === "listening" && state.heard ? t("voice.call.phase.hearing") : status}
      </p>
      {state.notice ? (
        <p className={styles.notice}>
          {t(state.notice)}
        </p>
      ) : null}
      {state.phase === "approval" ? <p className={styles.notice}>{t("voice.call.approvalHint")}</p> : null}
      {state.captions && (call.captions.you || call.captions.bot) ? (
        <div className={styles.captions} data-testid="voice-call-captions">
          {call.captions.you ? <span className={styles.you}>{call.captions.you}</span> : null}
          {call.captions.bot ? <span className={styles.bot}>{call.captions.bot}</span> : null}
        </div>
      ) : null}
      {live && state.pushToTalk && state.phase === "listening" ? (
        <Button
          variant={state.heard ? "primary" : "secondary"}
          className={styles.talk}
          data-testid="voice-call-talk"
          disabled={state.muted}
          onPointerDown={call.pushToTalkDown}
          onPointerUp={call.pushToTalkUp}
          onPointerLeave={() => state.heard && call.pushToTalkUp()}
          onKeyDown={talkKey(true)}
          onKeyUp={talkKey(false)}
        >
          {t("voice.call.holdToTalk")}
        </Button>
      ) : null}
      <div className={styles.controls}>
        {(
          <>
            <Button
              variant="ghost"
              size="sm"
              aria-pressed={state.muted}
              title={t(state.muted ? "voice.call.unmute" : "voice.call.mute")}
              aria-label={t(state.muted ? "voice.call.unmute" : "voice.call.mute")}
              data-testid="voice-call-mute"
              onClick={call.toggleMute}
            >
              {state.muted ? <MicOff aria-hidden="true" /> : <Mic aria-hidden="true" />}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              aria-pressed={state.pushToTalk}
              title={t("voice.call.pushToTalk")}
              aria-label={t("voice.call.pushToTalk")}
              data-testid="voice-call-ptt"
              onClick={call.togglePushToTalk}
            >
              <Hand aria-hidden="true" />
            </Button>
            <Button
              variant="ghost"
              size="sm"
              aria-pressed={state.captions}
              title={t("voice.call.captions")}
              aria-label={t("voice.call.captions")}
              data-testid="voice-call-captions-toggle"
              onClick={call.toggleCaptions}
            >
              <Captions aria-hidden="true" />
            </Button>
            {state.phase === "speaking" ? (
              <Button variant="secondary" size="sm" onClick={call.interrupt} data-testid="voice-call-interrupt">
                <Square aria-hidden="true" />
                {t("voice.call.interrupt")}
              </Button>
            ) : null}
            {isMockMode() && state.phase === "listening" ? (
              <Button variant="secondary" size="sm" onClick={call.simulateSpeech} data-testid="voice-call-simulate">
                <Sparkles aria-hidden="true" />
                {t("voice.call.simulate")}
              </Button>
            ) : null}
            <Button variant="danger" size="sm" onClick={call.end} data-testid="voice-call-end">
              <PhoneOff aria-hidden="true" />
              {t(state.phase === "starting" ? "voice.call.cancel" : "voice.call.end")}
            </Button>
          </>
        )}
      </div>
    </div>,
    document.body
  );
}
