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
import { HealthIndicator } from "./components/HealthIndicator";
import { botView } from "./lib/botProfile";
import { getAutostart, setAutostart } from "./lib/providers";
import { ToastProvider } from "./components/Toast";
import { ControlBridgeHost } from "./state/ControlBridgeHost";
import { PaletteHost } from "./state/PaletteHost";
import { ShellBridgeHost } from "./state/ShellBridgeHost";
import { DaemonProvider, useDaemon } from "./state/DaemonContext";
import { BotFormView } from "./views/BotFormView";
import { BotHomeView } from "./views/BotHomeView";
import { ChatView } from "./views/ChatView";
import { NeedsYouView } from "./views/NeedsYouView";
import { NotificationSettings } from "./views/NotificationSettings";
import { OnboardingView } from "./views/OnboardingView";
import { PermissionsSection, ProvidersSection } from "./views/SettingsSections";
import { SchedulesView } from "./views/SchedulesView";
import { UsageView } from "./views/UsageView";
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
  saveSettings,
  shouldOpenOnboardingOnStart
} from "./store/settingsStore";

type Route =
  | { kind: "home" }
  | { kind: "onboarding" }
  | { kind: "bot"; botId: string }
  | { kind: "botForm"; botId?: string }
  | { kind: "chat"; chatId: string; botId: string }
  | { kind: "needs" }
  | { kind: "usage" }
  | { kind: "schedules" }
  | { kind: "settings" };

