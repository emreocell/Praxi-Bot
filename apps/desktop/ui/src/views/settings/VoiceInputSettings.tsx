import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import formStyles from "../../components/Form.module.css";
import { LevelBar } from "../../components/MicLevel";
import { SettingRow, Switch } from "../../components/SettingsUi";
import { dictationSupported } from "../../lib/dictation";
import { micErrorKey } from "../../lib/micError";
import { listMics, openMic, watchLevel, type MicMeter } from "../../lib/micMeter";
import {
  detectPlatform,
  levelVerdict,
  micDeniedStepsKey,
  pttKeyLabel,
  SILENCE_STOP_CHOICES
} from "../../lib/voiceInput";
import type { AppSettings } from "../../store/settingsStore";
import styles from "../SettingsModal.module.css";

const PTT_KEYS = [...Array.from({ length: 12 }, (_, i) => `F${i + 1}`), "Pause", "ScrollLock"];
const TEST_MS = 5000;

type TestState =
  | { phase: "idle" }
  | { phase: "running" }
  | { phase: "done"; verdict: "silent" | "quiet" | "good" }
  | { phase: "error"; reason: "voice.micDenied" | "voice.micMissing" | "voice.micBusy" };

/** Push-to-talk, microphone choice with a live level meter and test, and auto-stop on silence. */
export function VoiceInputSettings({
  settings,
  update
}: {
  settings: AppSettings;
  update: (patch: Partial<AppSettings>) => void;
}) {
  const { t } = useTranslation();
  const [mics, setMics] = useState<MediaDeviceInfo[]>([]);
  const [level, setLevel] = useState(0);
  const [test, setTest] = useState<TestState>({ phase: "idle" });
  const active = useRef<{ meter: MicMeter; stream: MediaStream; timer: number } | null>(null);

  const refresh = useCallback(() => void listMics().then(setMics).catch(() => setMics([])), []);
  useEffect(() => {
    refresh();
    const md = navigator.mediaDevices;
    md?.addEventListener?.("devicechange", refresh);
    return () => md?.removeEventListener?.("devicechange", refresh);
  }, [refresh]);

  const stopTest = useCallback(() => {
    const a = active.current;
    if (!a) return;
    window.clearTimeout(a.timer);
    a.meter.stop();
    a.stream.getTracks().forEach((tr) => tr.stop());
    active.current = null;
    setLevel(0);
  }, []);
  useEffect(() => stopTest, [stopTest]);

  if (!dictationSupported()) {
    return (
      <SettingRow title={t("voice.input.title")} description={t("voice.input.unsupported")}>
        <span />
      </SettingRow>
    );
  }

  const runTest = async () => {
    stopTest();
    setTest({ phase: "running" });
    try {
      const stream = await openMic(settings.micDeviceId);
      let peak = 0;
      const meter = watchLevel(stream, (rms) => {
        peak = Math.max(peak, rms);
        setLevel(rms);
      });
      const timer = window.setTimeout(() => {
        stopTest();
        setTest({ phase: "done", verdict: levelVerdict(peak) });
      }, TEST_MS);
      active.current = { meter, stream, timer };
      refresh(); // labels appear once the microphone is allowed
    } catch (e) {
      setTest({ phase: "error", reason: micErrorKey(e) });
    }
  };

  const known = mics.some((m) => m.deviceId === settings.micDeviceId);
  const steps = t(micDeniedStepsKey(detectPlatform(navigator.userAgent)));
  return (
    <>
      <SettingRow
        title={t("voice.input.ptt.title")}
        description={t("voice.input.ptt.description", { key: pttKeyLabel(settings.pttKey) })}
      >
        <Switch
          label={t("voice.input.ptt.title")}
          checked={settings.pttEnabled}
          onChange={(pttEnabled) => update({ pttEnabled })}
        />
        <select
          className={`${formStyles.select} ${styles.shortInput}`}
          aria-label={t("voice.input.ptt.key")}
          value={settings.pttKey}
          disabled={!settings.pttEnabled}
          onChange={(e) => update({ pttKey: e.target.value })}
        >
          {PTT_KEYS.map((k) => (
            <option key={k} value={k}>
              {pttKeyLabel(k)}
            </option>
          ))}
        </select>
      </SettingRow>
      <SettingRow stacked title={t("voice.input.mic.title")} description={t("voice.input.mic.description")}>
        <select
          className={formStyles.input}
          aria-label={t("voice.input.mic.title")}
          value={known ? settings.micDeviceId : ""}
          onChange={(e) => update({ micDeviceId: e.target.value })}
        >
          <option value="">{t("voice.input.mic.default")}</option>
          {mics
            .filter((m) => m.deviceId && m.deviceId !== "default")
            .map((m, i) => (
              <option key={m.deviceId} value={m.deviceId}>
                {m.label || t("voice.input.mic.unnamed", { n: i + 1 })}
              </option>
            ))}
        </select>
        <Button size="sm" disabled={test.phase === "running"} onClick={() => void runTest()} data-testid="mic-test">
          {t("voice.input.mic.test")}
        </Button>
        <LevelBar rms={level} testId="mic-level" />
        {test.phase === "running" ? (
          <p role="status">{t("voice.input.mic.testing")}</p>
        ) : test.phase === "done" ? (
          <p role="status">{t(`voice.input.mic.verdict.${test.verdict}`)}</p>
        ) : test.phase === "error" ? (
          <p role="alert" data-testid="mic-error">
            {test.reason === "voice.micDenied" ? `${t("voice.input.deniedTitle")} ${steps}` : t(test.reason)}
          </p>
        ) : null}
      </SettingRow>
      <SettingRow title={t("voice.input.silence.title")} description={t("voice.input.silence.description")}>
        <select
          className={`${formStyles.select} ${styles.shortInput}`}
          aria-label={t("voice.input.silence.title")}
          value={settings.silenceStopSec}
          onChange={(e) => update({ silenceStopSec: Number(e.target.value) })}
        >
          {SILENCE_STOP_CHOICES.map((s) => (
            <option key={s} value={s}>
              {s === 0 ? t("voice.input.silence.off") : t("voice.input.silence.seconds", { count: s })}
            </option>
          ))}
        </select>
      </SettingRow>
    </>
  );
}
