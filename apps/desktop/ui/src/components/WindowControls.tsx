import { useEffect, useState } from "react";
import { Minus, Square, Copy, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { maximizeLabelKey, trackMaximized } from "../lib/windowChrome";
import styles from "./WindowControls.module.css";

/**
 * Minimize / maximize / close, drawn by the app in the top-right corner of the
 * window (the native title bar is off). Fixed to the window, so it sits above
 * the right panel when that is open. Rendered only inside Tauri, and only when
 * the native frame is off.
 */
export function WindowControls() {
  const { t } = useTranslation();
  const [maximized, setMaximized] = useState(false);

  useEffect(() => trackMaximized(getCurrentWindow(), setMaximized), []);

  const win = () => getCurrentWindow();
  const maxLabel = t(maximizeLabelKey(maximized));
  return (
    <div className={styles.controls} data-testid="window-controls" data-maximized={maximized}>
      <button type="button" className={styles.button} title={t("window.minimize")} aria-label={t("window.minimize")} onClick={() => void win().minimize()}>
        <Minus aria-hidden="true" />
      </button>
      <button type="button" className={styles.button} title={maxLabel} aria-label={maxLabel} onClick={() => void win().toggleMaximize()}>
        {maximized ? <Copy aria-hidden="true" className={styles.restoreIcon} /> : <Square aria-hidden="true" />}
      </button>
      <button type="button" className={`${styles.button} ${styles.close}`} title={t("window.close")} aria-label={t("window.close")} onClick={() => void win().close()}>
        <X aria-hidden="true" />
      </button>
    </div>
  );
}
