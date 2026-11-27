import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { isMockMode } from "../../api/mockMode";
import type { LiveVoiceEvent } from "../../api/types";
import { micErrorKey } from "../../lib/micError";
import { ownerKey } from "../../lib/ownerKey";
import { markdownToSpeech } from "../../lib/speechText";
import { detectSecrets } from "../../lib/secretDetect";
import { callMicrophone, STEP_DEADLINE_MS, withDeadline } from "../../lib/voiceCall/readiness";
import { createChunker, base64ToBytes, frameToPcm16, levelOf, OUTPUT_RATE } from "../../lib/liveVoice/pcm";
import { createPlaybackQueue, type PlaybackQueue, type PlaybackSink } from "../../lib/liveVoice/playback";
import {
  initialLiveState,
  isOpen,
  liveFailureNotice,
  liveReducer,
  transcriptTurns,
  type LiveEvent,
  type LiveNoticeKey,
  type LiveState
} from "../../lib/liveVoice/machine";
import { liveVoiceNotice } from "../../lib/liveVoice/readiness";
import { loadSettingsSync } from "../../store/settingsStore";
import type { RunState } from "../../state/timeline";
import { useDaemon, useLocalDaemonClient } from "../../state/DaemonContext";
import { useToast } from "../Toast";

const LEVEL_MS = 50;
/** Microphone audio waiting for the daemon beyond this (3 s) is dropped: late speech is useless. */
const MAX_OUTBOX = 30;
/** Consecutive failed audio uploads that end the conversation. */
const MAX_UPLOAD_FAILURES = 5;
/** A handover that never finishes is answered after this long; the bot keeps working in the chat. */
const HANDOVER_TIMEOUT_MS = 15 * 60_000;
const MAX_RESULT_CHARS = 4_000;

export interface LiveVoice {
  state: LiveState;
  /** Loudness 0..1 of the microphone and of the voice model's speech; read by the panel's animation. */
  levels: { current: { mic: number; bot: number } };
  start: () => void;
  /** End now and write the conversation into the chat. */
  end: () => void;
  /** Close the ended/failed card. */
  dismiss: () => void;
  toggleMute: () => void;
  /** Silence the rest of the voice model's current turn (speaking over it works too). */
  stopTalking: () => void;
  supported: boolean;
  blockedReason: LiveNoticeKey | "voice.live.blockedByCall" | null;
}

export interface LiveVoiceOptions {
  chatId: string;
  botId: string | null | undefined;
  botName: string;
  runs: readonly RunState[];
  /** The bot's run waits for a click on the approval card. */
  awaitingApproval: boolean;
  runActive: boolean;
  /** The classic voice call is open: only one voice feature at a time. */
  callOpen: boolean;
  /** Sends the request as an ordinary user message (same permission engine as typing). */
  sendText: (text: string) => Promise<void>;
  /** The transcript was written into the chat: reload its messages. */
  onSaved: () => void;
}

interface Handover {
  callId: string;
  known: Set<string>;
  runId: string | null;
  noted: boolean;
  timer: number;
}

/** The text of what a run said: plain words for the voice model to pass on. */
function runAnswer(run: RunState, labels: { code: string; table: string }): string {
  const md = run.items.flatMap((i) => (i.kind === "text" ? [i.text] : [])).join("\n\n");
  return markdownToSpeech(md, labels).slice(0, MAX_RESULT_CHARS);
}

/** Speech of the voice model through the page's audio context. */
function contextSink(ctx: AudioContext, dest: AudioNode): PlaybackSink {
  return {
    now: () => ctx.currentTime,
    play(samples, at, onEnded) {
      const buffer = ctx.createBuffer(1, samples.length, OUTPUT_RATE);
      buffer.getChannelData(0).set(samples);
      const src = ctx.createBufferSource();
      src.buffer = buffer;
      src.connect(dest);
      src.onended = onEnded;
      src.start(at);
      return {
        stop() {
          src.onended = null;
          try {
            src.stop();
          } catch {
            // Never started or already over.
          }
          src.disconnect();
        }
      };
    }
  };
}

async function openMicrophone(deviceId: string): Promise<MediaStream> {
  const audio = { echoCancellation: true, noiseSuppression: true, autoGainControl: true, channelCount: 1 };
  if (deviceId) {
    try {
      return await navigator.mediaDevices.getUserMedia({ audio: { ...audio, deviceId: { exact: deviceId } } });
    } catch (e) {
      const name = e instanceof Error ? e.name : "";
      if (name !== "OverconstrainedError" && name !== "NotFoundError") {
        throw e;
      }
    }
  }
  return navigator.mediaDevices.getUserMedia({ audio });
}

