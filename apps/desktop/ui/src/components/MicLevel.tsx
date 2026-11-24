import { meterFraction } from "../lib/voiceInput";
import styles from "./MicLevel.module.css";

/** A small live level bar (decorative: the text next to it carries the meaning for screen readers). */
export function LevelBar({ rms, testId }: { rms: number; testId?: string }) {
  return (
    <span className={styles.meter} aria-hidden="true" data-testid={testId}>
      <span className={styles.fill} style={{ width: `${Math.round(meterFraction(rms) * 100)}%` }} />
    </span>
  );
}

/** The visible "recording" indicator: a pulsing dot, a level bar and a plain sentence. */
export function RecordingIndicator({ rms, text, hint }: { rms: number; text: string; hint?: string }) {
  return (
    <span className={styles.live} role="status" data-testid="recording-indicator">
      <span className={styles.dot} aria-hidden="true" />
      <span>{text}</span>
      <LevelBar rms={rms} />
      {hint ? <span className={styles.hint}>{hint}</span> : null}
    </span>
  );
}
