import { LoaderCircle, Mic, Square } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { ApiError } from "../api/client";
import { dictationSupported, pickRecordingMime } from "../lib/dictation";
import { humanLine } from "../lib/humanError";
import { micErrorKey } from "../lib/micError";
import { openMic, watchLevel, type MicMeter } from "../lib/micMeter";
import { stopSpeaking } from "../lib/speaker";
import { createVad, LISTEN_VAD } from "../lib/voiceCall/vad";
import { transcribeRecording } from "../lib/dictationTarget";
import { transcribePayload } from "../lib/voiceAudio";
import {
  detectPlatform,
  IDLE,
  isPttPress,
  micDeniedStepsKey,
  pttKeyLabel,
  stepDictation,
  type DictationEvent,
  type DictationState
} from "../lib/voiceInput";
import { useDaemon, useLocalDaemonClient } from "../state/DaemonContext";
import { DEFAULT_SETTINGS, loadSettingsSync } from "../store/settingsStore";
import { RecordingIndicator } from "./MicLevel";
import { useToast } from "./Toast";

/** Longest single dictation; the recording stops by itself (keeps uploads small). */
const MAX_RECORD_MS = 120_000;

/**
 * Dictation for the composer: click to record and click again to stop, or (when switched on in
 * Settings > Voice) hold the push-to-talk key and release it to send. Recording can also stop by
 * itself after some seconds of silence. The recording is transcribed by the configured service and
 * handed to `onText`. The microphone is opened only by this action and closed right after.
 */
