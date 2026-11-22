import { BellDot, BellRing, ChevronDown, FlaskConical, GripVertical, Star, LoaderCircle, MessageSquare, Monitor, Plus, Search, Server, ServerOff, Settings2 } from "lucide-react";
import { unreadBotIds } from "./lib/unread";
import { useReadState } from "./store/readStateStore";
import { ApiError } from "./api/client";
import type { UpdateChatInput } from "./api/types";
import { isMockMode } from "./api/mockMode";

import { Fragment, useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react";
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
  SidePanel,
  isLightColor,
  resolveTheme,
} from "ui";
import styles from "./App.module.css";
import { AppsPill } from "./components/AppsPill";
import { BotPanel } from "./components/BotPanel";
import { WindowControls } from "./components/WindowControls";
import { ResizeGrip } from "./components/ResizeGrip";
import { PRODUCT_CONFIG } from "./config/product";
import {
  isMiniRail,
  loadShellLayout,
  fitRail,
  railAfterStep,
  searchPlan,
  FLOAT_PAD,
  isFloatShell,
  panelMax,
  panelWidthFromPointer,
  railTier,
  railWidthFromPointer,
  RAIL_MAX,
  RAIL_MIN,
  RAIL_MINI,
  PANEL_MIN,
  saveShellLayout,
  stepPanel,
  type ShellLayout
} from "./lib/shellLayout";
import { useMcp } from "./state/useMcp";
import { AccountMenu, type AccountAction } from "./components/AccountMenu";
import { Popover } from "./components/Popover";
import { HealthIndicator } from "./components/HealthIndicator";
import { HelpModal } from "./components/HelpModal";
import { WhatsNewModal } from "./components/WhatsNewModal";
import { notesFor, whatsNewAction, type WhatsNewFile } from "./lib/whatsNew";
import whatsNewFile from "./whats-new.json";
import { getVersion } from "@tauri-apps/api/app";
import { isHelpKey } from "./lib/shortcuts";
import { chatTitleLabel } from "./lib/chatTitle";
import { APP_COMMAND_EVENT, type AppCommandTarget } from "./lib/slash";
import { editorSection } from "./lib/botEditor";
import { BOT_COLORS, botView, draftFromBot } from "./lib/botProfile";
import { isBotMuted, setBotMuted } from "./lib/botMute";
import { makeBotMain } from "./lib/botOrder";
import {
  backFromChatOpensPanel,
  botChats,
  botDialogFromScreen,
  botLanding
} from "./lib/botLanding";
import { nextRouteAfterChatDelete } from "./lib/chatManage";
import { nativeEngineName } from "./lib/botEngine";
import { getAutostart, setAutostart } from "./lib/providers";
import { isProcessingRunState } from "./lib/runState";
import { PausedControlBanner } from "./components/PausedControlBanner";
import { RunBar } from "./components/RunBar";
import { runBarModel } from "./lib/runBar";
import { ToastProvider } from "./components/Toast";
import { ControlBridgeHost } from "./state/ControlBridgeHost";
import { requestStop } from "./state/appStop";
import { isNewerNativeSnapshot } from "./state/stopStatus";
import { StopStatusNotice } from "./components/StopStatusNotice";
import { LiveViewProvider, useLiveView } from "./state/LiveViewContext";
import { HumanDrivingBanner } from "./components/HumanDrivingBanner";
import { LiveViewer } from "./views/LiveViewer";
import type { LiveChatSlots } from "./views/LiveViewerChat";
import { appearanceFromQuery } from "./lib/appearance";
import { createDebounced } from "./lib/debounce";
import { backdropIsActive, CUSTOM_BACKDROP, NO_BACKDROP } from "./lib/backdropCatalog";
import { Backdrop, sceneColors } from "./components/Backdrop";
import { Wallpaper } from "./components/Wallpaper";
import { resolveWallpaper, wallpaperSceneId, WALLPAPER_DIM } from "./lib/wallpaper";
import { useApplyAppearance } from "./state/useApplyAppearance";
import { usePanelReserve } from "./state/usePanelReserve";
import { useViewportWidth } from "./state/useViewportWidth";
import { PaletteHost, type PaletteScreen } from "./state/PaletteHost";
import { ShellBridgeHost } from "./state/ShellBridgeHost";
import { AddSkillModal } from "./components/AddSkillModal";
import { useToast } from "./components/Toast";
import { createChatFor, newChatFailureNeedsToast } from "./lib/newChat";
import { LeaveGuardProvider, useLeaveRegistry } from "./state/LeaveGuard";
import { SkillsModal } from "./components/SkillsModal";
import { useBotSkills } from "./state/useBotSkills";
import { BotEditModal } from "./views/BotEditModal";
import { BotEmptyView } from "./views/BotEmptyView";
import { ErrorNotice } from "./components/ErrorNotice";
import { StartupProblem } from "./components/StartupProblem";
import { humanService } from "./lib/humanError";
import { FinishSetupHint } from "./components/FinishSetupHint";
import { BotProfileModal } from "./views/BotProfileModal";
import { useBotReorder } from "./components/useBotReorder";
import { setBotOrderSnapshot } from "./store/botOrderStore";
import { computerScopePlan } from "./lib/hostState";
import { ComputerPicker } from "./components/ComputerPicker";
import { ServerTerminalProvider } from "./state/ServerTerminalContext";
import { ComputerRequestModal } from "./components/ComputerAccess";
import { HostEmptyState } from "./components/HostEmptyState";
import { HostStateBanner } from "./components/HostStateBanner";
import { SafeModeBanner } from "./components/SafeModeBanner";
import { onboardingAllowed } from "./lib/hostMode";
import {
  DaemonProvider,
  HostSession,
  LOCAL_COMPUTER,
  useDaemon
} from "./state/DaemonContext";
import { ChatView } from "./views/ChatView";
import { NeedsYouView } from "./views/NeedsYouView";
import { NotificationHistoryView } from "./views/NotificationHistoryView";
import { unreadCount } from "./lib/notificationHistory";
import { useNotificationHistory } from "./store/notificationHistoryStore";
import { OnboardingView } from "./views/OnboardingView";
import { SchedulesView } from "./views/SchedulesView";
import { AppsModal } from "./views/AppsModal";
import { NewBotModal } from "./views/NewBotModal";
import { SettingsModal } from "./views/SettingsModal";
import { OPEN_AI_SETTINGS_EVENT } from "./state/useCliEngineRows";
import { EngineConsentProvider } from "./state/EngineConsent";
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
import { humanLine } from "./lib/humanError";

