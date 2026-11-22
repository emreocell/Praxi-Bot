import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { isMockMode } from "../../api/mockMode";
import { effectiveVoice } from "../../lib/botVoice";
import { dictationSupported, pickRecordingMime } from "../../lib/dictation";
import { micErrorKey } from "../../lib/micError";
import { speechSupported } from "../../lib/speaker";
import { markdownToSpeech } from "../../lib/speechText";
import { transcribePayload } from "../../lib/voiceAudio";
import { callReducer, initialCallState, type CallState } from "../../lib/voiceCall/callMachine";
import { cancelQueuedSpeech, queueSpeech, setDrainedHandler } from "../../lib/voiceCall/callSpeaker";
import { takeSpeakable } from "../../lib/voiceCall/sentences";
import { callMicrophone, voiceConfigNotice, voiceFailureNotice } from "../../lib/voiceCall/readiness";
import { ownerKey } from "../../lib/ownerKey";
import { BARGE_IN_VAD, createVad, LISTEN_VAD, rmsOf } from "../../lib/voiceCall/vad";
import { DEFAULT_SETTINGS, loadSettingsSync } from "../../store/settingsStore";
import type { RunState } from "../../state/timeline";
import { useDaemon } from "../../state/DaemonContext";

const FRAME_MS = 50;

export interface VoiceCall {
  state: CallState;
  /** Smoothed microphone loudness 0..1 (0 when the microphone is closed or muted). */
  level: number;
  /** What was heard and what the bot says; kept in memory only, cleared when the call ends. */
  captions: { you: string; bot: string };
  start: () => void;
  end: () => void;
  interrupt: () => void;
  toggleMute: () => void;
  togglePushToTalk: () => void;
  toggleCaptions: () => void;
  pushToTalkDown: () => void;
  pushToTalkUp: () => void;
  /** Mock mode only: pretend the owner said something (no microphone needed). */
  simulateSpeech: () => void;
  supported: boolean;
  blockedReason: "voice.call.unsupported" | "voice.call.daemonUnavailable" | "voice.call.runActive" | null;
}

export interface VoiceCallOptions {
  botId: string | null | undefined;
  runs: readonly RunState[];
  /** The run waits for a click on the approval card. */
  awaitingApproval: boolean;
  runActive: boolean;
  /** Sends the transcribed text as an ordinary user message (same permission engine as typing). */
  sendText: (text: string) => Promise<void>;
}

/** Everything the bot said in a run so far, as speakable text. */
function spokenText(run: RunState, labels: { code: string; table: string }): string {
  const md = run.items.flatMap((i) => (i.kind === "text" ? [i.text] : [])).join("\n\n");
  return markdownToSpeech(md, labels);
}

/**
 * Orchestrates the hands-free call (docs/design/voice-call-mode.md): microphone + energy VAD in,
 * one recording per utterance to the dictation call, the text as a normal message, the reply read
 * aloud sentence by sentence. The microphone is opened by `start` and released by `end` (or when
 * the component unmounts); audio lives in memory only until it is transcribed.
 */