export function DictateButton({ onText, className }: { onText: (text: string) => void; className?: string }) {
  const { t, i18n } = useTranslation();
  const { client } = useDaemon();
  const localClient = useLocalDaemonClient();
  const toast = useToast();
  const [state, setState] = useState<DictationState>(IDLE);
  const [level, setLevel] = useState(0);
  const stateRef = useRef<DictationState>(IDLE);
  const recorder = useRef<MediaRecorder | null>(null);
  const stream = useRef<MediaStream | null>(null);
  const meter = useRef<MicMeter | null>(null);
  const timer = useRef<number | null>(null);
  const discard = useRef(false);
  // Latest props/callbacks for the long-lived listeners below.
  const live = useRef({ client, localClient, onText, lang: i18n.language, t });
  live.current = { client, localClient, onText, lang: i18n.language, t };

  const release = useCallback(() => {
    if (timer.current !== null) {
      window.clearTimeout(timer.current);
      timer.current = null;
    }
    meter.current?.stop();
    meter.current = null;
    stream.current?.getTracks().forEach((tr) => tr.stop());
    stream.current = null;
    setLevel(0);
  }, []);

  const send = useCallback(
    async (blob: Blob) => {
      const { client: c, localClient: local, onText: done, lang, t: tr } = live.current;
      try {
        if (!c && !local) {
          return;
        }
        // The microphone is on this computer: this computer's own daemon transcribes, whichever
        // computer the bot works on. The paired server is only a fallback (lib/dictationTarget.ts).
        done(
          await transcribeRecording(blob, {
            local,
            active: c,
            language: lang.split("-")[0] ?? lang,
            encode: (b, provider) => transcribePayload(b, provider)
          })
        );
      } catch (e) {
        const note = e instanceof ApiError && e.status === 409 ? tr("voice.notConfigured")
          : e instanceof ApiError && e.status === 429 && e.code === "busy" ? tr("voice.offlineReadAloud.busy")
            : humanLine(e);
        toast.show(note, "danger");
      } finally {
        dispatchRef.current({ type: "sent" });
      }
    },
    [toast]
  );

  const dispatchRef = useRef<(ev: DictationEvent) => void>(() => undefined);
  const start = useCallback(async () => {
    const prefs = loadSettingsSync() ?? DEFAULT_SETTINGS;
    // Barge-in: stop reading a reply aloud so the microphone does not record the speaker.
    stopSpeaking();
    let s: MediaStream;
    try {
      s = await openMic(prefs.micDeviceId);
    } catch (e) {
      release();
      const key = micErrorKey(e);
      const text =
        key === "voice.micDenied"
          ? `${live.current.t("voice.input.deniedTitle")} ${live.current.t(micDeniedStepsKey(detectPlatform(navigator.userAgent)))}`
          : live.current.t(key);
      toast.show(text, "danger");
      dispatchRef.current({ type: "micFailed" });
      return;
    }
    stream.current = s;
    const before = stateRef.current;
    dispatchRef.current({ type: "micReady" });
    if (stateRef.current.phase !== "recording") {
      // Released while the microphone was opening: nothing to record.
      release();
      return;
    }
    try {
      const mime = pickRecordingMime((m) => MediaRecorder.isTypeSupported(m));
      const rec = new MediaRecorder(s, mime ? { mimeType: mime } : undefined);
      const chunks: Blob[] = [];
      rec.ondataavailable = (e) => e.data.size > 0 && chunks.push(e.data);
      rec.onstop = () => {
        release();
        const type = rec.mimeType || mime || "audio/webm";
        const blob = new Blob(chunks, { type });
        const keep = !discard.current && blob.size > 0;
        discard.current = false;
        dispatchRef.current({ type: "stopped", hasAudio: keep });
        if (keep) {
          void send(blob);
        }
      };
      recorder.current = rec;
      rec.start();
      timer.current = window.setTimeout(() => rec.state === "recording" && rec.stop(), MAX_RECORD_MS);
      const sec = before.phase === "starting" && before.via === "click" ? prefs.silenceStopSec : 0;
      const vad = sec > 0 ? createVad({ ...LISTEN_VAD, silenceMs: sec * 1000 }) : null;
      vad?.reset(performance.now());
      meter.current = watchLevel(s, (rms, now) => {
        setLevel(rms);
        const ev = vad?.feed(rms, now);
        if (ev === "speech_end" || ev === "no_speech" || ev === "too_long") {
          // Nobody spoke: stop without sending an empty clip to the service.
          discard.current = ev === "no_speech";
          dispatchRef.current({ type: "silence" });
        }
      });
    } catch (e) {
      release();
      toast.show(live.current.t(micErrorKey(e)), "danger");
      dispatchRef.current({ type: "micFailed" });
    }
  }, [release, send, toast]);

  dispatchRef.current = (ev: DictationEvent) => {
    const r = stepDictation(stateRef.current, ev);
    stateRef.current = r.state;
    setState(r.state);
    if (r.effect === "openMic") {
      void start();
    } else if (r.effect === "stopRecording") {
      if (recorder.current?.state === "recording") {
        recorder.current.stop();
      }
    } else if (r.effect === "discard") {
      discard.current = false;
      release();
    }
  };

  // Push-to-talk: only while switched on in Settings (read at each key, so a change applies at once).
  useEffect(() => {
    const onDown = (e: KeyboardEvent) => {
      const prefs = loadSettingsSync() ?? DEFAULT_SETTINGS;
      if (prefs.pttEnabled && isPttPress(e, prefs.pttKey)) {
        e.preventDefault();
        dispatchRef.current({ type: "press", via: "ptt" });
      }
    };
    const onUp = (e: KeyboardEvent) => {
      const prefs = loadSettingsSync() ?? DEFAULT_SETTINGS;
      if (prefs.pttEnabled && e.code === prefs.pttKey) {
        dispatchRef.current({ type: "release" });
      }
    };
    // Losing focus while the key is down would never deliver the key-up: treat it as a release.
    const onBlur = () => dispatchRef.current({ type: "release" });
    window.addEventListener("keydown", onDown);
    window.addEventListener("keyup", onUp);
    window.addEventListener("blur", onBlur);
    return () => {
      window.removeEventListener("keydown", onDown);
      window.removeEventListener("keyup", onUp);
      window.removeEventListener("blur", onBlur);
    };
  }, []);

  useEffect(
    () => () => {
      discard.current = true;
      if (recorder.current?.state === "recording") {
        recorder.current.stop();
      }
      release();
    },
    [release]
  );

  if (!dictationSupported()) {
    return null;
  }

  const recording = state.phase === "recording";
  const prefs = loadSettingsSync() ?? DEFAULT_SETTINGS;
  const label = t(recording ? "voice.dictateStop" : "voice.dictate");
  const hint =
    recording && state.via === "ptt"
      ? t("voice.input.releaseToSend", { key: pttKeyLabel(prefs.pttKey) })
      : recording && prefs.silenceStopSec > 0
        ? t("voice.input.stopsOnSilence", { count: prefs.silenceStopSec })
        : undefined;
  return (
    <>
      <Button
        variant="ghost"
        size="sm"
        className={className}
        title={label}
        aria-label={label}
        aria-pressed={recording}
        disabled={state.phase === "sending"}
        data-testid="dictate"
        onClick={() => dispatchRef.current(state.phase === "idle" ? { type: "press", via: "click" } : { type: "click" })}
      >
        {state.phase === "sending" ? (
          <LoaderCircle aria-hidden="true" />
        ) : recording ? (
          <Square aria-hidden="true" />
        ) : (
          <Mic aria-hidden="true" />
        )}
      </Button>
      {recording ? <RecordingIndicator rms={level} text={t("voice.input.recording")} hint={hint} /> : null}
    </>
  );
}
