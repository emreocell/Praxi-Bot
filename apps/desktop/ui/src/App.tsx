import type { ChangeEvent, CSSProperties } from "react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  applyTheme,
  Avatar,
  Button,
  Card,
  resolveTheme,
  Sidebar,
  SidebarFooter,
  SidebarHeader,
  SidebarItem,
  SidebarLayout,
  SidebarSection,
  subscribeSystemTheme,
  type ThemeMode
} from "ui";
import styles from "./App.module.css";
import {
  ACCENT_PRESETS,
  BLUR_PRESETS,
  DEFAULT_STOP_HOTKEY,
  DIM_PRESETS,
  HOTKEY_PRESETS,
  PRODUCT_CONFIG
} from "./config/product";
import { MOCK_BOTS, type MockBot } from "./data/mockBots";
import { i18n, type SupportedLanguage } from "./i18n";
import {
  broadcastSettingsUpdated,
  type ControlSnapshot,
  formatKeyboardEventAsHotkey,
  getControlState,
  giveBackControl,
  INITIAL_CONTROL_SNAPSHOT,
  matchesHotkeyEvent,
  pauseBotRun,
  resumeBotRun,
  setStopHotkey,
  simulateBotRun,
  stopBotRun,
  subscribeOverlayEvents,
  takeTheWheel
} from "./overlay/events";
import {
  type AppSettings,
  DEFAULT_SETTINGS,
  loadSettings,
  saveSettings
} from "./store/settingsStore";

type ActiveView = "bots" | "settings";

