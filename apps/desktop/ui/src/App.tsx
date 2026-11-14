import { BellDot, CalendarDays, FlaskConical, LoaderCircle, MessageSquare, Plus, Search, ServerOff, Settings2, Monitor, Minus, Square, X } from "lucide-react";
import { isMockMode } from "./api/mockMode";

import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  BotAvatar,
  Button,
  Card,
  Sidebar,
  SidebarFooter,
  SidebarHeader,
  SidebarItem,
  SidebarLayout,
  SidebarSection,
} from "ui";
import styles from "./App.module.css";
import { BrandIcon } from "./components/BrandIcon";
import { useMcp } from "./state/useMcp";
import { AccountMenu, type AccountAction } from "./components/AccountMenu";
import { HealthIndicator } from "./components/HealthIndicator";
import { HelpModal } from "./components/HelpModal";
import { BOT_COLORS, botView } from "./lib/botProfile";
import { getAutostart, setAutostart } from "./lib/providers";
import { ToastProvider } from "./components/Toast";
import { ControlBridgeHost } from "./state/ControlBridgeHost";
import { appearanceFromQuery } from "./lib/appearance";
import { createDebounced } from "./lib/debounce";
import { CUSTOM_BACKDROP, NO_BACKDROP } from "./lib/backdropCatalog";
import { Backdrop, sceneColors } from "./components/Backdrop";
import { useApplyAppearance } from "./state/useApplyAppearance";
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
import { NewBotModal } from "./views/NewBotModal";
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
  loadSettingsSync,
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
    runs,
    bots,
    chats,
    needs,
    refreshBots,
    retry
  } = useDaemon();
  const [settings, setSettings] = useState<AppSettings>(() => loadSettingsSync() ?? DEFAULT_SETTINGS);
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
  const [newBotOpen, setNewBotOpen] = useState(false);
  const [botSearch, setBotSearch] = useState<string | null>(null);
  const { servers: appServers } = useMcp(true);
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
      const params = new URLSearchParams(window.location.search);
      const mock = isMockMode();
      const merged: AppSettings = { ...loaded, autostart: osAutostart,
        ...(mock ? { onboardingCompleted: true, language: params.get("lang") === "en" ? "en" as const : "tr" as const } : {}),
      };
      if (mock) Object.assign(merged, appearanceFromQuery(params, merged));
      if (mock) {
        setRoute({ kind: "chat", chatId: "33333333-3333-4333-8333-333333333333", botId: "11111111-1111-4111-8111-111111111111" });
        const screen = params.get("screen");
        if (screen === "settings") setSettingsOpen((params.get("section") ?? "general") as SettingsSectionId);
        if (screen === "apps") setAppsOpen(true);
        // Review states of the other routes (mock mode only).
        const atlas = "11111111-1111-4111-8111-111111111111";
        if (screen === "onboarding") merged.onboardingCompleted = false;
        if (screen === "botform") setRoute({ kind: "botForm", botId: atlas });
        if (screen === "newbot") setNewBotOpen(true);
        if (screen === "bot") setRoute({ kind: "bot", botId: atlas });
        if (screen === "schedules") setRoute({ kind: "schedules" });
        if (screen === "needs") setRoute({ kind: "needs" });
        if (screen === "home" || screen === "offline") setRoute({ kind: "home" });
      }
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

  // Continuous inputs (colour pickers, sliders) apply live but persist / broadcast after a pause.
  const latestSettings = useRef<AppSettings>(settings);
  const persistLater = useRef(
    createDebounced(() => {
      void saveSettings(latestSettings.current);
      void broadcastSettingsUpdated(latestSettings.current);
    }, 250)
  );
  useEffect(() => {
    const flush = () => persistLater.current.flush();
    window.addEventListener("pagehide", flush);
    return () => {
      window.removeEventListener("pagehide", flush);
      flush();
    };
  }, []);

  const updateSettings = (patch: Partial<AppSettings>, opts?: { deferSave?: boolean }) => {
    setSettings((prev) => {
      const next: AppSettings = { ...prev, ...patch };
      latestSettings.current = next;
      if (opts?.deferSave) {
        persistLater.current();
      } else {
        persistLater.current.cancel();
        void saveSettings(next);
        void broadcastSettingsUpdated(next);
      }
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
  const { theme: activeTheme, accent: activeAccent } = useApplyAppearance(
    settings,
    selectedBot ? botView(selectedBot).color : undefined
  );

  const handleSimulateBotRun = () => {
    const view = selectedBot ? botView(selectedBot) : null;
    void simulateBotRun({
      botId: view?.id ?? "atlas",
      botName: view?.name ?? "Atlas",
      botShape: view?.shape,
      botAvatar: view?.avatar,
      botColor: view?.color ?? BOT_COLORS[0],
      botInitials: view?.initials ?? "AT"
    }).then(setControlState);
  };

  const handleAccountAction = (action: AccountAction) => {
    if (action === "settings") {
      setSettingsOpen("general");
    } else if (action === "computer") {
      setSettingsOpen("computer");
    } else if (action === "schedules") {
      setRoute({ kind: "schedules" });
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
      backdrop={
        settings.backdrop !== NO_BACKDROP && settings.backdrop !== CUSTOM_BACKDROP ? (
          <Backdrop
            scene={settings.backdrop}
            dim={settings.backdropDim}
            colors={sceneColors(activeTheme, activeAccent)}
          />
        ) : undefined
      }
      sidebar={
        <Sidebar>
          <SidebarHeader>
            <div className={styles.botTop}>
              <button className={styles.characterTile} title={selectedBot?.name} onClick={() => selectedBot && setRoute({ kind: "bot", botId: selectedBot.id })}>
                {selectedBot ? <BotAvatar bot={botView(selectedBot)} size="hero" decorative /> : null}
                <span>{selectedBot?.name ?? "Praxi-Bot"}</span>
              </button>
              <div className={styles.topActions}>
                <button className={styles.iconButton} title={t("sidebar.searchBots")} aria-label={t("sidebar.searchBots")} onClick={() => setBotSearch(botSearch === null ? "" : null)}><Search /></button>
                <button className={styles.iconButton} title={t("sidebar.newBot")} aria-label={t("sidebar.newBot")} onClick={() => setNewBotOpen(true)}><Plus /></button>
              </div>
            </div>
            {botSearch !== null ? <input className={styles.botSearch} autoFocus value={botSearch} onChange={e => setBotSearch(e.target.value)} aria-label={t("sidebar.searchBots")} placeholder={t("sidebar.searchBots")} /> : null}
          </SidebarHeader>
          <SidebarSection>
            {needs.length > 0 ? <SidebarItem active={route.kind === "needs"} onClick={() => setRoute({ kind: "needs" })}><BellDot /><span className={styles.botName}>{t("sidebar.needsYou")}</span><span className={styles.needsBadge} data-testid="needs-you-badge" aria-label={t("sidebar.needsYouCount", { count: needs.length })}>{needs.length}</span></SidebarItem> : null}
            {bots.filter(bot => bot.name.toLowerCase().includes((botSearch ?? "").toLowerCase())).map((bot) => {
              const view = botView(bot);
              const latest = chats.filter(c => c.bot_id === bot.id).sort((a,b) => b.created_at_ms - a.created_at_ms)[0];
              const config = bot.config as { bot?: { preview?: string; state?: string; unread?: boolean } };
              const live = runs.find(r => r.bot_id === bot.id && ["running", "pending", "waiting_approval"].includes(r.state));
              const state = needs.some(n => n.botId === bot.id) || live?.state === "waiting_approval" ? "waiting-approval" : live ? "working" : isMockMode() ? config.bot?.state : "idle";
              const avatarState = state === "thinking" || state === "working" || state === "waiting-approval" || state === "error" ? state : "idle";
              return <SidebarItem key={bot.id} active={routeBotId === bot.id} onClick={() => latest ? openChat(latest.id) : setRoute({ kind: "bot", botId: bot.id })}>
                <BotAvatar bot={view} size="md" state={avatarState} decorative />
                <div className={styles.botMeta}><span className={styles.botName}>{view.name}</span><span className={styles.botRole}>{(isMockMode() ? config.bot?.preview : undefined) ?? latest?.title ?? t("bots.newChat")}</span></div>
              </SidebarItem>;
            })}
          </SidebarSection>

          <SidebarFooter>
            <div className={styles.footerActions}>
              {import.meta.env.DEV && new URLSearchParams(window.location.search).get("dev") === "1" ? <Button variant="ghost" onClick={handleSimulateBotRun}><FlaskConical />{t("dev.simulateBotRun")}</Button> : null}
              <button className={styles.footerLink} data-testid="open-schedules" aria-current={route.kind === "schedules" ? "page" : undefined} onClick={() => setRoute({ kind: "schedules" })}><CalendarDays /><span>{t("sidebar.schedules")}</span></button>
              <div className={styles.bottomRow}>
                <button className={styles.appsPill} data-testid="open-apps" onClick={() => setAppsOpen(true)}><span>{t("sidebar.apps")}</span><span className={styles.appStack}>{appServers.filter(s => s.status === "connected").slice(0, 4).map(s => <BrandIcon key={s.name} name={s.name} small />)}</span></button>
              </div>
              <AccountMenu name={settings.accountName} computerTarget={settings.computerTarget} onAction={handleAccountAction} />
            </div>
          </SidebarFooter>
        </Sidebar>
      }
    >
      <div className={styles.mainColumn}>
        <div className={styles.titleBar} data-tauri-drag-region>
          <span className={styles.runtimeLabel} data-tauri-drag-region><Monitor />{t("account.localComputer")}</span>
          <div className={styles.windowActions}>
            <button className={styles.iconButton} title={t("account.settings")} aria-label={t("account.settings")} onClick={() => setSettingsOpen("general")}><Settings2 /></button>
            {isTauriRuntime() && !settings.windowFrame ? <>
              <button className={styles.iconButton} title={t("window.minimize")} aria-label={t("window.minimize")} onClick={() => void getCurrentWindow().minimize()}><Minus /></button>
              <button className={styles.iconButton} title={t("window.maximize")} aria-label={t("window.maximize")} onClick={() => void getCurrentWindow().toggleMaximize()}><Square /></button>
              <button className={styles.iconButton} title={t("common.close")} aria-label={t("common.close")} onClick={() => void getCurrentWindow().close()}><X /></button>
            </> : null}
          </div>
        </div>
        <HealthIndicator />
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
                <div className={styles.selectedBotContent} data-testid="daemon-offline-empty-state">
                  <span className={`${styles.stateIcon} ${styles.stateIconDanger}`}><ServerOff /></span><h1 className={styles.emptyStateTitle}>
                    {t("main.daemonOfflineTitle")}
                  </h1>
                  <p className={styles.emptyStateDescription}>
                    {t("main.daemonOfflineDescription")}
                  </p>
                  {daemonError ? <pre className={styles.errorDetailMono}>{daemonError}</pre> : null}
                  <div className={styles.optionRow}>
                    <Button variant="primary" onClick={retry}>
                      {t("main.daemonRetry")}
                    </Button>
                    <Button variant="secondary" onClick={() => handleOpenOnboarding(false)}>
                      {t("main.runSetupWizard")}
                    </Button>
                  </div>
                </div>
              ) : phase === "connecting" && bots.length === 0 ? (
                <div className={styles.selectedBotContent}>
                  <span className={styles.stateIcon}><LoaderCircle className={styles.spin} /></span><h1 className={styles.emptyStateTitle}>
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
                    <MessageSquare className={styles.emptyIcon} /><h1 className={styles.emptyStateTitle}>
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
                            onClick={() => setNewBotOpen(true)}
                          >
                            <Plus />{t("sidebar.newBot")}
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
                          onClick={() => setNewBotOpen(true)}
                        >
                          <Plus />{t("sidebar.newBot")}
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
            onOpenBot={() => setRoute({ kind: "botForm", botId: route.botId })}
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
          botColor: selectedBot ? botView(selectedBot).color : undefined,
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
      <NewBotModal
        open={newBotOpen}
        onClose={() => setNewBotOpen(false)}
        onDone={(botId) => {
          setNewBotOpen(false);
          setRoute({ kind: "bot", botId });
        }}
      />
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
