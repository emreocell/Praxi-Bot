import { useEffect, useState } from "react";
import { Minus, Square, Copy, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isTauriRuntime } from "../overlay/events";
import { maximizeLabelKey, trackMaximized } from "../lib/windowChrome";
import styles from "./WindowControls.module.css";

/**
 * The reserved title-bar strip: a full-width drag region (double-click maximises, via Tauri's
 * `data-tauri-drag-region`) with minimize / maximize / close at its right end.  The panels start
 * below it (`html[data-chrome="custom"]`, Sidebar.module.css), so nothing can cover the controls.
 * Rendered only when the native frame is off (Windows / Linux default); in a plain browser
 * (`?mock=1&chrome=1`) the buttons are inert, for layout checks.
 */
export function WindowControls() {
  const { t } = useTranslation();
  const [maximized, setMaximized] = useState(false);
  const live = isTauriRuntime();

  useEffect(() => (live ? trackMaximized(getCurrentWindow(), setMaximized) : undefined), [live]);

  const act = (fn: (w: ReturnType<typeof getCurrentWindow>) => Promise<void>) => () => {
    if (live) void fn(getCurrentWindow());
  };
  const maxLabel = t(maximizeLabelKey(maximized));
  return (
    <div className={styles.strip} data-tauri-drag-region data-testid="window-strip">
      <div className={styles.controls} data-testid="window-controls" data-maximized={maximized}>
        <button type="button" className={styles.button} title={t("window.minimize")} aria-label={t("window.minimize")} onClick={act((w) => w.minimize())}>
          <Minus aria-hidden="true" />
        </button>
        <button type="button" className={styles.button} title={maxLabel} aria-label={maxLabel} onClick={act((w) => w.toggleMaximize())}>
          {maximized ? <Copy aria-hidden="true" className={styles.restoreIcon} /> : <Square aria-hidden="true" />}
        </button>
        <button type="button" className={`${styles.button} ${styles.close}`} title={t("window.close")} aria-label={t("window.close")} onClick={act((w) => w.close())}>
          <X aria-hidden="true" />
        </button>
      </div>
    </div>
  );
}
