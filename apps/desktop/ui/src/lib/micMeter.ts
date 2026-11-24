import { rmsOf } from "./voiceCall/vad";

/**
 * Opens the chosen microphone; when that device is gone (unplugged) it falls back to the system
 * default instead of failing, so dictation keeps working.
 */
export async function openMic(deviceId: string): Promise<MediaStream> {
  if (deviceId) {
    try {
      return await navigator.mediaDevices.getUserMedia({ audio: { deviceId: { exact: deviceId } } });
    } catch (e) {
      const name = typeof e === "object" && e !== null && "name" in e ? String((e as { name: unknown }).name) : "";
      if (name !== "OverconstrainedError" && name !== "NotFoundError") {
        throw e;
      }
    }
  }
  return navigator.mediaDevices.getUserMedia({ audio: true });
}

export interface MicMeter {
  stop(): void;
}

/**
 * Reads the loudness (RMS 0..1) of a stream about 20 times a second. Only a number per frame is
 * passed on: no audio is kept or sent anywhere.
 */
export function watchLevel(stream: MediaStream, onLevel: (rms: number, nowMs: number) => void): MicMeter {
  const Ctx: typeof AudioContext | undefined =
    window.AudioContext ?? (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
  if (!Ctx) {
    return { stop() {} };
  }
  const ctx = new Ctx();
  const source = ctx.createMediaStreamSource(stream);
  const analyser = ctx.createAnalyser();
  analyser.fftSize = 1024;
  source.connect(analyser);
  const buf = new Float32Array(analyser.fftSize);
  const id = window.setInterval(() => {
    analyser.getFloatTimeDomainData(buf);
    onLevel(rmsOf(buf), performance.now());
  }, 50);
  return {
    stop() {
      window.clearInterval(id);
      source.disconnect();
      void ctx.close().catch(() => undefined);
    }
  };
}

/** The microphones this computer offers. Labels are empty until the owner has allowed the microphone once. */
export async function listMics(): Promise<MediaDeviceInfo[]> {
  if (!navigator.mediaDevices?.enumerateDevices) {
    return [];
  }
  return (await navigator.mediaDevices.enumerateDevices()).filter((d) => d.kind === "audioinput");
}