export function useVoiceCall({ botId, runs, awaitingApproval, runActive, sendText }: VoiceCallOptions): VoiceCall {
  const { t, i18n } = useTranslation();
  const { client } = useDaemon();
  const mock = isMockMode();
  const supported = mock || dictationSupported();
  const [state, dispatch] = useReducer(callReducer, undefined, () => initialCallState(!speechSupported()));
  const [level, setLevel] = useState(0);
  const [captions, setCaptions] = useState({ you: "", bot: "" });

  const stateRef = useRef(state);
  stateRef.current = state;
  const stream = useRef<MediaStream | null>(null);
  const audioCtx = useRef<AudioContext | null>(null);
  const analyser = useRef<AnalyserNode | null>(null);
  const timer = useRef<number | null>(null);
  const recorder = useRef<MediaRecorder | null>(null);
  const chunks = useRef<Blob[]>([]);
  const listenVad = useRef(createVad(LISTEN_VAD));
  const bargeVad = useRef(createVad(BARGE_IN_VAD));
  const callGen = useRef(0);
  /** Reply tracking of the current turn. */
  const turn = useRef<{ known: Set<string>; runId: string | null; spokenLen: number } | null>(null);
  const runsRef = useRef(runs);
  runsRef.current = runs;
  const busyRef = useRef(runActive);
  busyRef.current = runActive;
  const blockedReason = !supported ? "voice.call.unsupported" : !client ? "voice.call.daemonUnavailable" : runActive ? "voice.call.runActive" : null;
  const labels = { code: t("voice.codeOmitted"), table: t("voice.tableOmitted") };
  const labelsRef = useRef(labels);
  labelsRef.current = labels;

  const stopRecorder = useCallback((): Blob | null => {
    const rec = recorder.current;
    recorder.current = null;
    if (!rec) {
      return null;
    }
    const parts = chunks.current;
    chunks.current = [];
    rec.ondataavailable = null;
    rec.onstop = null;
    try {
      if (rec.state !== "inactive") {
        rec.stop();
      }
    } catch {
      // Already stopped.
    }
    return parts.length ? new Blob(parts, { type: rec.mimeType || "audio/webm" }) : null;
  }, []);

  /** Start a fresh recording of this listening turn. */
  const startRecorder = useCallback(() => {
    const s = stream.current;
    if (!s || recorder.current) {
      return;
    }
    const mime = pickRecordingMime((m) => MediaRecorder.isTypeSupported(m));
    const rec = new MediaRecorder(s, mime ? { mimeType: mime } : undefined);
    chunks.current = [];
    rec.ondataavailable = (e) => e.data.size > 0 && chunks.current.push(e.data);
    recorder.current = rec;
    // A timeslice keeps `chunks` filled so a stop returns everything recorded so far.
    rec.start(250);
  }, []);

  const releaseMic = useCallback(() => {
    if (timer.current !== null) {
      window.clearInterval(timer.current);
      timer.current = null;
    }
    stopRecorder();
    stream.current?.getTracks().forEach((tr) => tr.stop());
    stream.current = null;
    void audioCtx.current?.close().catch(() => undefined);
    audioCtx.current = null;
    analyser.current = null;
    setLevel(0);
  }, [stopRecorder]);

  const end = useCallback(() => {
    callGen.current += 1;
    setDrainedHandler(null);
    cancelQueuedSpeech();
    turn.current = null;
    releaseMic();
    setCaptions({ you: "", bot: "" });
    dispatch({ type: "END" });
  }, [releaseMic]);

  const interrupt = useCallback(() => {
    if (stateRef.current.phase !== "speaking") {
      return;
    }
    cancelQueuedSpeech();
    // The rest of this reply is dropped: stop following the run for speech.
    if (turn.current) {
      turn.current.runId = "interrupted";
    }
    dispatch({ type: "INTERRUPT" });
  }, []);

  /** Transcribe one utterance, then send it as a normal message. */
  const processUtterance = useCallback(
    async (blob: Blob | null) => {
      const gen = callGen.current;
      if (!client || !blob || blob.size === 0) {
        dispatch({ type: "TRANSCRIBED", empty: true });
        return;
      }
      let text: string;
      try {
        const provider = (await client.getVoice()).stt_provider;
        const out = await client.transcribe({
          ...(await transcribePayload(blob, provider, { mock })),
          language: i18n.language.split("-")[0]
        });
        text = out.text.trim();
      } catch (e) {
        if (gen === callGen.current) {
          dispatch({ type: "FAILED", notice: voiceFailureNotice(e) });
        }
        return;
      }
      if (gen !== callGen.current) {
        return;
      }
      if (!text) {
        dispatch({ type: "TRANSCRIBED", empty: true });
        return;
      }
      if (busyRef.current) {
        dispatch({ type: "FAILED", notice: "voice.call.runActive" });
        return;
      }
      setCaptions({ you: text, bot: "" });
      turn.current = { known: new Set(runsRef.current.map((r) => r.id)), runId: null, spokenLen: 0 };
      dispatch({ type: "TRANSCRIBED", empty: false });
      try {
        await sendText(text);
      } catch (e) {
        if (gen === callGen.current) {
          turn.current = null;
          dispatch({ type: "FAILED", notice: voiceFailureNotice(e) });
        }
      }
    },
    [client, i18n.language, mock, sendText]
  );

  const finishUtterance = useCallback(() => {
    if (stateRef.current.phase !== "listening" || !stateRef.current.heard) {
      return;
    }
    const blob = stopRecorder();
    dispatch({ type: "SPEECH_END" });
    void processUtterance(blob);
  }, [processUtterance, stopRecorder]);

  // Microphone frames: level meter, end-of-speech detection, barge-in.
  const tick = useCallback(() => {
    const a = analyser.current;
    const s = stateRef.current;
    if (!a || s.muted) {
      setLevel(0);
      return;
    }
    const buf = new Float32Array(a.fftSize);
    a.getFloatTimeDomainData(buf);
    const rms = rmsOf(buf);
    setLevel((prev) => prev * 0.6 + Math.min(1, rms * 6) * 0.4);
    const now = performance.now();
    if (s.phase === "listening" && !s.pushToTalk) {
      const ev = listenVad.current.feed(rms, now);
      if (ev === "speech_start") {
        dispatch({ type: "SPEECH_START" });
      } else if (ev === "speech_end" || ev === "too_long") {
        finishUtterance();
      } else if (ev === "no_speech") {
        dispatch({ type: "NO_SPEECH" });
        // Drop the silent recording and start a new one (audio of silence is never kept).
        stopRecorder();
        startRecorder();
      }
    } else if (s.phase === "speaking" && bargeVad.current.feed(rms, now) === "speech_start") {
      interrupt();
    }
  }, [finishUtterance, interrupt, startRecorder, stopRecorder]);

  const start = useCallback(() => {
    if (stateRef.current.phase !== "idle" && stateRef.current.phase !== "failed") {
      return;
    }
    const gen = ++callGen.current;
    dispatch({ type: "START" });
    if (blockedReason) {
      dispatch({ type: "FAILED", notice: blockedReason });
      return;
    }
    void (async () => {
      try {
        if (!client) return;
        const config = await client.getVoice();
        if (gen !== callGen.current) return;
        const notice = voiceConfigNotice(config);
        if (notice) {
          dispatch({ type: "FAILED", notice });
          return;
        }
        const key = await ownerKey(client.baseUrl);
        if (gen !== callGen.current) return;
        if (!key) {
          dispatch({ type: "FAILED", notice: "voice.call.ownerRequired" });
          return;
        }
        if (busyRef.current) {
          dispatch({ type: "FAILED", notice: "voice.call.runActive" });
          return;
        }
      } catch (e) {
        if (gen === callGen.current) dispatch({ type: "FAILED", notice: voiceFailureNotice(e) });
        return;
      }
      if (mock) {
        dispatch({ type: "MIC_READY" });
        return;
      }
      try {
        // Same permission prompt as dictation; asked only because the owner opened a call.
        const s = await callMicrophone(() => navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true } }));
        if (gen !== callGen.current) {
          s.getTracks().forEach((tr) => tr.stop());
          return;
        }
        stream.current = s;
        const ctx = new AudioContext();
        audioCtx.current = ctx;
        const resumeTimer = window.setTimeout(() => {
          if (gen !== callGen.current) return;
          callGen.current += 1;
          releaseMic();
          dispatch({ type: "FAILED", notice: "voice.call.micTimeout" });
        }, 10_000);
        try { await ctx.resume(); }
        finally { window.clearTimeout(resumeTimer); }
        if (gen !== callGen.current) return;
        const src = ctx.createMediaStreamSource(s);
        const an = ctx.createAnalyser();
        an.fftSize = 1024;
        src.connect(an);
        analyser.current = an;
        timer.current = window.setInterval(tick, FRAME_MS);
        dispatch({ type: "MIC_READY" });
      } catch (e) {
        if (gen !== callGen.current) return;
        releaseMic();
        dispatch({ type: "FAILED", notice: e instanceof Error && e.message === "voice.call.micTimeout" ? "voice.call.micTimeout" : micErrorKey(e) });
      }
    })();
  }, [blockedReason, client, mock, releaseMic, tick]);

  // `tick` is re-created when its callbacks change; the interval must call the newest one.
  const tickRef = useRef(tick);
  tickRef.current = tick;
  useEffect(() => {
    if (timer.current !== null) {
      window.clearInterval(timer.current);
      timer.current = window.setInterval(() => tickRef.current(), FRAME_MS);
    }
  }, [tick]);

  // Phase entry work: a new recording and a fresh VAD per listening turn.
  useEffect(() => {
    if (state.phase === "listening") {
      listenVad.current.reset(performance.now());
      if (!mock) {
        startRecorder();
      }
    } else if (state.phase === "speaking") {
      bargeVad.current.reset(performance.now());
    } else if (state.phase === "failed" || state.phase === "idle") {
      releaseMic();
    }
    if (state.phase !== "listening") {
      // Leaving listening without an utterance (muted turn, approval): the recording is dropped.
      stopRecorder();
    }
  }, [mock, releaseMic, startRecorder, state.phase, stopRecorder]);

  // Mute closes the audio track itself, so nothing is captured while muted.
  useEffect(() => {
    stream.current?.getAudioTracks().forEach((tr) => (tr.enabled = !state.muted));
  }, [state.muted]);

  // Approval pause: the run waits for a click; the call only mirrors that.
  useEffect(() => {
    if (awaitingApproval && state.phase !== "idle" && state.phase !== "failed" && state.phase !== "starting" && state.phase !== "approval") {
      dispatch({ type: "APPROVAL_PENDING" });
    } else if (!awaitingApproval && state.phase === "approval") {
      dispatch({ type: "APPROVAL_CLEARED" });
    }
  }, [awaitingApproval, state.phase]);

  // Follow the bot's reply: speak complete sentences as they arrive.
  useEffect(() => {
    const tr = turn.current;
    const phase = state.phase;
    if (!tr || tr.runId === "interrupted" || (phase !== "thinking" && phase !== "speaking" && phase !== "approval")) {
      return;
    }
    const run = tr.runId
      ? runs.find((r) => r.id === tr.runId)
      : runs.find((r) => !tr.known.has(r.id) && !r.parentRunId && r.botId === (botId ?? r.botId));
    if (!run) {
      return;
    }
    tr.runId = run.id;
    const text = spokenText(run, labelsRef.current);
    const done = run.finishedMs !== undefined;
    if (tr.spokenLen > text.length) {
      tr.spokenLen = text.length;
    }
    const { chunks: next, end: newLen } = takeSpeakable(text, tr.spokenLen, done && !run.error);
    tr.spokenLen = newLen;
    if (next.length > 0) {
      setCaptions((c) => ({ ...c, bot: text }));
      if (speechSupported()) {
        const prefs = loadSettingsSync() ?? DEFAULT_SETTINGS;
        const v = effectiveVoice(botId, { voiceName: prefs.voiceName, rate: prefs.voiceRate });
        setDrainedHandler(() => dispatch({ type: "SPEECH_DRAINED" }));
        for (const c of next) {
          queueSpeech(c, { voiceName: v.voiceName, rate: v.rate, lang: i18n.language });
        }
        dispatch({ type: "SPEAKING" });
      }
    } else if (text) {
      setCaptions((c) => (c.bot === text ? c : { ...c, bot: text }));
    }
    if (done) {
      turn.current = null;
      if (run.error) {
        dispatch({ type: "REPLY_FAILED" });
      } else {
        dispatch({ type: "REPLY_DONE" });
      }
    }
  }, [runs, state.phase, botId, i18n.language]);

  // The microphone never outlives the call or the chat that opened it.
  useEffect(
    () => () => {
      callGen.current += 1;
      setDrainedHandler(null);
      cancelQueuedSpeech();
      releaseMic();
    },
    [releaseMic]
  );

  const simulateSpeech = useCallback(() => {
    if (!mock || stateRef.current.phase !== "listening") {
      return;
    }
    dispatch({ type: "SPEECH_START" });
    // Ensure the reducer saw `heard` before the end event.
    window.setTimeout(() => {
      dispatch({ type: "SPEECH_END" });
      void processUtterance(new Blob(["mock"], { type: "audio/webm" }));
    }, 400);
  }, [mock, processUtterance]);

  return {
    state,
    level,
    captions,
    start,
    end,
    interrupt,
    toggleMute: () => dispatch({ type: "TOGGLE_MUTE" }),
    togglePushToTalk: () => dispatch({ type: "TOGGLE_PUSH_TO_TALK" }),
    toggleCaptions: () => dispatch({ type: "TOGGLE_CAPTIONS" }),
    pushToTalkDown: () => {
      if (stateRef.current.phase === "listening" && stateRef.current.pushToTalk && !stateRef.current.muted) {
        dispatch({ type: "SPEECH_START" });
      }
    },
    pushToTalkUp: () => finishUtterance(),
    simulateSpeech,
    supported,
    blockedReason
  };
}