type Route =
  | { kind: "home" }
  | { kind: "onboarding" }
  /** A bot without any chat yet: the empty state (there is no bot page). */
  | { kind: "bot"; botId: string }
  | { kind: "chat"; chatId: string; botId: string; messageId?: string; jumpRequestId?: number }
  | { kind: "needs" }
  | { kind: "notifications" }
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
    client,
    refreshBots,
    refreshChats,
    pendingChatIds,
    updateChat,
    deleteChat,
    retry,
    restartDaemon,
    home,
    workLocalNow,
    homeStale,
    serviceProblem,
    startup,
    retryInSeconds,
    activeComputerId
  } = useDaemon();
  const { openViewer } = useLiveView();
  const readState = useReadState(client?.baseUrl ?? "");
  const unreadBots = useMemo(() => unreadBotIds(chats, runs, needs, readState), [chats, runs, needs, readState]);
  const [settings, setSettings] = useState<AppSettings>(() => loadSettingsSync() ?? DEFAULT_SETTINGS);
  const [route, setRoute] = useState<Route>({ kind: "home" });
  const [homeChatSlots, setHomeChatSlots] = useState<LiveChatSlots | null>(null);
  const [localChatSlots, setLocalChatSlots] = useState<LiveChatSlots | null>(null);
  // Every route change goes through this gate: a bot dialog with unsaved data asks first, and one
  // that is saving blocks it (PR #95, finding 4).  Do not call `setRoute` directly elsewhere.
  const leave = useLeaveRegistry();
  const go = (next: Route) => {
    leave.attempt(() => setRoute(next));
  };
  const toast = useToast();
  // Chats, bots and runs belong to one computer: switching computers starts at home.
  const lastComputer = useRef(activeComputerId);
  useEffect(() => {
    if (lastComputer.current !== activeComputerId) {
      lastComputer.current = activeComputerId;
      setRoute({ kind: "home" });
    }
  }, [activeComputerId]);
  // The first-run wizard is written for this computer's own daemon: never while a host is selected.
  const onboardingBlocked = !onboardingAllowed(activeComputerId);
  // The bots' computer has no bots to show (not set up yet, or not answering): one calm screen, never a blank app.
  const hostEmpty = onboardingBlocked && phase === "ready" && bots.length === 0;
  useEffect(() => {
    if (onboardingBlocked && route.kind === "onboarding") {
      setRoute({ kind: "home" });
    }
  }, [onboardingBlocked, route.kind]);
  const [controlState, setControlStateRaw] = useState<ControlSnapshot>(
    INITIAL_CONTROL_SNAPSHOT
  );
  // Native snapshots arrive from events, reads and command replies in any order:
  // apply only a newer revision (a failed read has none and is ignored).
  const setControlState = useCallback((snap: ControlSnapshot) => {
    setControlStateRaw((prev) => (isNewerNativeSnapshot(prev.stateRev ?? -1, snap) ? snap : prev));
  }, []);
  const [recordingHotkey, setRecordingHotkey] = useState<boolean>(false);
  const [autostartError, setAutostartError] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState<SettingsSectionId | null>(
    null
  );
  const [appsOpen, setAppsOpen] = useState(false);
  // The model picker's "Install <CLI>" rows open Settings > AI, where the install flow lives.
  useEffect(() => {
    const open = () => setSettingsOpen("ai");
    window.addEventListener(OPEN_AI_SETTINGS_EVENT, open);
    return () => window.removeEventListener(OPEN_AI_SETTINGS_EVENT, open);
  }, []);
  const [newBotOpen, setNewBotOpen] = useState(false);
  // The dialogs of a bot (card 244-C): edit and profile replace the old pages; "add skill" can
  // sit on top of the skills dialog or the edit dialog.
  const [botEditOpen, setBotEditOpen] = useState(false);
  const [botProfileOpen, setBotProfileOpen] = useState(false);
  const [skillsOpen, setSkillsOpen] = useState(false);
  const [addSkillOpen, setAddSkillOpen] = useState(false);
  // Review hooks (mock mode only): `?screen=botform&section=models`, `?screen=addskill&step=install`.
  const [review] = useState(() => {
    const q = isMockMode() && typeof window !== "undefined" ? new URLSearchParams(window.location.search) : null;
    return { section: editorSection(q?.get("section")), installStep: q?.get("step") === "install" };
  });
  const [newChatBusy, setNewChatBusy] = useState(false);
  const [newChatError, setNewChatError] = useState<string | null>(null);
  const [botSearch, setBotSearch] = useState<string | null>(null);
  const { servers: appServers } = useMcp(true);
  const [helpOpen, setHelpOpen] = useState(false);
  const [settingsLoaded, setSettingsLoaded] = useState(false);
  const [appVersion, setAppVersion] = useState("");
  const [whatsNewDone, setWhatsNewDone] = useState(false);
  const [layout, setLayout] = useState<ShellLayout>(loadShellLayout);
  const [panelOpen, setPanelOpen] = useState(() => {
    if (!isMockMode() || typeof window === "undefined") {
      return false;
    }
    const q = new URLSearchParams(window.location.search);
    return q.get("lv") === "panel" || q.get("panel") === "1";
  });
  const viewportWidth = useViewportWidth();
  const chipRef = useRef<HTMLButtonElement | null>(null);
  // Remember the column widths (after a pause, so a drag writes once).
  const layoutRef = useRef(layout);
  useEffect(() => {
    layoutRef.current = layout;
    const timer = setTimeout(() => saveShellLayout(layout), 200);
    return () => clearTimeout(timer);
  }, [layout]);
  useEffect(() => {
    const flush = () => saveShellLayout(layoutRef.current);
    window.addEventListener("pagehide", flush);
    return () => {
      window.removeEventListener("pagehide", flush);
      flush();
    };
  }, []);
  const setRail = useCallback((rail: number) => setLayout((l) => (l.rail === rail ? l : { ...l, rail })), []);
  const setPanelWidth = useCallback((panel: number) => setLayout((l) => (l.panel === panel ? l : { ...l, panel })), []);
  const gridPad = () => (document.documentElement.dataset.float === "on" ? FLOAT_PAD : 0);
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
        ...(mock ? { onboardingCompleted: true, language: (["en", "de"] as const).find((l) => l === params.get("lang")) ?? ("tr" as const) } : {}),
      };
      if (mock) Object.assign(merged, appearanceFromQuery(params, merged));
      if (mock) {
        setRoute({ kind: "chat", chatId: "33333333-3333-4333-8333-333333333333", botId: "11111111-1111-4111-8111-111111111111" });
        const screen = params.get("screen");
        if (screen === "settings") setSettingsOpen((params.get("section") ?? "general") as SettingsSectionId);
        if (screen === "apps") setAppsOpen(true);
        if (params.get("panel") === "1") setPanelOpen(true);
        // Review state of the run bar: `?control=bot_driving|paused|human_driving`.
        const ctl = params.get("control");
        if (ctl === "bot_driving" || ctl === "paused" || ctl === "human_driving") {
          setControlState({
            ...INITIAL_CONTROL_SNAPSHOT,
            mode: ctl,
            stateRev: 1,
            stopSeq: 0,
            bot: { botId: "11111111-1111-4111-8111-111111111111", botName: "Atlas" },
            currentStep: params.get("lang") === "en" ? "Comparing sources in the browser" : "Kaynakları tarayıcıda karşılaştırıyor"
          });
        }
        const railParam = Number(params.get("rail"));
        if (railParam > 0) setLayout((l) => ({ ...l, rail: railParam < 150 ? RAIL_MINI : Math.min(RAIL_MAX, railParam) }));
        // Review states of the other routes (mock mode only).
        const atlas = "11111111-1111-4111-8111-111111111111";
        if (screen === "onboarding") merged.onboardingCompleted = false;
        const botDialog = botDialogFromScreen(screen);
        if (botDialog === "edit") setBotEditOpen(true);
        if (botDialog === "profile") setBotProfileOpen(true);
        if (botDialog === "skills") setSkillsOpen(true);
        if (botDialog === "addSkill") setAddSkillOpen(true);
        if (screen === "newbot") setNewBotOpen(true);
        if (screen === "bot" || screen === "gone") setRoute({ kind: "bot", botId: screen === "gone" ? "00000000-0000-4000-8000-000000000000" : atlas });
        if (screen === "schedules") setRoute({ kind: "schedules" });
        if (screen === "needs") setRoute({ kind: "needs" });
        if (screen === "home" || screen === "offline") setRoute({ kind: "home" });
      }
      setSettings(merged);
      setSettingsLoaded(true);
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
  }, [setControlState]);

  useEffect(() => {
    return subscribeOverlayEvents({
      onControlStateChanged: (snapshot) => {
        setControlState(snapshot);
      }
    });
  }, [setControlState]);

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

  // Sidebar order: settings hold the ids, the daemon context sorts `bots` with them (top = main bot).
  // Each computer keeps its OWN order (and so its own main bot): writing one never touches the other.
  useEffect(() => {
    setBotOrderSnapshot(settings.botOrder, settings.hostBotOrders);
  }, [settings.botOrder, settings.hostBotOrders]);
  const saveBotOrder = (next: string[]) => {
    updateSettings(
      activeComputerId === LOCAL_COMPUTER
        ? { botOrder: next }
        : { hostBotOrders: { ...settings.hostBotOrders, [activeComputerId]: next } }
    );
  };
  const [orderNote, setOrderNote] = useState("");
  const botIds = useMemo(() => bots.map((b) => b.id), [bots]);
  const reorder = useBotReorder(
    botIds,
    saveBotOrder,
    (id, pos) => setOrderNote(t("sidebar.reorderMoved", { name: bots.find((b) => b.id === id)?.name ?? "", pos }))
  );

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
        // One key press = one Stop decision (never on key repeat). The ledger halts
        // native input, cancels runs and pauses a held takeover.
        if (!event.repeat) {
          requestStop("hotkey");
        }
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [recordingHotkey, controlState.stopHotkey, settings.stopHotkey, setControlState]);

  // Native title bar on / off (Tauri window decorations; no-op in a browser).
  useEffect(() => {
    if (!isTauriRuntime()) {
      return;
    }
    void getCurrentWindow()
      .setDecorations(settings.windowFrame)
      .catch(() => {});
  }, [settings.windowFrame]);

  // Native frame off (the default on Windows): the app draws the window controls.
  // `?mock=1&chrome=1` previews the strip in a plain browser (inert buttons) for layout checks.
  const previewChrome = isMockMode() && new URLSearchParams(window.location.search).get("chrome") === "1";
  const customChrome = (isTauriRuntime() && !settings.windowFrame) || previewChrome;
  // Panels start below the reserved strip (see Sidebar.module.css); html[data-chrome] drives that.
  useEffect(() => {
    if (!customChrome) return undefined;
    document.documentElement.dataset.chrome = "custom";
    return () => {
      delete document.documentElement.dataset.chrome;
    };
  }, [customChrome]);

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
    if (onboardingBlocked) {
      return;
    }
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
    go({ kind: "onboarding" });
  };

  const routeBotId =
    route.kind === "bot" || route.kind === "chat" ? route.botId : undefined;
  const selectedBot = bots.find((bot) => bot.id === routeBotId) ?? bots[0];
  // The wallpaper (when on) takes the backdrop's place, so the panels' glass, tone sampling and
  // floating layout all follow the existing backdrop plumbing.
  const baseTheme = resolveTheme(settings.themeMode, settings.customTheme);
  const wallpaperKind = resolveWallpaper(settings.wallpaper, baseTheme.id, !isLightColor(baseTheme.bg));
  const shellSettings = useMemo(
    () =>
      wallpaperKind
        ? { ...settings, backdrop: wallpaperSceneId(wallpaperKind), backdropDim: WALLPAPER_DIM, veil: settings.wallpaperVeil }
        : settings,
    [settings, wallpaperKind]
  );
  const { theme: activeTheme, accent: activeAccent } = useApplyAppearance(
    shellSettings,
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
    } else if (action === "appearance" || action === "ai" || action === "usage" || action === "updates") {
      setSettingsOpen(action);
    } else if (action === "computer") {
      setSettingsOpen("computer");
    } else if (action === "schedules") {
      go({ kind: "schedules" });
    } else if (action === "apps") {
      setAppsOpen(true);
    } else {
      setHelpOpen(true);
    }
  };

  // Composer commands (/help, /settings ...) ask the shell to open the same places as the profile menu.
  const accountActionRef = useRef(handleAccountAction);
  accountActionRef.current = handleAccountAction;
  useEffect(() => {
    const onCommand = (e: Event) => {
      const target = (e as CustomEvent<AppCommandTarget>).detail;
      accountActionRef.current(target);
    };
    window.addEventListener(APP_COMMAND_EVENT, onCommand);
    return () => window.removeEventListener(APP_COMMAND_EVENT, onCommand);
  }, []);

  // Ctrl/Cmd + , opens settings (the shortcut the profile menu advertises).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key === ",") {
        e.preventDefault();
        setSettingsOpen("general");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // A bare `?` (no text field focused) opens the shortcuts dialog.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (isHelpKey(e)) {
        e.preventDefault();
        setHelpOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const handlePaletteScreen = (screen: PaletteScreen) => {
    if (screen === "settings" || screen === "usage") {
      setSettingsOpen(screen === "usage" ? "usage" : "general");
    } else {
      go({ kind: screen });
    }
  };

  const openChat = (chatId: string, messageId?: string) => {
    const chat = chats.find((c) => c.id === chatId);
    go({ kind: "chat", chatId, botId: chat?.bot_id ?? "", messageId, jumpRequestId: messageId ? Date.now() : undefined });
  };
  const openNeeds = () => go({ kind: "needs" });
  const notifyHistory = useNotificationHistory();
  const notifyUnread = unreadCount(notifyHistory);
  // Selecting a bot: its latest chat, or the empty state when it has none.
  const goToBot = (botId: string) => go(botLanding(botId, chats));
  const startChat = async (botId: string) => {
    if (!client || newChatBusy) {
      return;
    }
    setNewChatBusy(true);
    setNewChatError(null);
    const result = await createChatFor(client, botId, refreshChats);
    if (result.ok) {
      setBotProfileOpen(false);
      go({ kind: "chat", chatId: result.chatId, botId });
    } else {
      const detail = result.message;
      setNewChatError(detail);
      if (newChatFailureNeedsToast(route.kind)) {
        toast.show(t("bots.newChatFailed", { detail }), "danger");
      }
    }
    setNewChatBusy(false);
  };

  /** A suggested first prompt: open a chat and send the prompt as its first message. */
  const startChatWith = async (botId: string, prompt: string) => {
    if (!client || newChatBusy) {
      return;
    }
    setNewChatBusy(true);
    setNewChatError(null);
    const result = await createChatFor(client, botId, refreshChats);
    if (result.ok) {
      go({ kind: "chat", chatId: result.chatId, botId });
      try {
        await client.sendMessage(result.chatId, prompt);
      } catch (e) {
        toast.show(humanLine(e), "danger");
      }
    } else {
      setNewChatError(result.message);
    }
    setNewChatBusy(false);
  };

  const botInView = selectedBot !== undefined && (route.kind === "chat" || route.kind === "bot");
  // The bot dialogs belong to one bot: another bot, or leaving the bot views, closes them.
  const dialogBotId = botInView ? selectedBot?.id : undefined;
  const lastDialogBot = useRef<string | undefined>(undefined);
  useEffect(() => {
    // (A first selection - mock review links open a dialog together with the route - keeps them.)
    if (lastDialogBot.current !== undefined) {
      setBotEditOpen(false);
      setBotProfileOpen(false);
      setSkillsOpen(false);
      setAddSkillOpen(false);
    }
    lastDialogBot.current = dialogBotId;
  }, [dialogBotId]);
  const panelVisible = panelOpen && botInView;
  const float = isFloatShell(shellSettings.uiStyle, backdropIsActive(shellSettings.backdrop, shellSettings.backgroundUrl));
  const panelWidth = Math.min(layout.panel, panelMax(viewportWidth, layout.rail, float));
  const panelReserve = usePanelReserve(panelVisible);
  const railWidth = fitRail(viewportWidth, layout.rail, panelWidth, float, panelReserve);
  const fitStored = (candidate: number) => fitRail(viewportWidth, candidate, panelWidth, float, panelReserve);
  const search = searchPlan(layout.rail, railWidth, botSearch !== null);
  const searchButtonRef = useRef<HTMLButtonElement | null>(null);
  const closeSearch = useCallback(() => setBotSearch(null), []);
  // A dialog opened by a shortcut sits above the Search popover: close the popover instead of leaving it beneath.
  const dialogOpen = settingsOpen !== null || appsOpen || newBotOpen || helpOpen || botEditOpen || botProfileOpen || skillsOpen || addSkillOpen;
  useEffect(() => {
    if (dialogOpen) setBotSearch(null);
  }, [dialogOpen]);

  // "What's new": bundled notes of the installed version, shown once per version (never blocks, no network).
  useEffect(() => {
    // Review state `?mock=1&screen=whatsnew`: the card for the bundled notes (no Tauri runtime to ask).
    if (isMockMode() && new URLSearchParams(window.location.search).get("screen") === "whatsnew") setAppVersion("0.1.0");
    if (!isTauriRuntime()) return;
    void getVersion().then(setAppVersion).catch(() => undefined);
  }, []);
  const whatsNewNotes = notesFor(whatsNewFile as WhatsNewFile, appVersion, i18n.language);
  const whatsNewStep = settingsLoaded
    ? whatsNewAction({
        version: appVersion,
        lastSeen: settings.lastSeenVersion,
        onboardingCompleted: settings.onboardingCompleted,
        hasNotes: whatsNewNotes !== null
      })
    : "none";
  const recordWhatsNew = () => {
    setWhatsNewDone(true);
    updateSettings({ lastSeenVersion: appVersion });
  };
  useEffect(() => {
    if (whatsNewStep === "record") recordWhatsNew();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [whatsNewStep]);
  const showWhatsNew = whatsNewStep === "show" && !whatsNewDone && route.kind !== "onboarding" && !dialogOpen;
  const mini = isMiniRail(railWidth);

  // The computer card asks for the screen itself, only when the owner presses "show screen".
  const closePanel = () =>
    leave.attempt(() => {
      setPanelOpen(false);
      chipRef.current?.focus();
    });
  const togglePanel = () => (panelOpen ? closePanel() : setPanelOpen(true));
  const panelBotView = selectedBot ? botView(selectedBot) : undefined;
  const panelChats = selectedBot
    ? botChats(chats, selectedBot.id).map((c) => ({
        id: c.id,
        title: c.title ?? "",
        createdAtMs: c.created_at_ms,
        pinned: c.pinned,
        archived: c.archived
      }))
    : [];
  const handlePatchChat = async (chatId: string, patch: UpdateChatInput) => {
    try {
      await updateChat(chatId, patch);
    } catch (err) {
      toast.show(humanLine(err), "danger");
    }
  };

  const handleDeleteChat = async (chatId: string) => {
    const target = chats.find((c) => c.id === chatId);
    const remaining = chats.filter((c) => c.id !== chatId);
    const next = target ? nextRouteAfterChatDelete(chatId, target.bot_id, remaining, route) : null;
    try {
      await deleteChat(chatId);
      if (next) go(next);
    } catch (err) {
      toast.show(
        err instanceof ApiError && err.code === "conflict" ? t("chatManage.deleteActiveRun") : humanLine(err),
        "danger"
      );
    }
  };
  // One copy of the selected bot's skills for the panel and every bot dialog.
  const botSkills = useBotSkills(botInView && selectedBot ? selectedBot.id : null, client);
  useEffect(() => {
    setNewChatError(null);
  }, [routeBotId]);

  const isHumanDriving = controlState.mode === "human_driving";
  // The chat carries its own run bar (docked on the composer); every other screen
  // gets the same bar as a strip so Stop is never out of reach.
  const runBanner =
    route.kind === "chat" ? null : (
      <RunBar
        variant="strip"
        model={runBarModel({ chat: "none", native: controlState.mode })}
        botName={controlState.bot?.botName}
        detail={controlState.mode === "idle" ? null : controlState.currentStep}
        humanDriving={isHumanDriving}
        onStop={() => {
          requestStop("banner");
        }}
        onPause={() => void pauseBotRun().then(setControlState)}
        onResume={() => void resumeBotRun().then(setControlState)}
        onWheel={() =>
          void (isHumanDriving ? giveBackControl() : takeTheWheel()).then(
            setControlState
          )
        }
        onBigView={() => openViewer()}
      />
    );

  return (
    <SidebarLayout
      style={{ "--size-sidebar-width": `${railWidth}px` } as CSSProperties}
      backdrop={
        wallpaperKind ? (
          <Wallpaper kind={wallpaperKind} motion={settings.wallpaperMotion} />
        ) : settings.backdrop !== NO_BACKDROP && settings.backdrop !== CUSTOM_BACKDROP ? (
          <Backdrop
            scene={settings.backdrop}
            dim={settings.backdropDim}
            colors={sceneColors(activeTheme, activeAccent)}
          />
        ) : undefined
      }
      aside={
        <SidePanel
          open={panelVisible}
          width={panelWidth}
          aria-label={t("botPanel.toggle")}
          onKeyDown={(e) => {
            if (e.key === "Escape" && panelVisible) {
              e.stopPropagation();
              closePanel();
            }
          }}
        >
          <ResizeGrip
            side="start"
            label={t("shell.panelGrip")}
            value={panelWidth}
            min={PANEL_MIN}
            max={panelMax(viewportWidth, layout.rail, float)}
            onDrag={(x) => setPanelWidth(panelWidthFromPointer(x, window.innerWidth, gridPad(), layout.rail, float))}
            onStep={(dir) => setPanelWidth(stepPanel(panelWidth, dir === -1 ? 1 : -1, window.innerWidth, layout.rail, float))}
          />
          {selectedBot && panelBotView ? (
            <BotPanel
              bot={panelBotView}
              model={panelBotView.executor}
              engineCli={nativeEngineName(selectedBot.config)}
              chats={panelChats}
              activeChatId={route.kind === "chat" ? route.chatId : undefined}
              onClose={closePanel}
              preset={draftFromBot(selectedBot).preset}
              needs={needs}
              runs={runs}
              pendingChatIds={pendingChatIds}
              skills={botSkills}
              newChatBusy={newChatBusy}
              visible={panelVisible}
              onOpenChat={openChat}
              onNewChat={() => void startChat(selectedBot.id)}
              onPatchChat={handlePatchChat}
              onDeleteChat={handleDeleteChat}
              onOpenProfile={() => setBotProfileOpen(true)}
              onOpenSettings={() => setBotEditOpen(true)}
              onManageSkills={() => setSkillsOpen(true)}
              onAddSkill={() => setAddSkillOpen(true)}
              onOpenMail={() => setSettingsOpen("mail")}
              isMain={bots[0]?.id === selectedBot.id}
              mainBotName={bots[0] ? botView(bots[0]).name : undefined}
              onMakeMain={botIds.length > 1 ? () => saveBotOrder(makeBotMain(botIds, selectedBot.id)) : undefined}
              notifyMuted={isBotMuted(settings.mutedBotIds, selectedBot.id)}
              onToggleNotifyMute={() =>
                updateSettings({
                  mutedBotIds: setBotMuted(
                    settings.mutedBotIds,
                    selectedBot.id,
                    !isBotMuted(settings.mutedBotIds, selectedBot.id)
                  )
                })
              }
            />
          ) : null}
        </SidePanel>
      }
      sidebar={
        <Sidebar className={[styles.rail, mini ? styles.railMini : ""].filter(Boolean).join(" ")} data-rail-tier={railTier(railWidth)}>
          <a
            href="#main-content"
            className={styles.skipLink}
            data-testid="skip-to-main"
            onClick={(e) => {
              e.preventDefault();
              const main = document.getElementById("main-content");
              const target =
                main?.querySelector<HTMLElement>(
                  "textarea:not([disabled]), [data-autofocus], h1[tabindex], button:not([disabled]), [href], input:not([disabled])"
                ) ?? main;
              target?.focus();
            }}
          >
            {t("shell.skipToMain")}
          </a>
          <ResizeGrip
            side="end"
            label={t("shell.railGrip")}
            value={railWidth}
            min={RAIL_MINI}
            max={RAIL_MAX}
            onDrag={(x) => setRail(railWidthFromPointer(x, gridPad()))}
            onStep={(dir) => setRail(railAfterStep(layout.rail, railWidth, dir, fitStored))}
          />
          <SidebarHeader>
            <div className={styles.brand}>
              <b className={styles.brandName}>{PRODUCT_CONFIG.appName}</b>
              <div className={styles.topActions}>
                <button ref={searchButtonRef} className={styles.iconButton} title={t("sidebar.searchBots")} aria-label={t("sidebar.searchBots")} onClick={() => { if (search.expandStored) setRail(RAIL_MIN); setBotSearch(search.open ? "" : null); }}><Search /></button>
                <button className={styles.iconButton} title={t("sidebar.newBot")} aria-label={t("sidebar.newBot")} data-testid="new-bot-button" onClick={() => setNewBotOpen(true)}><Plus /></button>
              </div>
            </div>
            {botSearch !== null && !mini ? <input className={styles.botSearch} autoFocus value={botSearch} onChange={e => setBotSearch(e.target.value)} aria-label={t("sidebar.searchBots")} placeholder={t("sidebar.searchBots")} /> : null}
            {search.popover ? (
              <Popover open={botSearch !== null} anchor={searchButtonRef} onClose={closeSearch} placement="bottom-start" role="dialog" label={t("sidebar.searchBots")} width={240}>
                <input className={styles.botSearchPopover} value={botSearch ?? ""} onChange={e => setBotSearch(e.target.value)} aria-label={t("sidebar.searchBots")} placeholder={t("sidebar.searchBots")} />
              </Popover>
            ) : null}
          </SidebarHeader>
          <SidebarSection>
            <span className={styles.srOnly} role="status" aria-live="polite">{orderNote}</span>
            <div ref={reorder.containerRef} className={styles.botList} data-stale={homeStale ? "true" : undefined}>
            {needs.length > 0 ? <SidebarItem className={styles.railItem} active={route.kind === "needs"} title={t("sidebar.needsYou")} aria-label={mini ? t("sidebar.needsYouCount", { count: needs.length }) : undefined} onClick={openNeeds}><BellDot /><span className={`${styles.botName} ${styles.railText}`}>{t("sidebar.needsYou")}</span><span className={styles.needsBadge} data-testid="needs-you-badge" aria-label={t("sidebar.needsYouCount", { count: needs.length })}>{needs.length}</span></SidebarItem> : null}
            {notifyHistory.length > 0 ? <SidebarItem className={styles.railItem} active={route.kind === "notifications"} title={t("sidebar.notifications")} aria-label={mini ? t("sidebar.notificationsCount", { count: notifyUnread }) : undefined} onClick={() => go({ kind: "notifications" })}><BellRing /><span className={`${styles.botName} ${styles.railText}`}>{t("sidebar.notifications")}</span>{notifyUnread > 0 ? <span className={styles.needsBadge} data-testid="notify-history-badge" aria-label={t("sidebar.notificationsCount", { count: notifyUnread })}>{notifyUnread}</span> : null}</SidebarItem> : null}
            {bots.filter(bot => bot.name.toLowerCase().includes((botSearch ?? "").toLowerCase())).map((bot) => {
              const view = botView(bot);
              const latest = botChats(chats, bot.id).find((c) => !c.archived);
              const config = bot.config as { bot?: { preview?: string; state?: string; unread?: boolean } };
              const live = runs.find(r => r.bot_id === bot.id && isProcessingRunState(r.state));
              const state = needs.some(n => n.botId === bot.id) || live?.state === "waiting_approval" ? "waiting-approval" : live ? "working" : isMockMode() ? config.bot?.state : "idle";
              const avatarState = state === "thinking" || state === "working" || state === "waiting-approval" || state === "error" ? state : "idle";
              const index = botIds.indexOf(bot.id);
              const canReorder = botSearch === null || botSearch === "";
              const drag = reorder.drag;
              const dropEdge = drag && drag.id !== bot.id && drag.over === index ? (drag.over < drag.from ? "before" : "after") : undefined;
              return <div key={bot.id} className={styles.botRow} data-dragging={drag?.id === bot.id ? "true" : undefined} data-drop={dropEdge} {...(canReorder ? reorder.rowProps(bot.id) : { "data-bot-id": bot.id })}>
                <SidebarItem className={styles.railItem} active={routeBotId === bot.id} title={view.name} aria-label={mini ? view.name : undefined} onClick={() => goToBot(bot.id)}>
                  <BotAvatar bot={view} size="md" state={avatarState} decorative />
                  <div className={`${styles.botMeta} ${styles.railText}`}><span className={styles.botName}><span className={styles.botNameText}>{view.name}</span>{index === 0 && botIds.length > 1 ? <span className={styles.mainMark} role="img" title={t("sidebar.mainBotHint")} aria-label={t("sidebar.mainBotHint")}><Star aria-hidden="true" /></span> : null}</span><span className={styles.botRole}>{(isMockMode() ? config.bot?.preview : undefined) ?? chatTitleLabel(latest?.title, t("bots.newChat"))}</span></div>
                  {unreadBots.has(bot.id) ? <span className={styles.unreadDot} role="img" aria-label={t("sidebar.unreadBot")} data-testid="bot-unread" /> : null}
                </SidebarItem>
                {canReorder && !mini && botIds.length > 1 ? <button type="button" className={styles.grip} aria-label={t("sidebar.reorderBot", { name: view.name })} title={t("sidebar.reorderHint")} {...reorder.gripProps(bot.id)}><GripVertical aria-hidden="true" /></button> : null}
              </div>;
            })}
            </div>
          </SidebarSection>

          <SidebarFooter className={styles.railFoot}>
            {import.meta.env.DEV && new URLSearchParams(window.location.search).get("dev") === "1" ? <Button variant="ghost" onClick={handleSimulateBotRun}><FlaskConical />{mini ? null : t("dev.simulateBotRun")}</Button> : null}
            <div className={styles.footRow}>
              <AccountMenu name={settings.accountName} computerTarget={settings.computerTarget} compact={mini} onAction={handleAccountAction} />
              <AppsPill connected={appServers.filter(s => s.status === "connected").map(s => s.name)} onClick={() => setAppsOpen(true)} />
            </div>
          </SidebarFooter>
        </Sidebar>
      }
    >
      <div id="main-content" tabIndex={-1} className={styles.mainColumn}>
        <div className={styles.titleBar} data-tauri-drag-region>
          <span className={styles.runtimeLabel} data-tauri-drag-region title={home.kind === "host" && !workLocalNow ? home.name || t("computerPicker.host") : t("account.localComputer")}><i className={styles.liveDot} aria-hidden="true" />{home.kind === "host" && !workLocalNow ? <Server className={styles.runtimeIcon} aria-hidden="true" /> : <Monitor className={styles.runtimeIcon} aria-hidden="true" />}<span className={styles.runtimeName}>{home.kind === "host" && !workLocalNow ? home.name || t("computerPicker.host") : t("account.localComputer")}</span></span>
          {botInView && panelBotView ? (
            <button
              ref={chipRef}
              type="button"
              className={styles.botChip}
              aria-expanded={panelVisible}
              aria-controls="bot-panel"
              title={t("botPanel.toggle")}
              aria-label={`${panelBotView.name}: ${t("botPanel.toggle")}`}
              data-testid="bot-chip"
              onClick={togglePanel}
            >
              <BotAvatar bot={panelBotView} size="xs" decorative />
              <span className={styles.botChipName}>{panelBotView.name}</span>
              <ChevronDown aria-hidden="true" />
            </button>
          ) : <span />}
          <div className={styles.windowActions} data-tauri-drag-region>
            <ComputerPicker />
            <button className={styles.iconButton} title={t("account.settings")} aria-label={t("account.settings")} onClick={() => setSettingsOpen("general")}><Settings2 /></button>
          </div>
        </div>
        {activeComputerId === LOCAL_COMPUTER ? <HealthIndicator /> : null}
        <SafeModeBanner />
        <HostStateBanner />
        <ShellBridgeHost
          settings={settings}
          onOpenChat={openChat}
          onOpenNeeds={openNeeds}
        />
        <HumanDrivingBanner />
        {runBanner}
        {settings.onboardingSkipped && !settings.onboardingCompleted && route.kind !== "onboarding" ? (
          <FinishSetupHint onClick={() => handleOpenOnboarding(false)} />
        ) : null}
        {panelVisible || route.kind === "chat" ? null : <StopStatusNotice variant="main" />}
        <PausedControlBanner />
        {route.kind === "onboarding" && !onboardingBlocked ? (
          <OnboardingView
            settings={settings}
            onLanguageChange={handleLanguageChange}
            onHelp={() => setHelpOpen(true)}
            onSkip={() => {
              updateSettings({ onboardingSkipped: true });
              go({ kind: "home" });
            }}
            onComplete={({ botId }) => {
              updateSettings({ onboardingCompleted: true, onboardingSkipped: false });
              if (botId) {
                go({ kind: "bot", botId });
              } else {
                go({ kind: "home" });
              }
            }}
          />
        ) : null}
        {hostEmpty && (route.kind === "home" || route.kind === "chat" || route.kind === "bot") ? (
          <HostEmptyState onNewBot={() => setNewBotOpen(true)} onConnectModel={() => setSettingsOpen("ai")} />
        ) : null}
        {route.kind === "home" && !hostEmpty ? (
          <div className={styles.centerPane}>
            <Card padding="lg" className={styles.emptyStateCard}>
              {phase !== "ready" && startup ? (
                <StartupProblem startup={startup} raw={daemonError ?? ""} />
              ) : phase === "error" ? (
                <div className={styles.selectedBotContent} data-testid="daemon-offline-empty-state">
                  <span className={`${styles.stateIcon} ${styles.stateIconDanger}`}><ServerOff /></span>
                  <ErrorNotice error={humanService(serviceProblem ?? "down", daemonError ?? "")} />
                  {retryInSeconds ? (
                    <p className={styles.emptyStateDescription}>{t("errors.retrying", { seconds: retryInSeconds })}</p>
                  ) : null}
                  <div className={styles.optionRow}>
                    <Button variant="primary" onClick={() => void restartDaemon()}>
                      {t("errors.restartService")}
                    </Button>
                    <Button variant="secondary" onClick={retry}>
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
        {route.kind === "bot" && !hostEmpty ? (
          <BotEmptyView
            key={route.botId}
            bot={panelBotView && selectedBot?.id === route.botId ? panelBotView : undefined}
            busy={newChatBusy}
            error={newChatError}
            onNewChat={() => void startChat(route.botId)}
            onSuggest={(prompt) => void startChatWith(route.botId, prompt)}
            firstBot={bots[0] ? { id: bots[0].id, name: bots[0].name } : undefined}
            onGoFirst={goToBot}
            onCreateBot={() => setNewBotOpen(true)}
          />
        ) : null}
        {route.kind === "chat" && !hostEmpty ? (
          <ChatView
            key={route.chatId}
            chatId={route.chatId}
            jumpMessageId={route.messageId}
            jumpRequestId={route.jumpRequestId}
            onBack={() => {
              // There is no bot page to go back to: the chat list lives in the right panel.
              if (backFromChatOpensPanel(panelOpen)) setPanelOpen(true);
            }}
            control={controlState}
            onControl={setControlState}
            onSwitchChat={openChat}
            showAgentRoute={settings.showAgentRoute}
            sendWith={settings.sendWith}
            messageTimes={settings.messageTimes}
            liveChatSlots={localChatSlots ?? homeChatSlots}
          />
        ) : null}
        {route.kind === "needs" ? <NeedsYouView onOpenChat={openChat} /> : null}
        {route.kind === "notifications" ? <NotificationHistoryView onOpenChat={openChat} onOpenNeeds={openNeeds} /> : null}
        {route.kind === "schedules" ? (
          <SchedulesView onOpenChat={openChat} />
        ) : null}
      </div>
      <PaletteHost
        currentBotId={routeBotId}
        onScreen={handlePaletteScreen}
        onBot={goToBot}
        onChat={(chatId, botId) => go({ kind: "chat", chatId, botId })}
        onHelp={() => setHelpOpen(true)}
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
          goToBot(botId);
        }}
      />
      <BotProfileModal
        open={botProfileOpen && botInView}
        bot={selectedBot}
        skills={botSkills}
        newChatBusy={newChatBusy}
        onClose={() => setBotProfileOpen(false)}
        onEdit={() => {
          setBotProfileOpen(false);
          setBotEditOpen(true);
        }}
        onNewChat={() => {
          if (selectedBot) void startChat(selectedBot.id);
        }}
      />
      <BotEditModal
        open={botEditOpen && botInView}
        bot={selectedBot}
        skills={botSkills}
        initialSection={review.section}
        onClose={() => setBotEditOpen(false)}
        onAddSkill={() => setAddSkillOpen(true)}
        onDeleted={() => {
          setBotEditOpen(false);
          setRoute({ kind: "home" });
        }}
      />
      <SkillsModal
        open={skillsOpen && botInView}
        botName={panelBotView?.name ?? ""}
        skills={botSkills}
        onClose={() => setSkillsOpen(false)}
        onAddSkill={() => setAddSkillOpen(true)}
      />
      <AddSkillModal
        open={addSkillOpen && botInView}
        client={client}
        botName={panelBotView?.name ?? ""}
        skills={botSkills}
        initialStep={review.installStep ? "install" : undefined}
        onClose={() => setAddSkillOpen(false)}
      />
      <WhatsNewModal notes={showWhatsNew ? whatsNewNotes : null} onClose={recordWhatsNew} />
      <HelpModal
        open={helpOpen}
        onClose={() => setHelpOpen(false)}
        stopHotkey={controlState.stopHotkey || settings.stopHotkey}
        sendWith={settings.sendWith}
        onSetup={() => {
          setHelpOpen(false);
          handleOpenOnboarding(false);
        }}
      />
      <LiveViewer onChatSlots={route.kind === "chat" && !hostEmpty ? setHomeChatSlots : undefined} />
      {activeComputerId !== LOCAL_COMPUTER ? (
        <LiveViewer source="local" onChatSlots={route.kind === "chat" && !hostEmpty ? setLocalChatSlots : undefined} />
      ) : null}
      {customChrome ? <WindowControls /> : null}
    </SidebarLayout>
  );
}

/**
 * The local daemon's safety machinery (heartbeat, native Stop and tray, local event
 * stream, approval overlays) stays connected to THIS computer whichever computer the
 * views show. Selecting a host only swaps the daemon the chat and panel views talk
 * to (a second session and live view, rebuilt per host).
 */
export function ComputerScope({ children }: { children: ReactNode }) {
  const { activeComputerId } = useDaemon();
  const plan = computerScopePlan(activeComputerId, LOCAL_COMPUTER);
  return (
    <LiveViewProvider scope="local">
      {plan.localBridge ? <ControlBridgeHost /> : null}
      {!plan.hostSession ? (
        <Fragment key={LOCAL_COMPUTER}>{children}</Fragment>
      ) : (
        <HostSession key={activeComputerId}>
          <LiveViewProvider>{children}</LiveViewProvider>
        </HostSession>
      )}
    </LiveViewProvider>
  );
}

export function App() {
  return (
    <ToastProvider>
      <DaemonProvider>
        <ServerTerminalProvider>
          <ComputerRequestModal />
          <ComputerScope>
            <LeaveGuardProvider>
              <EngineConsentProvider>
                <AppShell />
              </EngineConsentProvider>
            </LeaveGuardProvider>
          </ComputerScope>
        </ServerTerminalProvider>
      </DaemonProvider>
    </ToastProvider>
  );
}
