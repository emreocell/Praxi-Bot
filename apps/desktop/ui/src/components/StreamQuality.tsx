import { useTranslation } from "react-i18next";
import { QUALITY_MODES, type QualityMode } from "../lib/liveViewAdaptive";
import styles from "./StreamQuality.module.css";

/** Auto / High / Data saver: how sharp the screen picture is versus how much data it uses. */
export function StreamQuality({
  mode,
  onChange
}: {
  mode: QualityMode;
  onChange: (mode: QualityMode) => void;
}) {
  const { t } = useTranslation();
  return (
    <div className={styles.group} role="group" aria-label={t("liveView.quality.label")} data-testid="stream-quality">
      {QUALITY_MODES.map((m) => (
        <button
          key={m}
          type="button"
          aria-pressed={mode === m}
          title={t(`liveView.quality.${m}Hint`)}
          data-testid={`stream-quality-${m}`}
          onClick={() => onChange(m)}
        >
          {t(`liveView.quality.${m}`)}
        </button>
      ))}
    </div>
  );
}