function AppShell() {
  const { t } = useTranslation();
  const {
    phase,
    error: daemonError,
    bots,
    chats,
    needs,
    refreshBots,
    retry
  } = useDaemon();
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);
  const [route, setRoute] = useState<Route>({ kind: "home" });
  const [controlState, setControlState] = useState<ControlSnapshot>(
    INITIAL_CONTROL_SNAPSHOT
  );
  const [recordingHotkey, setRecordingHotkey] = useState<boolean>(false);
  const [autostartError, setAutostartError] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    let mounted = true;
    void loadSettings().then(async (loaded) => {
      if (!mounted) {
        return;
      }
      const osAutostart = await getAutostart().catch(() => loaded.autostart);
      if (!mounted) {
        return;
      }
      const merged: AppSettings = { ...loaded, autostart: osAutostart };
      setSettings(merged);
      if (shouldOpenOnboardingOnStart(merged)) {
        setRoute({ kind: "onboarding" });
      }
      if (merged.language && merged.language !== i18n.language) {
        void i18n.changeLanguage(merged.language);
      }
      if (merged.stopHotkey) {
        void setStopHotkey(merged.stopHotkey)
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

  const handleToggleAutostart = async () => {
    setAutostartError(null);
    const nextVal = !settings.autostart;
    try {
      const applied = await setAutostart(nextVal);
      updateSettings({ autostart: applied });
    } catch {
      setAutostartError(t("settings.autostart.error"));
    }
  };

  const handleOpenOnboarding = (restartFromFirstStep: boolean) => {
    if (restartFromFirstStep) {
      updateSettings({
        onboardingStep: 1,
        onboardingSkipped: false
      });
    } else {
      updateSettings({
        onboardingSkipped: false
      });
    }
    setRoute({ kind: "onboarding" });
  };

  const routeBotId =
    route.kind === "bot" || route.kind === "chat"
      ? route.botId
      : route.kind === "botForm"
        ? route.botId
        : undefined;
  const selectedBot = bots.find((bot) => bot.id === routeBotId) ?? bots[0];

  const handleSimulateBotRun = () => {
    const view = selectedBot ? botView(selectedBot) : null;
    void simulateBotRun({
      botId: view?.id ?? "atlas",
      botName: view?.name ?? "Atlas",
      botAvatar: view?.avatar,
      botColor: view?.color ?? "#3b82f6",
      botInitials: view?.initials ?? "AT"
    }).then(setControlState);
  };

  const openChat = (chatId: string) => {
    const chat = chats.find((c) => c.id === chatId);
    setRoute({ kind: "chat", chatId, botId: chat?.bot_id ?? "" });
  };
  const openNeeds = () => setRoute({ kind: "needs" });

  const isRunActive = controlState.mode !== "idle";
  const isHumanDriving = controlState.mode === "human_driving";
  const isPaused = controlState.mode === "paused";

  const runBanner = isRunActive ? (
    <div className={styles.runBannerWrap}>
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
            {isHumanDriving ? t("widget.giveBack") : t("widget.takeTheWheel")}
          </Button>
        </div>
      </div>
    </div>
  ) : null;

  return (
    <SidebarLayout
      sidebar={
        <Sidebar>
          <SidebarHeader>
            <div className={styles.brandBlock}>
              <span className={styles.brandTitle}>{PRODUCT_CONFIG.appName}</span>
              <HealthIndicator />
            </div>
          </SidebarHeader>

          <SidebarSection label={t("sidebar.botsSection")}>
            {needs.length > 0 ? (
              <SidebarItem
                active={route.kind === "needs"}
                onClick={() => setRoute({ kind: "needs" })}
              >
                <span className={styles.botName}>{t("sidebar.needsYou")}</span>
                <span
                  className={styles.needsBadge}
                  data-testid="needs-you-badge"
                  aria-label={t("sidebar.needsYouCount", {
                    count: needs.length
                  })}
                >
                  {needs.length}
                </span>
              </SidebarItem>
            ) : null}
            {bots.map((bot) => {
              const view = botView(bot);
              const isSelected =
                (route.kind === "bot" ||
                  route.kind === "chat" ||
                  route.kind === "botForm") &&
                routeBotId === bot.id;
              return (
                <SidebarItem
                  key={bot.id}
                  active={isSelected}
                  accentColor={view.color}
                  onClick={() => setRoute({ kind: "bot", botId: bot.id })}
                >
                  <Avatar
                    name={view.name}
                    initials={view.initials}
                    src={view.avatar}
                    color={view.color}
                    size="md"
                  />
                  <div className={styles.botMeta}>
                    <span className={styles.botName}>{view.name}</span>
                    <span className={styles.botRole}>
                      {view.executor || t("bots.noModel")}
                    </span>
                  </div>
                  <span
                    className={styles.botColorDot}
                    style={{ "--bot-dot-color": view.color } as CSSProperties}
                    aria-hidden="true"
                  />
                </SidebarItem>
              );
            })}
            <Button
              variant="secondary"
              size="sm"
              fullWidth
              onClick={() => setRoute({ kind: "botForm" })}
            >
              {t("sidebar.newBot")}
            </Button>
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
                variant={route.kind === "schedules" ? "primary" : "secondary"}
                fullWidth
                onClick={() => setRoute({ kind: "schedules" })}
              >
                {t("sidebar.schedules")}
              </Button>
              <Button
                variant={route.kind === "usage" ? "primary" : "secondary"}
                fullWidth
                onClick={() => setRoute({ kind: "usage" })}
              >
                {t("sidebar.usage")}
              </Button>
              <Button
                variant={route.kind === "settings" ? "primary" : "secondary"}
                fullWidth
                onClick={() => setRoute({ kind: "settings" })}
              >
                {t("sidebar.settings")}
              </Button>
            </div>
          </SidebarFooter>
        </Sidebar>
      }
    >
      <div className={styles.mainColumn}>
        <ShellBridgeHost
          settings={settings}
          onOpenChat={openChat}
          onOpenNeeds={openNeeds}
        />
        {runBanner}
        {route.kind === "onboarding" ? (
          <OnboardingView
            settings={settings}
            onUpdateSettings={updateSettings}
            onLanguageChange={handleLanguageChange}
            onThemeChange={handleThemeChange}
            onSkip={(currentStep) => {
              updateSettings({
                onboardingSkipped: true,
                onboardingStep: currentStep
              });
              setRoute({ kind: "home" });
            }}
            onComplete={({ botId, chatId }) => {
              updateSettings({
                onboardingCompleted: true,
                onboardingSkipped: false,
                onboardingStep: 5,
                ...(botId ? { onboardingBotId: botId } : {})
              });
              if (chatId && botId) {
                setRoute({ kind: "chat", chatId, botId });
              } else if (botId) {
                setRoute({ kind: "bot", botId });
              } else {
                setRoute({ kind: "home" });
              }
            }}
          />
        ) : null}
        {route.kind === "home" ? (
          <div className={styles.centerPane}>
            <Card padding="lg" className={styles.emptyStateCard}>
              {phase === "error" ? (
                <div
                  className={styles.selectedBotContent}
                  data-testid="daemon-offline-empty-state"
                >
                  <h1 className={styles.emptyStateTitle}>
                    {t("main.daemonOfflineTitle")}
                  </h1>
                  <p className={styles.emptyStateDescription}>
                    {t("main.daemonOfflineDescription")}
                  </p>
                  {daemonError ? (
                    <pre className={styles.errorDetailMono}>{daemonError}</pre>
                  ) : null}
                  <div className={styles.optionRow}>
                    <Button variant="primary" onClick={retry}>
                      {t("main.daemonRetry")}
                    </Button>
                    <Button
                      variant="secondary"
                      onClick={() => handleOpenOnboarding(false)}
                    >
                      {t("main.runSetupWizard")}
                    </Button>
                  </div>
                </div>
              ) : phase === "connecting" && bots.length === 0 ? (
                <div className={styles.selectedBotContent}>
                  <h1 className={styles.emptyStateTitle}>
                    {t("main.daemonConnectingTitle")}
                  </h1>
                  <p className={styles.emptyStateDescription}>
                    {t("main.daemonConnectingDescription")}
                  </p>
                  <Button variant="secondary" onClick={retry}>
                    {t("main.daemonRetry")}
                  </Button>
                </div>
              ) : (
                <div
                  className={styles.selectedBotContent}
                  data-testid="home-empty-state"
                >
                  <h1 className={styles.emptyStateTitle}>
                    {bots.length === 0
                      ? t("main.welcomeTitle")
                      : t("main.selectBotTitle")}
                  </h1>
                  <p className={styles.emptyStateDescription}>
                    {bots.length === 0
                      ? t("main.welcomeDescription")
                      : t("main.selectBotDescription")}
                  </p>
                  <div className={styles.optionRow}>
                    {bots.length === 0 ? (
                      <>
                        <Button
                          variant="primary"
                          onClick={() => handleOpenOnboarding(false)}
                        >
                          {t("main.runSetupWizard")}
                        </Button>
                        <Button
                          variant="secondary"
                          onClick={() => setRoute({ kind: "botForm" })}
                        >
                          {t("sidebar.newBot")}
                        </Button>
                        <Button
                          variant="ghost"
                          onClick={() => void refreshBots()}
                        >
                          {t("main.refreshBots")}
                        </Button>
                      </>
                    ) : (
                      <Button
                        variant="primary"
                        onClick={() => setRoute({ kind: "botForm" })}
                      >
                        {t("sidebar.newBot")}
                      </Button>
                    )}
                  </div>
                </div>
              )}
            </Card>
          </div>
        ) : null}
        {route.kind === "bot" ? (
          <BotHomeView
            key={route.botId}
            botId={route.botId}
            onOpenChat={openChat}
            onEdit={() => setRoute({ kind: "botForm", botId: route.botId })}
            onDeleted={() => setRoute({ kind: "home" })}
          />
        ) : null}
        {route.kind === "botForm" ? (
          <BotFormView
            key={route.botId ?? "new"}
            botId={route.botId}
            onDone={(botId) => setRoute({ kind: "bot", botId })}
            onCancel={() =>
              setRoute(
                route.botId
                  ? { kind: "bot", botId: route.botId }
                  : { kind: "home" }
              )
            }
          />
        ) : null}
        {route.kind === "chat" ? (
          <ChatView
            key={route.chatId}
            chatId={route.chatId}
            onBack={() => setRoute({ kind: "bot", botId: route.botId })}
            onSwitchChat={openChat}
            showAgentRoute={settings.showAgentRoute}
          />
        ) : null}
        {route.kind === "needs" ? <NeedsYouView onOpenChat={openChat} /> : null}
        {route.kind === "usage" ? <UsageView /> : null}
        {route.kind === "schedules" ? (
          <SchedulesView onOpenChat={openChat} />
        ) : null}
        {route.kind === "settings" ? (
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
                  {t("settings.onboarding.title")}
                </h2>
                <p className={styles.settingDescription}>
                  {t("settings.onboarding.description")}
                </p>
              </div>
              <div className={styles.optionRow}>
                <span className={styles.hotkeyChip}>
                  {settings.onboardingCompleted
                    ? t("settings.onboarding.statusCompleted")
                    : settings.onboardingSkipped
                      ? t("settings.onboarding.statusSkipped", {
                          step: settings.onboardingStep || 1
                        })
                      : t("settings.onboarding.statusPending")}
                </span>
                <Button
                  variant="primary"
                  size="sm"
                  onClick={() =>
                    handleOpenOnboarding(settings.onboardingCompleted)
                  }
                  data-testid="settings-open-onboarding"
                >
                  {settings.onboardingCompleted
                    ? t("settings.onboarding.restart")
                    : t("settings.onboarding.resume")}
                </Button>
              </div>
            </Card>

            <Card padding="md" className={styles.settingCard}>
              <div className={styles.settingInfo}>
                <h2 className={styles.settingHeading}>
                  {t("settings.autostart.title")}
                </h2>
                <p className={styles.settingDescription}>
                  {t("settings.autostart.description")}
                </p>
              </div>
              <div className={styles.optionRow}>
                <span className={styles.hotkeyChip}>
                  {settings.autostart
                    ? t("settings.autostart.enabled")
                    : t("settings.autostart.disabled")}
                </span>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => void handleToggleAutostart()}
                  data-testid="settings-toggle-autostart"
                >
                  {settings.autostart
                    ? t("settings.autostart.disable")
                    : t("settings.autostart.enable")}
                </Button>
              </div>
              {autostartError ? (
                <p className={styles.errorDetailMono}>{autostartError}</p>
              ) : null}
            </Card>

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

            <NotificationSettings
              settings={settings}
              onChange={updateSettings}
            />

            <ProvidersSection />
            <PermissionsSection />
          </div>
        </div>
        ) : null}
      </div>
      <PaletteHost
        currentBotId={routeBotId}
        onScreen={(screen) => setRoute({ kind: screen })}
        onBot={(botId) => setRoute({ kind: "bot", botId })}
        onChat={(chatId, botId) => setRoute({ kind: "chat", chatId, botId })}
      />
    </SidebarLayout>
  );
}

export function App() {
  return (
    <ToastProvider>
      <DaemonProvider>
        <ControlBridgeHost />
        <AppShell />
      </DaemonProvider>
    </ToastProvider>
  );
}
