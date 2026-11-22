import { LoaderCircle, Mic, Square } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { ApiError } from "../api/client";
import { dictationSupported, pickRecordingMime } from "../lib/dictation";
import { humanLine } from "../lib/humanError";
import { micErrorKey } from "../lib/micError";
import { stopSpeaking } from "../lib/speaker";
import { transcribePayload } from "../lib/voiceAudio";
import { useDaemon } from "../state/DaemonContext";
import { useToast } from "./Toast";

/** Longest single dictation; the recording stops by itself (keeps uploads small). */
const MAX_RECORD_MS = 120_000;

type Phase = "idle" | "recording" | "sending";

/**
 * Push-to-talk dictation for the composer: click to record, click again to stop; the recording is
 * transcribed by the configured service (Settings > General) and handed to `onText`. The microphone
 * is only opened by this click (the WebView asks the owner for permission) and closed right after.
 */
export function DictateButton({ onText, className }: { onText: (text: string) => void; className?: string }) {
  const { t, i18n } = useTranslation();
  const { client } = useDaemon();
  const toast = useToast();
  const [phase, setPhase] = useState<Phase>("idle");
  const recorder = useRef<MediaRecorder | null>(null);
  const stream = useRef<MediaStream | null>(null);
  const timer = useRef<number | null>(null);

  const release = () => {
    if (timer.current !== null) {
      window.clearTimeout(timer.current);
      timer.current = null;
    }
    stream.current?.getTracks().forEach((tr) => tr.stop());
    stream.current = null;
  };
  useEffect(() => () => {
    recorder.current?.stop();
    release();
  }, []);

  if (!dictationSupported()) {
    return null;
  }

  const start = async () => {
    // Barge-in: stop reading a reply aloud so the microphone does not record the speaker.
    stopSpeaking();
    try {
      const s = await navigator.mediaDevices.getUserMedia({ audio: true });
      stream.current = s;
      const mime = pickRecordingMime((m) => MediaRecorder.isTypeSupported(m));
      const rec = new MediaRecorder(s, mime ? { mimeType: mime } : undefined);
      const chunks: Blob[] = [];
      rec.ondataavailable = (e) => e.data.size > 0 && chunks.push(e.data);
      rec.onstop = () => {
        release();
        const type = rec.mimeType || mime || "audio/webm";
        void send(new Blob(chunks, { type }));
      };
      recorder.current = rec;
      rec.start();
      timer.current = window.setTimeout(() => rec.state === "recording" && rec.stop(), MAX_RECORD_MS);
      setPhase("recording");
    } catch (e) {
      release();
      toast.show(t(micErrorKey(e)), "danger");
      setPhase("idle");
    }
  };

  const send = async (blob: Blob) => {
    if (!client || blob.size === 0) {
      setPhase("idle");
      return;
    }
    setPhase("sending");
    try {
      const provider = await client
        .getVoice()
        .then((c) => c.stt_provider)
        .catch(() => undefined);
      const out = await client.transcribe({
        ...(await transcribePayload(blob, provider)),
        language: i18n.language.split("-")[0]
      });
      onText(out.text);
    } catch (e) {
      toast.show(e instanceof ApiError && e.status === 409 ? t("voice.notConfigured") : humanLine(e), "danger");
    } finally {
      setPhase("idle");
    }
  };

  const label = t(phase === "recording" ? "voice.dictateStop" : "voice.dictate");
  return (
    <Button
      variant="ghost"
      size="sm"
      className={className}
      title={label}
      aria-label={label}
      aria-pressed={phase === "recording"}
      disabled={phase === "sending"}
      data-testid="dictate"
      onClick={() => (phase === "recording" ? recorder.current?.stop() : void start())}
    >
      {phase === "sending" ? <LoaderCircle aria-hidden="true" /> : phase === "recording" ? <Square aria-hidden="true" /> : <Mic aria-hidden="true" />}
    </Button>
  );
}