/**
 * Orchestrates the live voice conversation (docs/design/voice-live.md): the microphone goes to the
 * daemon as 16 kHz PCM16, the voice model's speech comes back and is played, and whatever needs the
 * bot is handed over as an ordinary chat message (so the permission engine and the approval cards
 * decide, never this code). When the conversation ends its text is written into the chat. Audio is
 * never kept here: it only passes through memory.
 */
export function useLiveVoice(options: LiveVoiceOptions): LiveVoice {
  const { t, i18n } = useTranslation();
  const toast = useToast();
  const { client } = useDaemon();
  const local = useLocalDaemonClient();
  const clientRef = useRef(client);
  clientRef.current = client;
  const mock = isMockMode();
  const supported =
    mock ||
    (typeof navigator !== "undefined" &&
      !!navigator.mediaDevices?.getUserMedia &&
      (typeof AudioContext !== "undefined" || "webkitAudioContext" in window));

  const [state, setState] = useState<LiveState>(initialLiveState);
  const stateRef = useRef(state);
  const dispatch = useCallback((e: LiveEvent) => {
    stateRef.current = liveReducer(stateRef.current, e);
    setState(stateRef.current);
  }, []);

  const opts = useRef(options);
  opts.current = options;
  const labels = { code: t("voice.codeOmitted"), table: t("voice.tableOmitted") };
  const labelsRef = useRef(labels);
  labelsRef.current = labels;

  const levels = useRef({ mic: 0, bot: 0 });
  /** Everything that must be released when the conversation ends. */
  const live = useRef<{
    gen: number;
    abort: AbortController | null;
    sessionId: string | null;
    stream: MediaStream | null;
    ctx: AudioContext | null;
    proc: ScriptProcessorNode | null;
    analyser: AnalyserNode | null;
    player: PlaybackQueue | null;
    levelTimer: number | null;
    idleTimer: number | null;
    silenced: boolean;
    chatId: string;
  }>({ gen: 0, abort: null, sessionId: null, stream: null, ctx: null, proc: null, analyser: null, player: null, levelTimer: null, idleTimer: null, silenced: false, chatId: options.chatId });
  const handovers = useRef(new Map<string, Handover>());
  const outbox = useRef<Uint8Array[]>([]);
  const pumping = useRef(false);
  const uploadFailures = useRef(0);
  const runsRef = useRef(options.runs);
  runsRef.current = options.runs;

  const blockedReason: LiveVoice["blockedReason"] = !supported
    ? "voice.call.unsupported"
    : !local
      ? "voice.live.notice.daemonUnavailable"
      : options.callOpen
        ? "voice.live.blockedByCall"
        : options.runActive
          ? "voice.live.notice.runActive"
          : null;

  const releaseAudio = useCallback(() => {
    const l = live.current;
    if (l.levelTimer !== null) {
      window.clearInterval(l.levelTimer);
      l.levelTimer = null;
    }
    if (l.idleTimer !== null) {
      window.clearTimeout(l.idleTimer);
      l.idleTimer = null;
    }
    l.player?.flush();
    l.player = null;
    if (l.proc) {
      l.proc.onaudioprocess = null;
      l.proc.disconnect();
      l.proc = null;
    }
    l.stream?.getTracks().forEach((track) => track.stop());
    l.stream = null;
    void l.ctx?.close().catch(() => undefined);
    l.ctx = null;
    l.analyser = null;
    outbox.current = [];
    levels.current = { mic: 0, bot: 0 };
  }, []);

  const clearHandovers = useCallback(() => {
    for (const h of handovers.current.values()) {
      window.clearTimeout(h.timer);
    }
    handovers.current.clear();
  }, []);

  /** Close everything and write the conversation into the chat. Safe to call twice. */
  const finish = useCallback(
    (reason: "limit" | "idle" | "error" | "closed") => {
      const l = live.current;
      if (!isOpen(stateRef.current.phase)) {
        return;
      }
      const sessionId = l.sessionId;
      l.sessionId = null;
      l.gen += 1;
      l.abort?.abort();
      l.abort = null;
      releaseAudio();
      clearHandovers();
      dispatch({ type: "ENDED", reason });
      const turns = transcriptTurns(stateRef.current.lines);
      const target = clientRef.current;
      if (sessionId) {
        void local?.liveVoiceClose(sessionId).catch(() => undefined);
      }
      if (turns.length > 0 && target) {
        target
          .saveVoiceTranscript(l.chatId, turns)
          .then(() => opts.current.onSaved())
          .catch(() => toast.show(t("voice.live.saveFailed"), "danger"));
      }
      // A conversation the owner ended needs no card; one that ran out or broke keeps its notice.
      if (reason === "closed") {
        dispatch({ type: "RESET" });
      }
    },
    [clearHandovers, dispatch, local, releaseAudio, t, toast]
  );

  const finishRef = useRef(finish);
  finishRef.current = finish;

  const answer = useCallback(
    (callId: string, text: string) => {
      const l = live.current;
      const h = handovers.current.get(callId);
      if (!h) {
        return;
      }
      window.clearTimeout(h.timer);
      handovers.current.delete(callId);
      dispatch({ type: "TOOL_DONE" });
      if (l.sessionId) {
        void local?.liveVoiceToolResult(l.sessionId, callId, text).catch(() => undefined);
      }
    },
    [dispatch, local]
  );

  /** The voice model asks the bot to do something: it becomes an ordinary chat message. */
  const handOver = useCallback(
    (callId: string, request: string) => {
      const l = live.current;
      if (handovers.current.has(callId)) {
        return;
      }
      dispatch({ type: "TOOL_START" });
      const entry: Handover = {
        callId,
        known: new Set(runsRef.current.map((r) => r.id)),
        runId: null,
        noted: false,
        timer: window.setTimeout(
          () => answer(callId, "The bot is still working on it. It will keep going in the chat; tell the owner to check there."),
          HANDOVER_TIMEOUT_MS
        )
      };
      handovers.current.set(callId, entry);
      if (opts.current.runActive) {
        answer(callId, "The bot is busy with another task right now. Ask the owner to wait for it to finish.");
        return;
      }
      // Nobody is looking at a screen to confirm a warning: a credential is never passed on.
      if (detectSecrets(request).length > 0) {
        answer(callId, "That request contains something that looks like a password or key, so it was not passed on. Ask the owner to type it in the chat instead.");
        return;
      }
      void opts.current.sendText(request).catch(() => {
        if (l.sessionId) {
          answer(callId, "The request could not be sent to the bot. Tell the owner and ask them to try again.");
        }
      });
    },
    [answer, dispatch]
  );

  const onEvent = useCallback(
    (ev: LiveVoiceEvent, gen: number) => {
      const l = live.current;
      if (gen !== l.gen) {
        return;
      }
      switch (ev.type) {
        case "ready":
          l.sessionId = ev.session_id;
          dispatch({ type: "READY", maxSeconds: ev.max_seconds });
          break;
        case "audio": {
          if (l.silenced) {
            break;
          }
          const bytes = ev.data ? base64ToBytes(ev.data) : new Uint8Array(0);
          if (bytes.length > 0) {
            l.player?.push(bytes);
          }
          dispatch({ type: "AUDIO" });
          if (mock) {
            // No audio device in the mock app: pretend the speech ends a moment after the last chunk.
            if (l.idleTimer !== null) window.clearTimeout(l.idleTimer);
            l.idleTimer = window.setTimeout(() => dispatch({ type: "PLAYBACK_IDLE" }), 1200);
          }
          break;
        }
        case "you":
          dispatch({ type: "YOU", text: ev.text });
          break;
        case "bot":
          if (!l.silenced) {
            dispatch({ type: "BOT", text: ev.text });
          }
          break;
        case "turn_complete":
          l.silenced = false;
          dispatch({ type: "TURN_COMPLETE" });
          break;
        case "interrupted":
          l.silenced = false;
          l.player?.flush();
          dispatch({ type: "INTERRUPTED" });
          break;
        case "tool_call":
          handOver(ev.call_id, ev.request);
          break;
        case "tool_cancelled":
          for (const id of ev.call_ids) {
            // The model no longer waits: the bot keeps working in the chat, nothing is sent back.
            const h = handovers.current.get(id);
            if (h) {
              window.clearTimeout(h.timer);
              handovers.current.delete(id);
              dispatch({ type: "TOOL_DONE" });
            }
          }
          break;
        case "reconnecting":
          dispatch({ type: "RECONNECTING" });
          break;
        case "ended":
          finish(ev.reason);
          break;
      }
    },
    [dispatch, finish, handOver, mock]
  );

  /** Upload microphone chunks one after another so the model hears them in order. */
  const pump = useCallback(async () => {
    if (pumping.current) {
      return;
    }
    pumping.current = true;
    const gen = live.current.gen;
    try {
      while (outbox.current.length > 0 && gen === live.current.gen) {
        const chunk = outbox.current.shift()!;
        const sid = live.current.sessionId;
        if (!sid || !local) {
          break;
        }
        try {
          await local.liveVoiceAudio(sid, chunk, live.current.abort?.signal);
          uploadFailures.current = 0;
        } catch {
          uploadFailures.current += 1;
          if (uploadFailures.current >= MAX_UPLOAD_FAILURES && gen === live.current.gen) {
            finish("error");
          }
        }
      }
    } finally {
      pumping.current = false;
    }
  }, [finish, local]);

  const start = useCallback(() => {
    const phase = stateRef.current.phase;
    if (phase !== "idle" && phase !== "failed" && phase !== "ended") {
      return;
    }
    const l = live.current;
    const gen = ++l.gen;
    l.chatId = opts.current.chatId;
    l.silenced = false;
    dispatch({ type: "START" });
    const fail = (notice: LiveNoticeKey) => {
      if (gen !== l.gen) return;
      l.gen += 1;
      l.abort?.abort();
      l.abort = null;
      releaseAudio();
      clearHandovers();
      dispatch({ type: "FAILED", notice });
    };
    if (blockedReason) {
      fail(blockedReason === "voice.live.blockedByCall" ? "voice.live.notice.runActive" : blockedReason);
      return;
    }
    void (async () => {
      if (!local) return;
      try {
        const config = await withDeadline(local.getVoice());
        const notice = liveVoiceNotice(config);
        if (notice) return fail(notice);
        if (!(await withDeadline(ownerKey(local.baseUrl)))) return fail("voice.live.notice.ownerRequired");
      } catch (e) {
        return fail(liveFailureNotice(e));
      }
      if (gen !== l.gen) return;
      if (!mock) {
        try {
          const stream = await callMicrophone(() => openMicrophone(loadSettingsSync()?.micDeviceId ?? ""));
          if (gen !== l.gen) {
            stream.getTracks().forEach((track) => track.stop());
            return;
          }
          l.stream = stream;
          const Ctx = window.AudioContext ?? (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
          const ctx = new Ctx();
          l.ctx = ctx;
          await withDeadline(ctx.resume(), STEP_DEADLINE_MS);
          if (gen !== l.gen) return;
          // Speech out: queue -> analyser (level) -> speakers.
          const analyser = ctx.createAnalyser();
          analyser.fftSize = 512;
          analyser.connect(ctx.destination);
          l.analyser = analyser;
          l.player = createPlaybackQueue(contextSink(ctx, analyser), () => dispatch({ type: "PLAYBACK_IDLE" }));
          // Microphone in: frames -> 16 kHz PCM16 -> 100 ms chunks (nothing while muted).
          const chunker = createChunker();
          const source = ctx.createMediaStreamSource(stream);
          const proc = ctx.createScriptProcessor(2048, 1, 1);
          proc.onaudioprocess = (frame) => {
            const samples = frame.inputBuffer.getChannelData(0);
            if (stateRef.current.muted || !l.sessionId) {
              levels.current.mic = 0;
              return;
            }
            levels.current.mic = levelOf(samples);
            for (const chunk of chunker.push(frameToPcm16([samples], ctx.sampleRate))) {
              outbox.current.push(chunk);
            }
            if (outbox.current.length > MAX_OUTBOX) {
              outbox.current.splice(0, outbox.current.length - MAX_OUTBOX);
            }
            void pump();
          };
          const silent = ctx.createGain();
          silent.gain.value = 0;
          source.connect(proc);
          proc.connect(silent);
          silent.connect(ctx.destination);
          l.proc = proc;
        } catch (e) {
          return fail(e instanceof Error && e.message === "voice.call.micTimeout" ? "voice.call.micTimeout" : micErrorKey(e));
        }
      }
      uploadFailures.current = 0;
      // The speech level of the orb, 20 times a second.
      const buf = new Float32Array(512);
      l.levelTimer = window.setInterval(() => {
        if (l.analyser) {
          l.analyser.getFloatTimeDomainData(buf);
          levels.current.bot = levelOf(buf);
        } else if (mock) {
          const now = performance.now() / 1000;
          const s = stateRef.current;
          levels.current.mic = s.muted ? 0 : 0.2 + 0.15 * Math.sin(now * 3);
          levels.current.bot = s.botSpeaking ? 0.4 + 0.3 * Math.sin(now * 7) : 0;
        }
      }, LEVEL_MS);
      const abort = new AbortController();
      l.abort = abort;
      if (stateRef.current.muted) {
        l.stream?.getAudioTracks().forEach((track) => (track.enabled = false));
      }
      let sawEnd = false;
      local
        .liveVoiceOpen(
          { language: i18n.language.split("-")[0], assistant_name: opts.current.botName || undefined },
          (ev) => {
            if (ev.type === "ended") sawEnd = true;
            onEvent(ev, gen);
          },
          abort.signal
        )
        .then(() => {
          // The stream closed without an `ended` event: the daemon went away.
          if (gen === l.gen && !sawEnd) {
            if (stateRef.current.phase === "starting") fail("voice.live.notice.daemonUnavailable");
            else finish("error");
          }
        })
        .catch((e: unknown) => {
          if (gen !== l.gen) return;
          if (stateRef.current.phase === "starting") fail(liveFailureNotice(e));
          else finish("error");
        });
    })();
  }, [blockedReason, clearHandovers, dispatch, finish, i18n.language, local, mock, onEvent, pump, releaseAudio]);

  const end = useCallback(() => {
    const phase = stateRef.current.phase;
    if (phase === "starting") {
      live.current.gen += 1;
      live.current.abort?.abort();
      releaseAudio();
      dispatch({ type: "RESET" });
      return;
    }
    finish("closed");
  }, [dispatch, finish, releaseAudio]);

  const dismiss = useCallback(() => dispatch({ type: "RESET" }), [dispatch]);

  const toggleMute = useCallback(() => {
    dispatch({ type: "TOGGLE_MUTE" });
    // Muting closes the audio track itself, so nothing is captured while muted.
    live.current.stream?.getAudioTracks().forEach((track) => (track.enabled = !stateRef.current.muted));
  }, [dispatch]);

  const stopTalking = useCallback(() => {
    const l = live.current;
    if (!stateRef.current.botSpeaking) {
      return;
    }
    // The rest of this turn is dropped until the model starts its next one.
    l.silenced = true;
    l.player?.flush();
    dispatch({ type: "STOP_TALKING" });
  }, [dispatch]);

  // Follow the runs started by handovers: answer the voice model when each one finishes, and tell
  // it once when a run waits for a click on the approval card. Approving stays a click.
  useEffect(() => {
    const claimed = new Set([...handovers.current.values()].flatMap((h) => (h.runId ? [h.runId] : [])));
    let waiting = false;
    for (const h of [...handovers.current.values()]) {
      const run = h.runId
        ? options.runs.find((r) => r.id === h.runId)
        : options.runs.find(
            (r) => !h.known.has(r.id) && !claimed.has(r.id) && !r.parentRunId && r.botId === (options.botId ?? r.botId)
          );
      if (!run) {
        continue;
      }
      h.runId = run.id;
      claimed.add(run.id);
      if (run.finishedMs !== undefined) {
        const text = runAnswer(run, labelsRef.current);
        answer(
          h.callId,
          run.error
            ? "The bot could not finish that request. Tell the owner it failed; the details are in the chat."
            : text || "The bot finished and had nothing to add."
        );
      } else if (options.awaitingApproval) {
        waiting = true;
        if (!h.noted && live.current.sessionId) {
          h.noted = true;
          void local
            ?.liveVoiceNote(live.current.sessionId, "The bot is waiting for the owner to approve something on the card in the chat. Tell the owner to look at the screen; you cannot approve for them and approvals cannot be given by voice.")
            .catch(() => undefined);
        }
      }
    }
    if (isOpen(stateRef.current.phase) && stateRef.current.approvalWaiting !== waiting) {
      dispatch({ type: "APPROVAL", waiting });
    }
  }, [options.runs, options.awaitingApproval, options.botId, answer, dispatch, local]);

  // The microphone never outlives the conversation or the chat that opened it.
  useEffect(
    () => () => {
      if (isOpen(stateRef.current.phase)) {
        finishRef.current("closed");
      } else {
        live.current.gen += 1;
        live.current.abort?.abort();
        releaseAudio();
      }
    },
    // Unmount only.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    []
  );

  return { state, levels, start, end, dismiss, toggleMute, stopTalking, supported, blockedReason };
}