export function App() {
  const { t } = useTranslation();
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);
  const [selectedBotId, setSelectedBotId] = useState<MockBot["id"] | null>(
    null
  );
  const [activeView, setActiveView] = useState<ActiveView>("bots");
  const [controlState, setControlState] = useState<ControlSnapshot>(
    INITIAL_CONTROL_SNAPSHOT
  );
  const [recordingHotkey, setRecordingHotkey] = useState<boolean>(false);
  const fileInputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    let mounted = true;
    void loadSettings().then((loaded) => {
      if (!mounted) {
        return;
      }
      setSettings(loaded);
      if (loaded.language && loaded.language !== i18n.language) {
        void i18n.changeLanguage(loaded.language);
      }
      if (loaded.stopHotkey) {
        void setStopHotkey(loaded.stopHotkey)
          .then((snap) => {
            if (mounted) {
              setControlState(snap);
            }
          })
          .catch(() => {});
      }
    });

    void getControlState().then((snap) => {
      if (mounted) {
        setControlState(snap);
      }
    });

    return () => {
      mounted = false;
    };
  }, []);

  useEffect(() => {
    return subscribeOverlayEvents({
      onControlStateChanged: (snapshot) => {
        setControlState(snapshot);
      }
    });
  }, []);

  useEffect(() => {
    const activeTheme = resolveTheme(settings.themeMode);
    applyTheme(activeTheme, {
      accentColor: settings.accentColor,
      background: {
        url: settings.backgroundUrl,
        blur: settings.backgroundBlur,
        dim: settings.backgroundDim
      }
    });

    if (settings.themeMode === "system") {
      return subscribeSystemTheme((systemTheme) => {
        applyTheme(systemTheme, {
          accentColor: settings.accentColor,
          background: {
            url: settings.backgroundUrl,
            blur: settings.backgroundBlur,
            dim: settings.backgroundDim
          }
        });
      });
    }
    return undefined;
  }, [
    settings.themeMode,
    settings.accentColor,
    settings.backgroundUrl,
    settings.backgroundBlur,
    settings.backgroundDim
  ]);

  const updateSettings = (patch: Partial<AppSettings>) => {
    setSettings((prev) => {
      const next: AppSettings = { ...prev, ...patch };
      void saveSettings(next);
      void broadcastSettingsUpdated(next);
      return next;
    });
  };

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (recordingHotkey) {
        event.preventDefault();
        event.stopPropagation();
        if (event.key === "Escape" && !event.ctrlKey && !event.altKey && !event.shiftKey && !event.metaKey) {
          setRecordingHotkey(false);
          return;
        }
        const recorded = formatKeyboardEventAsHotkey(event);
        if (recorded) {
          setRecordingHotkey(false);
          updateSettings({ stopHotkey: recorded });
          void setStopHotkey(recorded)
            .then(setControlState)
            .catch(() => {});
        }
        return;
      }

      const activeHotkey = controlState.stopHotkey || settings.stopHotkey;
      if (matchesHotkeyEvent(event, activeHotkey)) {
        event.preventDefault();
        void stopBotRun().then(setControlState);
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [recordingHotkey, controlState.stopHotkey, settings.stopHotkey]);

  const handleThemeChange = (themeMode: ThemeMode) => {
    updateSettings({ themeMode });
  };

  const handleAccentChange = (accentColor: string) => {
    updateSettings({ accentColor });
  };

  const handleLanguageChange = (language: SupportedLanguage) => {
    void i18n.changeLanguage(language);
    updateSettings({ language });
  };

  const handleStopHotkeyChange = (stopHotkey: string) => {
    setRecordingHotkey(false);
    updateSettings({ stopHotkey });
    void setStopHotkey(stopHotkey)
      .then(setControlState)
      .catch(() => {});
  };

  const handleBackgroundFileChange = (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    if (!file) {
      return;
    }
    const reader = new FileReader();
    reader.onload = () => {
      const result = typeof reader.result === "string" ? reader.result : "";
      if (result) {
        updateSettings({
          backgroundUrl: result,
          backgroundFileName: file.name
        });
      }
    };
    reader.readAsDataURL(file);
    event.target.value = "";
  };

  const handleClearBackground = () => {
    updateSettings({
      backgroundUrl: "",
      backgroundFileName: ""
    });
  };

  const selectedBot =
    MOCK_BOTS.find((bot) => bot.id === selectedBotId) ?? null;

  const handleSimulateBotRun = () => {
    const targetBot = selectedBot ?? MOCK_BOTS[0];
    void simulateBotRun({
      botId: targetBot.id,
      botName: targetBot.name,
      botAvatar: targetBot.avatarSrc,
      botColor: targetBot.color,
      botInitials: targetBot.initials
    }).then(setControlState);
  };

  const isRunActive = controlState.mode !== "idle";
  const isHumanDriving = controlState.mode === "human_driving";
  const isPaused = controlState.mode === "paused";

  return (
    <SidebarLayout
      sidebar={
        <Sidebar>
          <SidebarHeader>
            <span className={styles.brandTitle}>{PRODUCT_CONFIG.appName}</span>
          </SidebarHeader>

          <SidebarSection label={t("sidebar.botsSection")}>
            {MOCK_BOTS.map((bot) => {
              const isSelected =
                activeView === "bots" && selectedBotId === bot.id;
              return (
                <SidebarItem
                  key={bot.id}
                  active={isSelected}
                  accentColor={bot.color}
                  onClick={() => {
                    setSelectedBotId((prev) =>
                      prev === bot.id ? null : bot.id
                    );
                    setActiveView("bots");
                  }}
                >
                  <Avatar
                    name={bot.name}
                    initials={bot.initials}
                    src={bot.avatarSrc}
                    color={bot.color}
                    isAnimated={bot.isAnimated}
                    size="md"
                  />
                  <div className={styles.botMeta}>
                    <span className={styles.botName}>{bot.name}</span>
                    <span className={styles.botRole}>{t(bot.roleKey)}</span>
                  </div>
                  <span
                    className={styles.botColorDot}
                    style={
                      { "--bot-dot-color": bot.color } as CSSProperties
                    }
                    aria-hidden="true"
                  />
                </SidebarItem>
              );
            })}
          </SidebarSection>

          <SidebarFooter>
            <div className={styles.footerActions}>
              {import.meta.env.DEV ? (
                <Button
                  variant="secondary"
                  fullWidth
                  onClick={handleSimulateBotRun}
                >
                  {t("dev.simulateBotRun")}
                </Button>
              ) : null}
              <Button
                variant={activeView === "settings" ? "primary" : "secondary"}
                fullWidth
                onClick={() =>
                  setActiveView((prev) =>
                    prev === "settings" ? "bots" : "settings"
                  )
                }
              >
                {t("sidebar.settings")}
              </Button>
            </div>
          </SidebarFooter>
        </Sidebar>
      }
    >
      {activeView === "bots" ? (
        <div className={styles.centerPane}>
          <Card
            padding="lg"
            accentColor={selectedBot?.color}
            className={styles.emptyStateCard}
          >
            {selectedBot ? (
              <div className={styles.selectedBotContent}>
                <Avatar
                  name={selectedBot.name}
                  initials={selectedBot.initials}
                  src={selectedBot.avatarSrc}
                  color={selectedBot.color}
                  isAnimated={selectedBot.isAnimated}
                  size="lg"
                />
                <span className={styles.badgeLabel}>
                  {t("main.selectedBotLabel")}
                </span>
                <h1 className={styles.emptyStateTitle}>{selectedBot.name}</h1>
                <p className={styles.emptyStateDescription}>
                  {t(selectedBot.roleKey)}
                </p>
                <div className={styles.optionRow}>
                  <Button
                    variant="secondary"
                    size="sm"
                    onClick={() => setSelectedBotId(null)}
                  >
                    {t("main.clearSelection")}
                  </Button>
                  {import.meta.env.DEV ? (
                    <Button
                      variant="primary"
                      size="sm"
                      onClick={handleSimulateBotRun}
                    >
                      {t("dev.simulateBotRun")}
                    </Button>
                  ) : null}
                </div>
              </div>
            ) : (
              <div className={styles.selectedBotContent}>
                <h1 className={styles.emptyStateTitle}>
                  {t("main.selectBotTitle")}
                </h1>
                <p className={styles.emptyStateDescription}>
                  {t("main.selectBotDescription")}
                </p>
                {import.meta.env.DEV ? (
                  <Button
                    variant="primary"
                    size="sm"
                    onClick={handleSimulateBotRun}
                  >
                    {t("dev.simulateBotRun")}
                  </Button>
                ) : null}
              </div>
            )}

            {isRunActive ? (
              <div className={styles.activeRunBanner} role="status">
                <div className={styles.activeRunHeader}>
                  <span className={styles.botName}>
                    {controlState.bot?.botName ?? t("dev.runningLabel")}
                  </span>
                  <span
                    className={[
                      styles.activeRunBadge,
                      isHumanDriving ? styles.activeRunBadgeHuman : ""
                    ]
                      .filter(Boolean)
                      .join(" ")}
                  >
                    {t(`widget.status.${controlState.mode}`)}
                  </span>
                </div>
                <p className={styles.activeRunStep}>
                  {isHumanDriving
                    ? t("widget.humanDrivingStep")
                    : controlState.currentStep || t("widget.idleStep")}
                </p>
                <div className={styles.activeRunControls}>
                  <Button
                    variant="danger"
                    size="sm"
                    onClick={() => void stopBotRun().then(setControlState)}
                  >
                    {t("widget.stop")}
                  </Button>
                  <Button
                    variant="secondary"
                    size="sm"
                    disabled={isHumanDriving}
                    onClick={() => {
                      if (isPaused) {
                        void resumeBotRun().then(setControlState);
                      } else {
                        void pauseBotRun().then(setControlState);
                      }
                    }}
                  >
                    {isPaused ? t("widget.resume") : t("widget.pause")}
                  </Button>
                  <Button
                    variant={isHumanDriving ? "primary" : "secondary"}
                    size="sm"
                    onClick={() => {
                      if (isHumanDriving) {
                        void giveBackControl().then(setControlState);
                      } else {
                        void takeTheWheel().then(setControlState);
                      }
                    }}
                  >
                    {isHumanDriving
                      ? t("widget.giveBack")
                      : t("widget.takeTheWheel")}
                  </Button>
                </div>
              </div>
            ) : null}
          </Card>
        </div>
      ) : (
        <div className={styles.settingsPane}>
          <div className={styles.settingsContainer}>
            <header className={styles.settingsHeader}>
              <h1 className={styles.settingsTitle}>{t("settings.title")}</h1>
              <p className={styles.settingsSubtitle}>
                {t("settings.subtitle")}
              </p>
            </header>

            <Card padding="md" className={styles.settingCard}>
              <div className={styles.settingInfo}>
                <h2 className={styles.settingHeading}>
                  {t("settings.theme.title")}
                </h2>
                <p className={styles.settingDescription}>
                  {t("settings.theme.description")}
                </p>
              </div>
              <div className={styles.optionRow}>
                {(["light", "dark", "system"] as const).map((mode) => (
                  <Button
                    key={mode}
                    variant="secondary"
                    active={settings.themeMode === mode}
                    onClick={() => handleThemeChange(mode)}
                  >
                    {t(`settings.theme.${mode}`)}
                  </Button>
                ))}
              </div>
            </Card>

            <Card padding="md" className={styles.settingCard}>
              <div className={styles.settingInfo}>
                <h2 className={styles.settingHeading}>
                  {t("settings.accent.title")}
                </h2>
                <p className={styles.settingDescription}>
                  {t("settings.accent.description")}
                </p>
              </div>
              <div className={styles.accentControls}>
                <div className={styles.swatchRow}>
                  {ACCENT_PRESETS.map((preset) => {
                    const isSelected =
                      settings.accentColor.toLowerCase() ===
                      preset.value.toLowerCase();
                    return (
                      <button
                        key={preset.id}
                        type="button"
                        className={[
                          styles.colorSwatch,
                          isSelected ? styles.colorSwatchActive : ""
                        ]
                          .filter(Boolean)
                          .join(" ")}
                        style={
                          {
                            "--swatch-color": preset.value
                          } as CSSProperties
                        }
                        title={t(`settings.accent.presets.${preset.id}`)}
                        aria-label={t(`settings.accent.presets.${preset.id}`)}
                        onClick={() => handleAccentChange(preset.value)}
                      />
                    );
                  })}
                  <label className={styles.customColorLabel}>
                    <span>{t("settings.accent.custom")}</span>
                    <input
                      type="color"
                      className={styles.colorInput}
                      value={settings.accentColor || "#2563eb"}
                      onChange={(e) => handleAccentChange(e.target.value)}
                    />
                  </label>
                </div>
                {settings.accentColor ? (
                  <div>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => handleAccentChange("")}
                    >
                      {t("settings.accent.reset")}
                    </Button>
                  </div>
                ) : null}
              </div>
            </Card>

            <Card padding="md" className={styles.settingCard}>
              <div className={styles.settingInfo}>
                <h2 className={styles.settingHeading}>
                  {t("settings.background.title")}
                </h2>
                <p className={styles.settingDescription}>
                  {t("settings.background.description")}
                </p>
              </div>

              <input
                ref={fileInputRef}
                type="file"
                accept="image/png,image/jpeg,image/webp,image/gif,image/svg+xml"
                className={styles.hiddenFileInput}
                onChange={handleBackgroundFileChange}
              />

              <div className={styles.optionRow}>
                <Button
                  variant="primary"
                  onClick={() => fileInputRef.current?.click()}
                >
                  {t("settings.background.chooseFile")}
                </Button>
                {settings.backgroundUrl ? (
                  <Button variant="secondary" onClick={handleClearBackground}>
                    {t("settings.background.clearFile")}
                  </Button>
                ) : null}
                <span className={styles.fileNameLabel}>
                  {settings.backgroundFileName ||
                    t("settings.background.noFile")}
                </span>
              </div>

              <div className={styles.subSettingBlock}>
                <span className={styles.subSettingLabel}>
                  {t("settings.background.blur")}
                </span>
                <div className={styles.optionRow}>
                  {BLUR_PRESETS.map((preset) => (
                    <Button
                      key={preset.id}
                      size="sm"
                      variant="secondary"
                      active={settings.backgroundBlur === preset.value}
                      onClick={() =>
                        updateSettings({ backgroundBlur: preset.value })
                      }
                    >
                      {t(`settings.background.blurLevels.${preset.id}`)}
                    </Button>
                  ))}
                </div>
              </div>

              <div className={styles.subSettingBlock}>
                <span className={styles.subSettingLabel}>
                  {t("settings.background.dim")}
                </span>
                <div className={styles.optionRow}>
                  {DIM_PRESETS.map((preset) => (
                    <Button
                      key={preset.id}
                      size="sm"
                      variant="secondary"
                      active={settings.backgroundDim === preset.value}
                      onClick={() =>
                        updateSettings({ backgroundDim: preset.value })
                      }
                    >
                      {t(`settings.background.dimLevels.${preset.id}`)}
                    </Button>
                  ))}
                </div>
              </div>
            </Card>

            <Card padding="md" className={styles.settingCard}>
              <div className={styles.settingInfo}>
                <h2 className={styles.settingHeading}>
                  {t("settings.hotkey.title")}
                </h2>
                <p className={styles.settingDescription}>
                  {t("settings.hotkey.description")}
                </p>
              </div>
              <div className={styles.optionRow}>
                <span className={styles.subSettingLabel}>
                  {t("settings.hotkey.currentLabel")}:
                </span>
                <span className={styles.hotkeyChip}>
                  {settings.stopHotkey || DEFAULT_STOP_HOTKEY}
                </span>
                <Button
                  size="sm"
                  variant={recordingHotkey ? "primary" : "secondary"}
                  onClick={() => setRecordingHotkey((prev) => !prev)}
                >
                  {recordingHotkey
                    ? t("settings.hotkey.recording")
                    : t("settings.hotkey.record")}
                </Button>
                {settings.stopHotkey !== DEFAULT_STOP_HOTKEY ? (
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => handleStopHotkeyChange(DEFAULT_STOP_HOTKEY)}
                  >
                    {t("settings.hotkey.reset")}
                  </Button>
                ) : null}
              </div>
              <div className={styles.optionRow}>
                {HOTKEY_PRESETS.map((preset) => (
                  <Button
                    key={preset.id}
                    size="sm"
                    variant="secondary"
                    active={
                      settings.stopHotkey.toLowerCase() ===
                      preset.value.toLowerCase()
                    }
                    onClick={() => handleStopHotkeyChange(preset.value)}
                  >
                    {t(`settings.hotkey.presets.${preset.id}`)}
                  </Button>
                ))}
              </div>
            </Card>

            <Card padding="md" className={styles.settingCard}>
              <div className={styles.settingInfo}>
                <h2 className={styles.settingHeading}>
                  {t("settings.language.title")}
                </h2>
                <p className={styles.settingDescription}>
                  {t("settings.language.description")}
                </p>
              </div>
              <div className={styles.optionRow}>
                {(["en", "tr"] as const).map((lang) => (
                  <Button
                    key={lang}
                    variant="secondary"
                    active={settings.language === lang}
                    onClick={() => handleLanguageChange(lang)}
                  >
                    {t(`settings.language.${lang}`)}
                  </Button>
                ))}
              </div>
            </Card>
          </div>
        </div>
      )}
    </SidebarLayout>
  );
}
