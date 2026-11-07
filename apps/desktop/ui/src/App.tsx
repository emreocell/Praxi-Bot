import type { CSSProperties } from "react";
import { useEffect, useState } from "react";
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
import { PRODUCT_CONFIG } from "./config/product";
import { AccountMenu, type AccountAction } from "./components/AccountMenu";
import { HealthIndicator } from "./components/HealthIndicator";
import { HelpModal } from "./components/HelpModal";
import { botView } from "./lib/botProfile";
import { getAutostart, setAutostart } from "./lib/providers";
import { ToastProvider } from "./components/Toast";
import { ControlBridgeHost } from "./state/ControlBridgeHost";
import { PaletteHost, type PaletteScreen } from "./state/PaletteHost";
import { ShellBridgeHost } from "./state/ShellBridgeHost";
import { DaemonProvider, useDaemon } from "./state/DaemonContext";
import { BotFormView } from "./views/BotFormView";
import { BotHomeView } from "./views/BotHomeView";
import { ChatView } from "./views/ChatView";
import { NeedsYouView } from "./views/NeedsYouView";
import { OnboardingView } from "./views/OnboardingView";
import { SchedulesView } from "./views/SchedulesView";
import { AppsModal } from "./views/AppsModal";
import { SettingsModal } from "./views/SettingsModal";
import type { SettingsSectionId } from "./views/settings/types";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { i18n, type SupportedLanguage } from "./i18n";
import {
  broadcastSettingsUpdated,
  type ControlSnapshot,
  formatKeyboardEventAsHotkey,
  getControlState,
  isTauriRuntime,
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
  | { kind: "schedules" };

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
  const [settingsOpen, setSettingsOpen] = useState<SettingsSectionId | null>(
    null
  );
  const [appsOpen, setAppsOpen] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);

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

  // Native title bar on / off (Tauri window decorations; no-op in a browser).
  useEffect(() => {
    if (!isTauriRuntime()) {
      return;
    }
    void getCurrentWindow()
      .setDecorations(settings.windowFrame)
      .catch(() => {});
  }, [settings.windowFrame]);

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

  const handleWindowFrameChange = (windowFrame: boolean) => {
    updateSettings({ windowFrame });
  };

  const handleStopHotkeyChange = (stopHotkey: string) => {
    setRecordingHotkey(false);
    updateSettings({ stopHotkey });
    void setStopHotkey(stopHotkey)
      .then(setControlState)
      .catch(() => {});
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

  const handleAccountAction = (action: AccountAction) => {
    if (action === "settings") {
      setSettingsOpen("general");
    } else if (action === "computer") {
      setSettingsOpen("computer");
    } else if (action === "apps") {
      setAppsOpen(true);
    } else {
      setHelpOpen(true);
    }
  };

  const handlePaletteScreen = (screen: PaletteScreen) => {
    if (screen === "settings" || screen === "usage") {
      setSettingsOpen(screen === "usage" ? "usage" : "general");
    } else {
      setRoute({ kind: screen });
    }
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
                variant="secondary"
                fullWidth
                data-testid="open-apps"
                onClick={() => setAppsOpen(true)}
              >
                {t("sidebar.apps")}
              </Button>
              <AccountMenu
                name={settings.accountName}
                computerTarget={settings.computerTarget}
                onAction={handleAccountAction}
              />
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
            sendWith={settings.sendWith}
          />
        ) : null}
        {route.kind === "needs" ? <NeedsYouView onOpenChat={openChat} /> : null}
        {route.kind === "schedules" ? (
          <SchedulesView onOpenChat={openChat} />
        ) : null}
      </div>
      <PaletteHost
        currentBotId={routeBotId}
        onScreen={handlePaletteScreen}
        onBot={(botId) => setRoute({ kind: "bot", botId })}
        onChat={(chatId, botId) => setRoute({ kind: "chat", chatId, botId })}
      />
      <SettingsModal
        open={settingsOpen !== null}
        initialSection={settingsOpen ?? "general"}
        onClose={() => setSettingsOpen(null)}
        bridge={{
          settings,
          update: updateSettings,
          onTheme: handleThemeChange,
          onAccent: handleAccentChange,
          onLanguage: handleLanguageChange,
          onWindowFrame: handleWindowFrameChange,
          recordingHotkey,
          onToggleRecordHotkey: () => setRecordingHotkey((prev) => !prev),
          onStopHotkey: handleStopHotkeyChange,
          onToggleAutostart: () => void handleToggleAutostart(),
          autostartError,
          onOpenOnboarding: (restart) => {
            setSettingsOpen(null);
            handleOpenOnboarding(restart);
          }
        }}
      />
      <AppsModal open={appsOpen} onClose={() => setAppsOpen(false)} />
      <HelpModal
        open={helpOpen}
        onClose={() => setHelpOpen(false)}
        stopHotkey={controlState.stopHotkey || settings.stopHotkey}
        sendKeys={settings.sendWith === "enter" ? "Enter" : "Ctrl+Enter"}
        onSetup={() => {
          setHelpOpen(false);
          handleOpenOnboarding(false);
        }}
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
