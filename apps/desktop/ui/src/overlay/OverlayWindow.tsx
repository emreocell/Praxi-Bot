import type { CSSProperties } from "react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  isReducedMotionPreferred,
  subscribeReducedMotion
} from "ui";
import { i18n } from "../i18n";
import { useApplyAppearance } from "../state/useApplyAppearance";
import {
  type AppSettings,
  DEFAULT_SETTINGS,
  loadSettings,
  loadSettingsSync
} from "../store/settingsStore";
import {
  type ControlSnapshot,
  extractTypedTextPreview,
  getControlState,
  INITIAL_CONTROL_SNAPSHOT,
  type MonitorGeometry,
  type OverlayClickPayload,
  type OverlayShowTargetPayload,
  physicalPointToLogical,
  physicalRectToLogical,
  readMonitorGeometryFromUrl,
  subscribeOverlayEvents
} from "./events";
import styles from "./OverlayWindow.module.css";

interface RippleInstance {
  id: number;
  logicalX: number;
  logicalY: number;
}

export function OverlayWindow() {
  const { t } = useTranslation();
  const [settings, setSettings] = useState<AppSettings>(() => loadSettingsSync() ?? DEFAULT_SETTINGS);
  const [monitor, setMonitor] = useState<MonitorGeometry>(() =>
    readMonitorGeometryFromUrl()
  );
  const [controlState, setControlState] = useState<ControlSnapshot>(
    INITIAL_CONTROL_SNAPSHOT
  );
  const [target, setTarget] = useState<OverlayShowTargetPayload | null>(null);
  const [ripples, setRipples] = useState<RippleInstance[]>([]);
  const [reducedMotion, setReducedMotion] = useState<boolean>(() =>
    isReducedMotionPreferred()
  );

  useEffect(() => {
    return subscribeReducedMotion(setReducedMotion);
  }, []);

  useEffect(() => {
    const handleResize = () => {
      setMonitor(readMonitorGeometryFromUrl());
    };
    window.addEventListener("resize", handleResize);
    return () => {
      window.removeEventListener("resize", handleResize);
    };
  }, []);

  useEffect(() => {
    let mounted = true;
    void loadSettings().then((loaded) => {
      if (!mounted) return;
      setSettings(loaded);
      if (loaded.language && loaded.language !== i18n.language) {
        void i18n.changeLanguage(loaded.language);
      }
    });

    void getControlState().then((snap) => {
      if (!mounted) return;
      setControlState(snap);
      setTarget(snap.activeTarget);
    });

    return () => {
      mounted = false;
    };
  }, []);

  useApplyAppearance(settings, undefined, true);

  useEffect(() => {
    let rippleCounter = 0;

    return subscribeOverlayEvents({
      onShowTarget: (payload) => {
        setTarget(payload);
      },
      onClick: (payload: OverlayClickPayload) => {
        const currentMonitor = readMonitorGeometryFromUrl();
        const logical = physicalPointToLogical(
          { x: payload.x, y: payload.y },
          currentMonitor
        );
        rippleCounter += 1;
        const rippleId = (payload.timestampMs ?? Date.now()) + rippleCounter;
        setRipples((prev) => [
          ...prev.slice(-4),
          { id: rippleId, logicalX: logical.x, logicalY: logical.y }
        ]);
        window.setTimeout(() => {
          setRipples((prev) => prev.filter((item) => item.id !== rippleId));
        }, 700);
      },
      onClear: () => {
        setTarget(null);
        setRipples([]);
      },
      onControlStateChanged: (snapshot) => {
        setControlState(snapshot);
        if (snapshot.mode !== "bot_driving") {
          setTarget(null);
        } else if (snapshot.activeTarget) {
          setTarget(snapshot.activeTarget);
        }
      },
      onSettingsUpdated: (updated) => {
        setSettings(updated);
        if (updated.language && updated.language !== i18n.language) {
          void i18n.changeLanguage(updated.language);
        }
      }
    });
  }, []);

  const isHumanDriving = controlState.mode === "human_driving";
  const logicalRect = target
    ? physicalRectToLogical(target.rect, monitor)
    : null;
  const typedPreview = extractTypedTextPreview(target);

  const resolveActionBadge = (rawAction: string): string => {
    const lower = rawAction.trim().toLowerCase();
    if (lower.startsWith("type") || lower === "set_value") {
      return t("overlay.actions.type");
    }
    if (lower.includes("click")) {
      return t("overlay.actions.click");
    }
    if (lower.includes("scroll")) {
      return t("overlay.actions.scroll");
    }
    if (lower.includes("key") || lower.includes("hotkey")) {
      return t("overlay.actions.key");
    }
    return t("overlay.actions.default");
  };

  const targetBoxStyle: CSSProperties | undefined = logicalRect
    ? ({
        "--target-x": `${Math.round(logicalRect.x)}px`,
        "--target-y": `${Math.round(logicalRect.y)}px`,
        "--target-w": `${Math.max(24, Math.round(logicalRect.width))}px`,
        "--target-h": `${Math.max(24, Math.round(logicalRect.height))}px`
      } as CSSProperties)
    : undefined;

  const placeLabelBelow = logicalRect ? logicalRect.y < 44 : false;

  return (
    <div className={styles.overlayViewport} aria-live="polite">
      {isHumanDriving ? (
        <div className={styles.humanControlBannerWrap}>
          <div className={styles.humanControlBanner} role="status">
            <div className={styles.humanControlBadgeRow}>
              <span className={styles.humanControlDot} aria-hidden="true" />
              <span className={styles.humanControlTitle}>
                {t("overlay.youAreInControl")}
              </span>
            </div>
            <span className={styles.humanControlHint}>
              {t("overlay.humanDrivingHint")}
            </span>
          </div>
        </div>
      ) : null}

      {!isHumanDriving && target && logicalRect ? (
        <div
          className={[
            styles.targetHighlight,
            reducedMotion ? styles.targetReducedMotion : styles.targetAnimated
          ]
            .filter(Boolean)
            .join(" ")}
          style={targetBoxStyle}
        >
          <div
            className={[
              styles.targetLabelPill,
              placeLabelBelow ? styles.targetLabelPillBelow : ""
            ]
              .filter(Boolean)
              .join(" ")}
          >
            <span className={styles.actionTag}>
              {resolveActionBadge(target.action)}
            </span>
            <span className={styles.targetLabelText}>{target.label}</span>
          </div>

          {typedPreview ? (
            <div className={styles.typingPreviewPill}>
              <span className={styles.typingPreviewLabel}>
                {t("overlay.typingPreview")}:
              </span>
              <span className={styles.typingPreviewText}>{typedPreview}</span>
            </div>
          ) : null}
        </div>
      ) : null}

      {ripples.map((ripple) => (
        <div
          key={ripple.id}
          className={styles.rippleAnchor}
          style={
            {
              "--ripple-x": `${Math.round(ripple.logicalX)}px`,
              "--ripple-y": `${Math.round(ripple.logicalY)}px`
            } as CSSProperties
          }
        >
          <span
            className={[
              styles.rippleRing,
              reducedMotion ? "" : styles.rippleRingAnimated
            ]
              .filter(Boolean)
              .join(" ")}
          />
          <span className={styles.rippleDot} />
        </div>
      ))}
    </div>
  );
}
