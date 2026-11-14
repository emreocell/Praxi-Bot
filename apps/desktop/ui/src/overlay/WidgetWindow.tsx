import { getCurrentWindow } from "@tauri-apps/api/window";
import type { MouseEvent as ReactMouseEvent } from "react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  BotAvatar,
  Button,
  Card
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
  getControlState,
  giveBackControl,
  INITIAL_CONTROL_SNAPSHOT,
  isTauriRuntime,
  matchesHotkeyEvent,
  pauseBotRun,
  resumeBotRun,
  stopBotRun,
  subscribeOverlayEvents,
  takeTheWheel
} from "./events";
import styles from "./WidgetWindow.module.css";

export function WidgetWindow() {
  const { t } = useTranslation();
  const [settings, setSettings] = useState<AppSettings>(() => loadSettingsSync() ?? DEFAULT_SETTINGS);
  const [controlState, setControlState] = useState<ControlSnapshot>(
    INITIAL_CONTROL_SNAPSHOT
  );

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
    });

    return () => {
      mounted = false;
    };
  }, []);

  useApplyAppearance(settings, undefined, true);

  useEffect(() => {
    return subscribeOverlayEvents({
      onControlStateChanged: (snapshot) => {
        setControlState(snapshot);
      },
      onSettingsUpdated: (updated) => {
        setSettings(updated);
        if (updated.language && updated.language !== i18n.language) {
          void i18n.changeLanguage(updated.language);
        }
      }
    });
  }, []);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (matchesHotkeyEvent(event, controlState.stopHotkey)) {
        event.preventDefault();
        void stopBotRun().then(setControlState);
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [controlState.stopHotkey]);

  const handleDragMouseDown = (event: ReactMouseEvent<HTMLDivElement>) => {
    if (event.button !== 0 || !isTauriRuntime()) {
      return;
    }
    void getCurrentWindow()
      .startDragging()
      .catch(() => {});
  };

  const handleStop = () => {
    void stopBotRun().then(setControlState);
  };

  const handlePauseToggle = () => {
    if (controlState.mode === "paused") {
      void resumeBotRun()
        .then(setControlState)
        .catch(() => {});
    } else if (controlState.mode === "bot_driving") {
      void pauseBotRun()
        .then(setControlState)
        .catch(() => {});
    }
  };

  const handleWheelToggle = () => {
    if (controlState.mode === "human_driving") {
      void giveBackControl()
        .then(setControlState)
        .catch(() => {});
    } else {
      void takeTheWheel()
        .then(setControlState)
        .catch(() => {});
    }
  };

  const bot = controlState.bot ?? {
    botId: "atlas",
    botName: "Atlas",
    botColor: undefined,
    botInitials: "AT",
    botAvatar: undefined
  };

  const isHumanDriving = controlState.mode === "human_driving";
  const isPaused = controlState.mode === "paused";

  const statusBadgeClass = [
    styles.statusBadge,
    isHumanDriving
      ? styles.statusHuman
      : isPaused
        ? styles.statusPaused
        : styles.statusDriving
  ]
    .filter(Boolean)
    .join(" ");

  const statusDotClass = [
    styles.statusDot,
    isHumanDriving
      ? styles.statusDotHuman
      : isPaused
        ? styles.statusDotPaused
        : ""
  ]
    .filter(Boolean)
    .join(" ");

  const stepDisplay = isHumanDriving
    ? t("widget.humanDrivingStep")
    : isPaused
      ? controlState.currentStep || t("widget.pausedStep")
      : controlState.currentStep || t("widget.idleStep");

  return (
    <div className={styles.widgetViewport}>
      <Card
        padding="md"
        accentColor={isHumanDriving ? undefined : bot.botColor}
        className={[
          styles.widgetCard,
          isHumanDriving ? styles.widgetCardHuman : ""
        ]
          .filter(Boolean)
          .join(" ")}
      >
        <div
          className={styles.dragHeader}
          data-tauri-drag-region
          onMouseDown={handleDragMouseDown}
          aria-label={t("widget.dragHandleAria")}
        >
          <div className={styles.botIdentity}>
            <BotAvatar bot={{ id: bot.botId, name: bot.botName, shape: bot.botShape, avatar: bot.botAvatar, color: bot.botColor }} size="md" state={controlState.mode === "paused" ? "waiting-approval" : controlState.mode === "idle" ? "idle" : "working"} />
            <div className={styles.botTextMeta}>
              <span className={styles.botName}>{bot.botName}</span>
              <span className={styles.hotkeyHint}>
                {t("widget.hotkeyHint", { hotkey: controlState.stopHotkey })}
              </span>
            </div>
          </div>

          <span className={statusBadgeClass}>
            <span className={statusDotClass} aria-hidden="true" />
            <span>{t(`widget.status.${controlState.mode}`)}</span>
          </span>
        </div>

        <div className={styles.stepContainer} role="status" aria-live="polite">
          <p className={styles.stepText}>{stepDisplay}</p>
          {isHumanDriving && controlState.refusedActionsCount > 0 ? (
            <span className={styles.refusedInfo}>
              {t("widget.refusedBadge", {
                count: controlState.refusedActionsCount
              })}
            </span>
          ) : null}
        </div>

        <div className={styles.controlsRow}>
          <div className={styles.primaryControls}>
            <Button variant="danger" size="sm" onClick={handleStop}>
              {t("widget.stop")}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              disabled={isHumanDriving || controlState.mode === "idle"}
              onClick={handlePauseToggle}
            >
              {isPaused ? t("widget.resume") : t("widget.pause")}
            </Button>
          </div>

          <Button
            variant={isHumanDriving ? "primary" : "secondary"}
            size="sm"
            disabled={controlState.mode === "idle"}
            onClick={handleWheelToggle}
          >
            {isHumanDriving ? t("widget.giveBack") : t("widget.takeTheWheel")}
          </Button>
        </div>
      </Card>
    </div>
  );
}
