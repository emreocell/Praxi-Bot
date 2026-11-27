import { BellDot, BellRing, Bot, CalendarClock, ChevronDown, FlaskConical, GripVertical, Star, LoaderCircle, MessageSquare, Plus, Search, Server, ServerOff, Settings2, Target } from "lucide-react";
import { effectiveWallpaperMotion } from "./lib/accessibility";
import { unreadBotIds } from "./lib/unread";
import { useReadState } from "./store/readStateStore";
import { ApiError } from "./api/client";
import type { Run, UpdateChatInput } from "./api/types";
import { isMockMode } from "./api/mockMode";

import { Fragment, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ConnectivityNotice } from "./components/ConnectivityNotice";
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
import { openChatPopout, openLivePopout } from "./lib/popout";
import styles from "./App.module.css";
import { TITLEBAR_LEAD_ID } from "./lib/titleBar";
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
import {
  deriveSidePanelState,
  hasUnanswerableNeed,
  PANEL_DOCK_MIN_WIDTH,
  screenPanelStep,
  type SidePanelMode
} from "./lib/sidePanelState";
import { useMcp } from "./state/useMcp";
import { AccountMenu, type AccountAction } from "./components/AccountMenu";
import { MessageSearch } from "./components/MessageSearch";
import { botDotStatus, railNavIndex } from "./lib/railNav";
import { HealthIndicator } from "./components/HealthIndicator";
import { ServiceBanner } from "./components/ServiceBanner";
import { ServiceStateProvider } from "./state/ServiceState";
import { HelpModal } from "./components/HelpModal";
import { GettingStarted } from "./components/GettingStarted";
import type { StepTarget } from "./lib/gettingStarted";
import { DemoModal } from "./demo/DemoModal";
import { type DemoTaskId, offerDemo } from "./demo/demoEngine";
import { useProviders } from "./state/useProviders";
import { WhatsNewModal } from "./components/WhatsNewModal";
import { installedReleaseNotes, notesFor, whatsNewAction, type WhatsNewFile } from "./lib/whatsNew";
import { updateStatus, type UpdateInfo } from "./lib/appUpdate";
import whatsNewFile from "./whats-new.json";
import { getVersion } from "@tauri-apps/api/app";
import { isHelpKey } from "./lib/shortcuts";
import { chatTitleLabel } from "./lib/chatTitle";
import { OPEN_CHAT_EVENT, type OpenChatTarget } from "./lib/openChatLink";
import { APP_COMMAND_EVENT, type AppCommandTarget } from "./lib/slash";
import { editorSection } from "./lib/botEditor";
import { BOT_COLORS, botView, draftFromBot } from "./lib/botProfile";
import { isBotMuted, setBotMuted } from "./lib/botMute";
import { makeBotMain } from "./lib/botOrder";
import { botRowMenu } from "./lib/contextMenu";
import { SEARCH_ALL_CHATS_EVENT, type SearchRequest, takeSearchRequest } from "./lib/chatSearchRequest";
import { useContextMenu, useMenuTranslate } from "./components/ContextMenu";
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
import { RunTimelineModal } from "./components/RunTimelineModal";
import { runBarModel } from "./lib/runBar";
import { ToastProvider } from "./components/Toast";
import { ControlBridgeHost } from "./state/ControlBridgeHost";
import { requestStop } from "./state/appStop";
import { isNewerNativeSnapshot } from "./state/stopStatus";
import { StopStatusNotice } from "./components/StopStatusNotice";
import { LiveViewProvider, useLiveView, useLocalLiveView } from "./state/LiveViewContext";
import { HumanDrivingBanner } from "./components/HumanDrivingBanner";
import { LiveViewer } from "./views/LiveViewer";
import type { LiveChatSlots } from "./views/LiveViewerChat";
import { appearanceFromQuery, systemPairOf } from "./lib/appearance";
import { createDebounced } from "./lib/debounce";
import { backdropIsActive, CUSTOM_BACKDROP, NO_BACKDROP } from "./lib/backdropCatalog";
import { Backdrop, sceneColors } from "./components/Backdrop";
import { Wallpaper } from "./components/Wallpaper";
import { resolveWallpaper, wallpaperSceneId, WALLPAPER_DIM } from "./lib/wallpaper";
import { useApplyAppearance } from "./state/useApplyAppearance";
import { usePanelReserve } from "./state/usePanelReserve";
import { useViewportWidth } from "./state/useViewportWidth";
import { PaletteHost, type PaletteScreen } from "./state/PaletteHost";
import { DigestNotifier } from "./state/DigestNotifier";
import { OwnerNoticeSync } from "./state/OwnerNoticeSync";
import { ShellBridgeHost } from "./state/ShellBridgeHost";
import { AddSkillModal } from "./components/AddSkillModal";
import { useToast } from "./components/Toast";
import { createChatFor, newChatFailureNeedsToast } from "./lib/newChat";
import { LeaveGuardProvider, useLeaveRegistry } from "./state/LeaveGuard";
import { SkillMarketModal } from "./components/SkillMarketModal";
import { SkillsModal } from "./components/SkillsModal";
import { useBotSkills } from "./state/useBotSkills";
import { BotEditModal } from "./views/BotEditModal";
import { BotEmptyView } from "./views/BotEmptyView";
import { ErrorNotice } from "./components/ErrorNotice";
import { StartingNotice, StartupProblem } from "./components/StartupProblem";
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
import { HomeFirstSteps } from "./components/HomeFirstSteps";
import { HostStateBanner } from "./components/HostStateBanner";
import { SafeModeBanner } from "./components/SafeModeBanner";
import { WorkspaceWarningBanner } from "./components/WorkspaceWarningBanner";
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
import { actionableCount } from "./lib/notificationHistory";
import { restoreNotificationHistory, useNotificationHistory } from "./store/notificationHistoryStore";
import { OnboardingView } from "./views/OnboardingView";
import { SchedulesView } from "./views/SchedulesView";
import { WorkflowsView } from "./views/WorkflowsView";
import { AppsModal } from "./views/AppsModal";
import { NewBotModal } from "./views/NewBotModal";
import { RecipeFlow } from "./views/RecipeFlow";
import type { PrereqFix } from "./lib/recipePrereqs";
import { MyBotsView } from "./views/MyBotsView";
import { nativePauseAction } from "./lib/myBots";
import { SettingsModal, type SettingsTargetRequest } from "./views/SettingsModal";
import { VaultModal } from "./views/VaultModal";
import { type AppsStart, OPEN_APPS_EVENT, OPEN_SETTINGS_EVENT, OPEN_VAULT_EVENT, type SettingsTarget } from "./lib/settingsLink";
import { OPEN_AI_SETTINGS_EVENT } from "./state/useCliEngineRows";
import { OPEN_BOT_LIMITS_EVENT } from "./lib/botLimits";
import type { BotEditorSection } from "./lib/botEditor";
import { EngineConsentProvider } from "./state/EngineConsent";
import type { SettingsSectionId } from "./views/settings/types";
import type { SettingsPageId } from "./lib/settingsPages";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { createDeepLinkDispatcher } from "./lib/deepLinks";
import { onDeepLinkPending } from "./lib/shell";
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
  | { kind: "myBots" }
  | { kind: "onboarding" }
  /** A bot without any chat yet: the empty state (there is no bot page). */
  | { kind: "bot"; botId: string; edit?: boolean }
  | { kind: "chat"; chatId: string; botId: string; messageId?: string; jumpRequestId?: number; external?: boolean }
  | { kind: "needs" }
  | { kind: "notifications" }
  | { kind: "schedules"; botId?: string }
  | { kind: "workflows"; botId?: string };

function AppShell({ deepLinks }: { deepLinks: ReturnType<typeof createDeepLinkDispatcher> }) {
  const { t } = useTranslation();
  const {
    phase,
    error: daemonError,
    runs,
    events,
    bots,
    chats,
    needs,
    conn,
    client,
    refreshBots,
    refreshChats,
    refreshRuns,
    pendingChatIds,
    updateChat,
    deleteChat,
    retry,
    restartDaemon,
    homeStale,
    serviceProblem,
    startup,
    retryInSeconds,
    activeComputerId,
    hostState,
    initialComputer,
    isActiveComputer
  } = useDaemon();
  const {
    openViewer,
    viewerOpen
  } = useLiveView();
  const localViewerOpen = useLocalLiveView().viewerOpen;
  const readState = useReadState(client?.baseUrl ?? "");
  const unreadBots = useMemo(() => unreadBotIds(chats, runs, needs, readState), [chats, runs, needs, readState]);
  const [settings, setSettings] = useState<AppSettings>(() => loadSettingsSync() ?? DEFAULT_SETTINGS);
  const [route, setRoute] = useState<Route>({ kind: "home" });
  const [linkedRun, setLinkedRun] = useState<Run | null>(null);
  const [homeChatSlots, setHomeChatSlots] = useState<LiveChatSlots | null>(null);
  const [localChatSlots, setLocalChatSlots] = useState<LiveChatSlots | null>(null);
  // Every route change goes through this gate: a bot dialog with unsaved data asks first, and one
  // that is saving blocks it (PR #95, finding 4).  Do not call `setRoute` directly elsewhere.
  const leave = useLeaveRegistry();
  const go = (next: Route) => {
    leave.attempt(() => setRoute(next));
  };
  const toast = useToast();
  const popoutFailed = () => toast.show(t("popout.openFailed"), "danger");
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
  const hostVersionBlocked = hostState?.kind === "version_mismatch" && hostState.blocking;
  // The bots' computer has no bots to show (not set up yet, or not answering): one calm screen, never a blank app.
  // A connection whose client is withheld (version check pending or blocked) shows the version screen, not this one;
  // no connection at all is the "not answering" case this screen is for.
  const hostEmpty = onboardingBlocked && phase === "ready" && (client !== null || conn === null) && bots.length === 0;
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
  const [settingsOpen, setSettingsOpen] = useState<SettingsSectionId | SettingsPageId | null>(
    null
  );
  const [settingsTarget, setSettingsTarget] = useState<SettingsTargetRequest | null>(null);
  const settingsSeq = useRef(0);
  const openSettings = useCallback((page: SettingsSectionId | SettingsPageId, card?: string, field?: string) => {
    setSettingsTarget(card ? { page, card, field, seq: ++settingsSeq.current } : null);
    setSettingsOpen(page);
  }, []);
  const [appsOpen, setAppsOpen] = useState(false);
  // The guided mail flow sends the owner to the Apps window for the sign-in step (it stays on top of Settings).
  const [appsStart, setAppsStart] = useState<AppsStart | null>(null);
  useEffect(() => {
    const open = (e: Event) => {
      setAppsStart((e as CustomEvent<AppsStart>).detail ?? null);
      setAppsOpen(true);
    };
    window.addEventListener(OPEN_APPS_EVENT, open);
    return () => window.removeEventListener(OPEN_APPS_EVENT, open);
  }, []);
  // "Start from a goal": the recipe dialog stays open under the screen a fix opens (answers are kept)
  // and checks again once that screen closes.
  const [recipeOpen, setRecipeOpen] = useState(false);
  const [recipeRecheck, setRecipeRecheck] = useState(0);
  useEffect(() => {
    if (settingsOpen === null && !appsOpen) setRecipeRecheck((k) => k + 1);
  }, [settingsOpen, appsOpen]);
  const openRecipeFix = (fix: PrereqFix) => {
    if (fix.kind === "apps") setAppsOpen(true);
    else setSettingsOpen(fix.section);
  };
  const [vaultOpen, setVaultOpen] = useState(false);
  // The model picker's "Install <CLI>" rows open Settings > AI models, where the install flow lives.
  useEffect(() => {
    const open = () => openSettings("models", "detectedAccounts");
    window.addEventListener(OPEN_AI_SETTINGS_EVENT, open);
    return () => window.removeEventListener(OPEN_AI_SETTINGS_EVENT, open);
  }, [openSettings]);
  // A message can point at a place in Settings (e.g. the voice call card's "Open settings").
  useEffect(() => {
    const open = (e: Event) => {
      const detail = (e as CustomEvent<SettingsTarget>).detail;
      openSettings(detail.page, detail.card, detail.field);
    };
    window.addEventListener(OPEN_SETTINGS_EVENT, open);
    return () => window.removeEventListener(OPEN_SETTINGS_EVENT, open);
  }, [openSettings]);
  // Settings -> Apps & connections -> Vault button opens the Vault modal cleanly.
  useEffect(() => {
    const open = () => {
      setSettingsOpen(null);
      setVaultOpen(true);
    };
    window.addEventListener(OPEN_VAULT_EVENT, open);
    return () => window.removeEventListener(OPEN_VAULT_EVENT, open);
  }, []);
  const [newBotOpen, setNewBotOpen] = useState(false);
  // The dialogs of a bot (card 244-C): edit and profile replace the old pages; "add skill" can
  // sit on top of the skills dialog or the edit dialog.
  const [botEditOpen, setBotEditOpen] = useState(false);
  const [botProfileOpen, setBotProfileOpen] = useState(false);
  const [skillsOpen, setSkillsOpen] = useState(false);
  const [addSkillOpen, setAddSkillOpen] = useState(false);
  const [marketOpen, setMarketOpen] = useState(false);
  // Review hooks (mock mode only): `?screen=botform&section=models`, `?screen=addskill&step=install`.
  const [review] = useState(() => {
    const q = isMockMode() && typeof window !== "undefined" ? new URLSearchParams(window.location.search) : null;
    return { section: editorSection(q?.get("section")), installStep: q?.get("step") === "install" };
  });
  // "Raise limit" on a stopped run opens the editor on its Limits section.
  const [editSection, setEditSection] = useState<BotEditorSection | undefined>(review.section);
  useEffect(() => {
    const open = () => {
      setEditSection("limits");
      setBotEditOpen(true);
    };
    window.addEventListener(OPEN_BOT_LIMITS_EVENT, open);
    return () => window.removeEventListener(OPEN_BOT_LIMITS_EVENT, open);
  }, []);
  const [newChatBusy, setNewChatBusy] = useState(false);
  const [newChatError, setNewChatError] = useState<string | null>(null);
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchSeed, setSearchSeed] = useState<SearchRequest | null>(null);
  const { servers: appServers } = useMcp(true);
  const [helpOpen, setHelpOpen] = useState(false);
  // The offline demo: `null` task = closed. It lives only here, never as a chat.
  const [demo, setDemo] = useState<{ task: DemoTaskId | null } | null>(null);
  const { dto: providersDto } = useProviders();
  const showDemoOffer = offerDemo(providersDto);
  const [settingsLoaded, setSettingsLoaded] = useState(false);
  const [linkOwner] = useState(() => ({}));
  const linkSession = useMemo(() => client === null ? null : {}, [client]);
  useLayoutEffect(() => () => deepLinks.detach(linkOwner), [deepLinks, linkOwner]);
  useLayoutEffect(() => {
    if (!isTauriRuntime()) return;
    const waiting = !settingsLoaded || initialComputer === "waiting" ||
      (initialComputer === "ready" && (phase === "connecting" || hostState?.kind === "connecting" || (conn && !client && !hostVersionBlocked)));
    const offline = initialComputer === "offline" || phase === "error" || !linkSession ||
      hostState?.kind === "offline" || hostState?.kind === "not_paired" ||
      hostVersionBlocked;
    deepLinks.update({
      owner: linkOwner,
      computerId: initialComputer === "ready" ? activeComputerId : null,
      selected: () => isActiveComputer(activeComputerId),
      connection: waiting ? "waiting" : offline ? "offline" : {
        key: linkSession, refreshBots, refreshChats, refreshRuns
      },
      open: (destination) => {
        if (settingsOpen !== null || newBotOpen || recipeOpen || newChatBusy || pendingChatIds.size > 0) return false;
        return leave.attemptSilently(() => {
          if (destination.kind === "run") {
            setLinkedRun(destination.run);
          } else {
            setLinkedRun(null);
            setRoute(destination.kind === "chat" ? { ...destination, external: true } : destination);
          }
        }) === "went";
      },
      feedback: (failure) => toast.show(t(`deepLinks.${failure}`), "danger")
    });
  });
  const [appVersion, setAppVersion] = useState("");
  const [installedNotes, setInstalledNotes] = useState<UpdateInfo | null>(null);
  const [updateNotesLoaded, setUpdateNotesLoaded] = useState(false);
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
        ...(mock ? { onboardingCompleted: true, language: (["en", "de", "es", "pt", "id"] as const).find((l) => l === params.get("lang")) ?? ("tr" as const) } : {}),
      };
      if (mock) Object.assign(merged, appearanceFromQuery(params, merged));
      if (mock) {
        setRoute({ kind: "chat", chatId: "33333333-3333-4333-8333-333333333333", botId: "11111111-1111-4111-8111-111111111111" });
        const screen = params.get("screen");
        if (screen === "settings") setSettingsOpen((params.get("section") ?? "general") as SettingsSectionId);
        if (screen === "apps") setAppsOpen(true);
        if (screen === "vault") setVaultOpen(true);
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
        if (screen === "myBots") setRoute({ kind: "myBots" });
        if (screen === "needs") setRoute({ kind: "needs" });
        if (screen === "notifications" || screen === "notifications-empty") setRoute({ kind: "notifications" });
        if (screen === "home" || screen === "offline" || screen === "nobots") setRoute({ kind: "home" });
      }
      setSettings(merged);
      setSettingsLoaded(true);
      if (shouldOpenOnboardingOnStart(merged)) {
        setRoute({ kind: "onboarding" });
      }
      if (merged.language && merged.language !== i18n.language) {
        await i18n.changeLanguage(merged.language);
      }
      if (mock) {
        const screen = params.get("screen");
        if (screen === "notifications-empty") {
          restoreNotificationHistory([]);
        } else if (screen === "notifications") {
          const now = Date.now();
          const atlasId = "11111111-1111-4111-8111-111111111111";
          const novaId = "22222222-2222-4222-8222-222222222222";
          const chatId = "33333333-3333-4333-8333-333333333333";
          const openSite = i18n.t("notify.approval.openSite", { bot: "Atlas", site: "webioo.com.tr" });
          const busyFailed = i18n.t("notify.failed.retrying", {
            reason: i18n.t("notify.failed.reasonBusy"),
            seconds: "46"
          });
          restoreNotificationHistory([
            {
              id: "mock-n1",
              kind: "needsYou",
              title: openSite,
              body: "",
              botId: atlasId,
              botName: "Atlas",
              chatId,
              runId: "mock-live",
              callId: "mock-open-webioo",
              ts: now - 45_000,
              shown: true,
              read: false
            },
            {
              id: "mock-n2",
              kind: "needsYou",
              title: openSite,
              body: "",
              botId: atlasId,
              botName: "Atlas",
              chatId,
              runId: "mock-live",
              ts: now - 75_000,
              shown: true,
              read: false
            },
            {
              id: "mock-n3",
              kind: "needsYou",
              title: openSite,
              body: "",
              botId: atlasId,
              botName: "Atlas",
              chatId,
              runId: "mock-live",
              ts: now - 110_000,
              shown: true,
              read: false
            },
            {
              id: "mock-n4",
              kind: "needsYou",
              title: openSite,
              body: "",
              botId: atlasId,
              botName: "Atlas",
              chatId,
              runId: "mock-live",
              ts: now - 140_000,
              shown: true,
              read: false
            },
            {
              id: "mock-n5",
              kind: "longRun",
              title: i18n.t("notify.generic.longRun"),
              body: "",
              botId: novaId,
              botName: "Nova",
              chatId,
              runId: "mock-long-1",
              ts: now - 2 * 60_000,
              shown: true,
              read: false
            },
            {
              id: "mock-n6",
              kind: "failed",
              title: busyFailed,
              body: "",
              botId: "bot-milo",
              botName: "Milo",
              chatId,
              runId: "mock-fail-1",
              retryAtMs: now + 46_000,
              retryable: true,
              ts: now - 5 * 60_000,
              shown: true,
              read: false
            },
            {
              id: "mock-n7",
              kind: "scheduleDone",
              title: i18n.t("notify.generic.scheduleDone"),
              body: "",
              botId: "bot-ember",
              botName: "Ember",
              chatId,
              runId: "mock-sched-1",
              ts: now - 35 * 60_000,
              shown: false,
              heldBy: "quiet",
              read: false
            },
            {
              id: "mock-n8",
              kind: "digest",
              title: i18n.t("digest.notifyTitle"),
              body: "2 done · $0.14",
              botId: "",
              botName: "",
              chatId: "",
              ts: now - 2 * 86_400_000,
              shown: true,
              read: true
            }
          ]);
        }
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
  const baseTheme = resolveTheme(settings.themeMode, settings.customTheme, systemPairOf(settings));
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
    } else if (action === "vault") {
      setVaultOpen(true);
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

  // Ctrl/Cmd + Shift + F searches the message history of every bot.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.altKey && e.shiftKey && e.key.toLowerCase() === "f") {
        e.preventDefault();
        setSearchOpen(true);
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
  // A card inside Settings (the Usage page's expensive runs) opens a chat and gets out of the way.
  const openChatRef = useRef(openChat);
  openChatRef.current = openChat;
  useEffect(() => {
    const onOpen = (e: Event) => {
      setSettingsOpen(null);
      openChatRef.current((e as CustomEvent<OpenChatTarget>).detail.chatId);
    };
    window.addEventListener(OPEN_CHAT_EVENT, onOpen);
    return () => window.removeEventListener(OPEN_CHAT_EVENT, onOpen);
  }, []);
  const openNeeds = () => go({ kind: "needs" });
  /** Where a "Getting started" step's button leads: always an existing screen, never a new flow. */
  const openGettingStartedTarget = (target: StepTarget, botId: string | undefined) => {
    if (target === "ai") setSettingsOpen("ai");
    else if (target === "computer") setSettingsOpen("computer");
    else if (target === "apps") setAppsOpen(true);
    else if (target === "schedules") go({ kind: "schedules" });
    else if (botId) void startChat(botId);
    else setNewBotOpen(true);
  };
  const notifyHistory = useNotificationHistory();
  const liveCallIds = useMemo(() => new Set(needs.map((n) => n.callId)), [needs]);
  const notifyActionCount = actionableCount(notifyHistory, Date.now(), liveCallIds);
  // Selecting a bot: its latest chat, or the empty state when it has none.
  const goToBot = (botId: string) => go(botLanding(botId, chats));
  // Right-click on a bot in the rail.  The profile and settings dialogs belong to the selected bot
  // (another bot closes them), so for a bot that is not selected they open once it is.
  const botMenu = useContextMenu();
  const menuT = useMenuTranslate();
  const [pendingBotDialog, setPendingBotDialog] = useState<{ botId: string; dialog: "profile" | "edit" } | null>(null);
  const openBotDialog = (botId: string, dialog: "profile" | "edit") => {
    if (selectedBot?.id !== botId || !botInView) goToBot(botId);
    setPendingBotDialog({ botId, dialog });
  };
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
  const requestedBotEdit = route.kind === "bot" && route.edit === true;
  const lastDialogBot = useRef<string | undefined>(undefined);
  useEffect(() => {
    // (A first selection - mock review links open a dialog together with the route - keeps them.)
    if (lastDialogBot.current !== undefined || requestedBotEdit) {
      setBotEditOpen(requestedBotEdit);
      setBotProfileOpen(false);
      setSkillsOpen(false);
      setAddSkillOpen(false);
    }
    lastDialogBot.current = dialogBotId;
  }, [dialogBotId, requestedBotEdit]);
  useEffect(() => {
    if (!pendingBotDialog || dialogBotId !== pendingBotDialog.botId) return;
    if (pendingBotDialog.dialog === "profile") setBotProfileOpen(true);
    else setBotEditOpen(true);
    setPendingBotDialog(null);
  }, [pendingBotDialog, dialogBotId]);
  // "Search all chats" from a right-click menu opens the message search with the phrase filled in.
  useEffect(() => {
    const open = () => {
      const request = takeSearchRequest();
      if (!request) return;
      setSearchSeed(request);
      setSearchOpen(true);
    };
    window.addEventListener(SEARCH_ALL_CHATS_EVENT, open);
    return () => window.removeEventListener(SEARCH_ALL_CHATS_EVENT, open);
  }, []);
  const float = isFloatShell(shellSettings.uiStyle, backdropIsActive(shellSettings.backdrop, shellSettings.backgroundUrl));
  const panelWidth = Math.min(layout.panel, panelMax(viewportWidth, layout.rail, float));
  // The panel overlays (instead of squeezing the chat) only while the full-screen live view is open.
  // A preview stream started by the computer card must not flip the panel's mode a moment after the
  // owner opened it, which would make the chat column jump.
  const screenOpen = viewerOpen || localViewerOpen;
  const needAttentionKey = needs
    .filter((n) => n.botId === selectedBot?.id)
    .map((n) => n.callId)
    .sort()
    .join(",");
  const [dismissedNeedKey, setDismissedNeedKey] = useState("");
  // The full-screen view takes the whole window: the panel steps aside when it opens and returns, if it
  // was open, when it closes (`screenPanelStep`). Esc in the view closes it, and with it this hiding.
  const wasScreenOpen = useRef(false);
  const panelStash = useRef<boolean | null>(null);
  const panelOpenRef = useRef(panelOpen);
  panelOpenRef.current = panelOpen;
  useEffect(() => {
    const step = screenPanelStep({
      wasScreenOpen: wasScreenOpen.current,
      screenOpen,
      panelOpen: panelOpenRef.current,
      stash: panelStash.current
    });
    wasScreenOpen.current = screenOpen;
    panelStash.current = step.stash;
    if (step.panelOpen !== panelOpenRef.current) setPanelOpen(step.panelOpen);
  }, [screenOpen]);
  const unanswerableNeed = hasUnanswerableNeed({
    routeKind: route.kind,
    selectedBotId: selectedBot?.id,
    activeChatId: route.kind === "chat" ? route.chatId : undefined,
    needs
  });
  const attentionDismissed = dismissedNeedKey !== "" && dismissedNeedKey === needAttentionKey;
  const panelState = deriveSidePanelState({
    botInView,
    ownerOpen: panelOpen,
    unanswerableNeed,
    attentionDismissed,
    liveViewState: screenOpen ? "live" : "idle",
    viewportWidth,
    storedPanelWidth: panelWidth,
    railWidth: RAIL_MIN,
    float
  });
  const panelVisible = panelState.visible;
  const panelDocked = panelState.mode === "docked";
  const lastPanelModeRef = useRef<Exclude<SidePanelMode, "hidden">>("overlay");
  if (panelState.mode !== "hidden") {
    lastPanelModeRef.current = panelState.mode;
  }
  const panelRenderMode: Exclude<SidePanelMode, "hidden"> =
    panelState.mode !== "hidden"
      ? panelState.mode
      : screenOpen || viewportWidth < PANEL_DOCK_MIN_WIDTH
        ? "overlay"
        : lastPanelModeRef.current;
  const panelReserve = usePanelReserve(panelDocked);
  const railWidth = fitRail(viewportWidth, layout.rail, panelWidth, float, panelReserve);
  const fitStored = (candidate: number) => fitRail(viewportWidth, candidate, panelWidth, float, panelReserve);
  // A dialog opened by a shortcut sits above the message search: close the search instead of leaving it beneath.
  const dialogOpen = settingsOpen !== null || appsOpen || newBotOpen || recipeOpen || helpOpen || botEditOpen || botProfileOpen || skillsOpen || addSkillOpen || marketOpen || vaultOpen;
  useEffect(() => {
    if (dialogOpen) setSearchOpen(false);
  }, [dialogOpen]);

  // "What's new": bundled notes of the installed version, shown once per version (never blocks, no network).
  useEffect(() => {
    // Review state `?mock=1&screen=whatsnew`: the card for the bundled notes (no Tauri runtime to ask).
    if (isMockMode() && new URLSearchParams(window.location.search).get("screen") === "whatsnew") setAppVersion("0.1.0");
    if (!isTauriRuntime()) return;
    void getVersion().then(setAppVersion).catch(() => undefined);
  }, []);
  const notesLanguage = i18n.language;
  useEffect(() => {
    let cancelled = false;
    void updateStatus(notesLanguage).then((status) => {
      if (!cancelled) setInstalledNotes(status.installedNotes);
    }).catch(() => undefined).finally(() => {
      if (!cancelled) setUpdateNotesLoaded(true);
    });
    return () => { cancelled = true; };
  }, [notesLanguage]);
  const whatsNewNotes = installedReleaseNotes(installedNotes, appVersion, t("whatsNew.label"))
    ?? notesFor(whatsNewFile as WhatsNewFile, appVersion, i18n.language);
  const whatsNewStep = settingsLoaded && updateNotesLoaded
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
      if (unanswerableNeed && needAttentionKey) {
        setDismissedNeedKey(needAttentionKey);
      }
      chipRef.current?.focus();
    });
  const togglePanel = () => (panelVisible ? closePanel() : setPanelOpen(true));

  // The panel closes through its own X, the control that opened it, or Escape (`panelDismissedBy`).
  // There is deliberately no click-away: the owner clicks into the chat and the composer while the
  // panel stays open. With the full-screen live view open, the viewer's Escape closes the panel first.
  useEffect(() => {
    if (!panelVisible || panelRenderMode !== "overlay" || screenOpen || dialogOpen || demo !== null) {
      return undefined;
    }
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !e.defaultPrevented && !helpOpen) {
        e.preventDefault();
        leave.attempt(() => {
          setPanelOpen(false);
          if (unanswerableNeed && needAttentionKey) {
            setDismissedNeedKey(needAttentionKey);
          }
          chipRef.current?.focus();
        });
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [
    panelVisible,
    panelRenderMode,
    screenOpen,
    dialogOpen,
    demo,
    helpOpen,
    leave,
    unanswerableNeed,
    needAttentionKey
  ]);
  // What the full-screen live view needs to offer (and respect) the right-hand bot panel.
  const liveInfoPanel = botInView
    ? {
        open: panelVisible,
        width: panelWidth,
        onToggle: togglePanel,
        onClose: closePanel
      }
    : undefined;
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
          <Wallpaper kind={wallpaperKind} motion={effectiveWallpaperMotion(settings.wallpaperMotion, settings.reduceMotion)} />
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
          mode={panelRenderMode}
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
              onPopOutChat={(chatId, opts) => {
                const target = chats.find((c) => c.id === chatId);
                void openChatPopout(chatId, {
                  ...opts,
                  computerId: activeComputerId,
                  title: chatTitleLabel(target?.title, t("bots.newChat")),
                  windowFrame: settings.windowFrame
                }).catch(popoutFailed);
              }}
              onPopOutLive={(opts) => {
                const liveSource = opts?.source ?? "home";
                void openLivePopout({
            title: t("liveView.title"),
                  liveSource,
                  targetId: opts?.targetId,
                  nearCursor: opts?.nearCursor,
                  botId: selectedBot.id,
                  computerId: liveSource === "local" ? LOCAL_COMPUTER : activeComputerId,
                  windowFrame: settings.windowFrame
                }).catch(popoutFailed);
              }}
              onNewChat={() => void startChat(selectedBot.id)}
              onPatchChat={handlePatchChat}
              onDeleteChat={handleDeleteChat}
              onOpenProfile={() => setBotProfileOpen(true)}
              onOpenSettings={() => setBotEditOpen(true)}
              onManageSkills={() => setSkillsOpen(true)}
              onAddSkill={() => setAddSkillOpen(true)}
              onOpenMail={() => setSettingsOpen("mail")}
              onOpenSchedules={() => go({ kind: "schedules" })}
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
              // The message box first: a plain selector list returns the first match in page order,
              // which in a chat is a button inside the oldest message.
              const target =
                main?.querySelector<HTMLElement>("[data-composer-input]:not([disabled])") ??
                main?.querySelector<HTMLElement>(
                  "textarea:not([disabled]), [data-autofocus], h1[tabindex], button:not([disabled]), [href], input:not([disabled])"
                ) ??
                main;
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
              <img className={styles.brandMark} src="/apple-touch-icon.png" alt="" width={28} height={28} draggable={false} data-testid="brand-mark" />
              <b className={styles.brandName}>{PRODUCT_CONFIG.appName}</b>
              <div className={styles.topActions}>
                <button className={styles.iconButton} title={t("myBots.title")} aria-label={t("myBots.title")} aria-current={route.kind === "myBots" ? "page" : undefined} data-testid="my-bots-button" onClick={() => go({ kind: "myBots" })}><Bot /></button>
                <button className={styles.iconButton} title={t("messageSearch.open")} aria-label={t("messageSearch.open")} aria-keyshortcuts="Control+Shift+F Meta+Shift+F" data-testid="message-search-button" onClick={() => setSearchOpen(true)}><Search /></button>
                <button className={styles.iconButton} title={t("sidebar.newBot")} aria-label={t("sidebar.newBot")} data-testid="new-bot-button" onClick={() => setNewBotOpen(true)}><Plus /></button>
              </div>
            </div>
          </SidebarHeader>
          <SidebarSection>
            <span className={styles.srOnly} role="status" aria-live="polite">{orderNote}</span>
            <div
              ref={reorder.containerRef}
              className={styles.botList}
              data-stale={homeStale ? "true" : undefined}
              onKeyDown={(e) => {
                // Up / Down / Home / End move between the rail's rows (the reorder grip keeps its own arrows).
                const row = e.target as HTMLElement;
                if (!row.matches("[data-rail-nav]") || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
                const rows = [...e.currentTarget.querySelectorAll<HTMLElement>("[data-rail-nav]")];
                const next = railNavIndex(rows.indexOf(row), rows.length, e.key);
                if (next === null) return;
                e.preventDefault();
                rows[next]?.focus();
              }}
            >
            {needs.length > 0 ? <SidebarItem className={`${styles.railItem} ${styles.railLink}`} active={route.kind === "needs"} title={t("sidebar.needsYou")} aria-label={mini ? t("sidebar.needsYouCount", { count: needs.length }) : undefined} onClick={openNeeds} data-rail-nav><BellDot /><span className={`${styles.botName} ${styles.railText}`}>{t("sidebar.needsYou")}</span><span className={styles.needsBadge} data-testid="needs-you-badge" aria-label={t("sidebar.needsYouCount", { count: needs.length })}>{needs.length}</span></SidebarItem> : null}
            {notifyHistory.length > 0 || route.kind === "notifications" ? <SidebarItem className={`${styles.railItem} ${styles.railLink}`} active={route.kind === "notifications"} title={t("sidebar.notifications")} aria-label={mini && notifyActionCount > 0 ? t("sidebar.notificationsCount", { count: notifyActionCount }) : mini ? t("sidebar.notifications") : undefined} onClick={() => go({ kind: "notifications" })} data-testid="sidebar-notifications" data-rail-nav><BellRing /><span className={`${styles.botName} ${styles.railText}`}>{t("sidebar.notifications")}</span>{notifyActionCount > 0 ? <span className={styles.notifyBadge} data-testid="notify-history-badge" aria-label={t("sidebar.notificationsCount", { count: notifyActionCount })}>{notifyActionCount}</span> : null}</SidebarItem> : null}
            <SidebarItem className={`${styles.railItem} ${styles.railLink}`} active={route.kind === "schedules"} title={t("sidebar.schedules")} aria-label={mini ? t("sidebar.schedules") : undefined} onClick={() => go({ kind: "schedules" })} data-testid="sidebar-schedules" data-rail-nav><CalendarClock /><span className={`${styles.botName} ${styles.railText}`}>{t("sidebar.schedules")}</span></SidebarItem>
            {bots.map((bot) => {
              const view = botView(bot);
              const latest = botChats(chats, bot.id).find((c) => !c.archived);
              const config = bot.config as { bot?: { preview?: string; state?: string; unread?: boolean } };
              const live = runs.find(r => r.bot_id === bot.id && isProcessingRunState(r.state));
              const state = needs.some(n => n.botId === bot.id) || live?.state === "waiting_approval" ? "waiting-approval" : live ? "working" : isMockMode() ? config.bot?.state : "idle";
              const avatarState = state === "thinking" || state === "working" || state === "waiting-approval" || state === "error" ? state : "idle";
              const dot = botDotStatus(avatarState);
              const dotLabel = t(`sidebar.status.${dot}`);
              const index = botIds.indexOf(bot.id);
              const canReorder = true;
              const drag = reorder.drag;
              const dropEdge = drag && drag.id !== bot.id && drag.over === index ? (drag.over < drag.from ? "before" : "after") : undefined;
              const rowMenu = botMenu.bind(() => botRowMenu({
                t: menuT,
                name: view.name,
                muted: isBotMuted(settings.mutedBotIds, bot.id),
                onOpen: () => goToBot(bot.id),
                onToggleMute: () => updateSettings({ mutedBotIds: setBotMuted(settings.mutedBotIds, bot.id, !isBotMuted(settings.mutedBotIds, bot.id)) }),
                onMakeMain: botIds.length > 1 && index !== 0 ? () => saveBotOrder(makeBotMain(botIds, bot.id)) : undefined,
                onProfile: () => openBotDialog(bot.id, "profile"),
                onSettings: () => openBotDialog(bot.id, "edit")
              }));
              return <div key={bot.id} onContextMenu={rowMenu.onContextMenu} onKeyDown={rowMenu.onKeyDown} className={styles.botRow} data-dragging={drag?.id === bot.id ? "true" : undefined} data-drop={dropEdge} {...(canReorder ? reorder.rowProps(bot.id) : { "data-bot-id": bot.id })}>
                <SidebarItem className={styles.railItem} active={routeBotId === bot.id} title={`${view.name} · ${dotLabel}`} aria-label={mini ? `${view.name}, ${dotLabel}` : undefined} onClick={() => goToBot(bot.id)} data-rail-nav data-status={dot}>
                  <span className={styles.avatarWrap}>
                    <BotAvatar bot={view} size="md" state={avatarState} decorative />
                    <span className={styles.statusDot} data-status={dot} aria-hidden="true" data-testid="bot-status-dot" />
                  </span>
                  <span className={styles.srOnly}>{dotLabel}</span>
                  <div className={`${styles.botMeta} ${styles.railText}`}><span className={styles.botName}><span className={styles.botNameText}>{view.name}</span>{index === 0 && botIds.length > 1 ? <span className={styles.mainMark} role="img" title={t("sidebar.mainBotHint")} aria-label={t("sidebar.mainBotHint")}><Star aria-hidden="true" /></span> : null}</span><span className={styles.botRole}>{(isMockMode() ? config.bot?.preview : undefined) ?? chatTitleLabel(latest?.title, t("bots.newChat"))}</span></div>
                  {unreadBots.has(bot.id) ? <span className={styles.unreadDot} role="img" aria-label={t("sidebar.unreadBot")} data-testid="bot-unread" /> : null}
                </SidebarItem>
                {canReorder && !mini && botIds.length > 1 ? <button type="button" className={styles.grip} aria-label={t("sidebar.reorderBot", { name: view.name })} title={t("sidebar.reorderHint")} {...reorder.gripProps(bot.id)}><GripVertical aria-hidden="true" /></button> : null}
              </div>;
            })}
            <SidebarItem
              className={`${styles.railItem} ${styles.newBotRow}`}
              title={t("sidebar.newBot")}
              aria-label={mini ? t("sidebar.newBot") : undefined}
              onClick={() => setNewBotOpen(true)}
              data-rail-nav
              data-testid="sidebar-new-bot"
            >
              <span className={styles.newBotIcon} aria-hidden="true"><Plus /></span>
              <span className={`${styles.newBotText} ${styles.railText}`}>{t("sidebar.newBot")}</span>
            </SidebarItem>
            {botMenu.menu}
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
        <ConnectivityNotice />
        <div className={styles.titleBar} data-tauri-drag-region>
          {/* One place picker only (the one on the right). This cell is the header's leading slot (drag region, room for a Back button) and keeps the bot chip centred. */}
          <div className={styles.titleLead} id={TITLEBAR_LEAD_ID} data-tauri-drag-region />
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
        <ServiceBanner />
        <SafeModeBanner />
        <HostStateBanner />
        <WorkspaceWarningBanner onOpenStorage={() => setSettingsOpen("computer")} />
        <ShellBridgeHost
          settings={settings}
          onOpenChat={openChat}
          onOpenNeeds={openNeeds}
          onTrayNoticeShown={() => updateSettings({ trayNoticeSeen: true })}
        />
        <DigestNotifier
          digestTime={settings.digestTime}
          digestLastDay={settings.digestLastDay}
          quietHours={settings.quietHours}
          isPrivate={settings.notifyPrivate}
          onHandled={(day) => updateSettings({ digestLastDay: day })}
        />
        <OwnerNoticeSync isPrivate={settings.notifyPrivate} />
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
            onDemo={() => setDemo({ task: null })}
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
        {route.kind === "home" && !hostEmpty && !hostVersionBlocked ? (
          <div className={styles.centerPane}>
            <Card padding="lg" className={styles.emptyStateCard}>
              {phase !== "ready" && startup ? (
                <StartupProblem startup={startup} raw={daemonError ?? ""} />
              ) : phase === "error" ? (
                <div className={styles.selectedBotContent} data-testid="daemon-offline-empty-state">
                  <span className={`${styles.stateIcon} ${serviceProblem === "starting" ? "" : styles.stateIconDanger}`}>
                    {serviceProblem === "starting" ? <Server /> : <ServerOff />}
                  </span>
                  {serviceProblem === "starting" ? (
                    <StartingNotice />
                  ) : (
                    <ErrorNotice error={humanService(serviceProblem ?? "down", daemonError ?? "")} />
                  )}
                  {retryInSeconds ? (
                    <p className={styles.emptyStateDescription}>{t("errors.retrying", { seconds: retryInSeconds })}</p>
                  ) : null}
                  <div className={styles.optionRow}>
                    <Button variant={serviceProblem === "starting" ? "secondary" : "primary"} onClick={() => void restartDaemon(undefined, "owner-offline-screen")}>
                      {t("errors.restartService")}
                    </Button>
                    <Button variant={serviceProblem === "starting" ? "primary" : "ghost"} onClick={retry}>
                      {t("main.daemonRetry")}
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
                    {bots.length === 0 ? (
                      <>
                        <HomeFirstSteps
                          onConnectModel={() => handleOpenOnboarding(false)}
                          onNewBot={() => setNewBotOpen(true)}
                          onRefresh={() => void refreshBots()}
                        />
                        <Button
                          variant="secondary"
                          data-testid="home-start-from-goal"
                          onClick={() => setRecipeOpen(true)}
                        >
                          <Target />{t("recipes.entry")}
                        </Button>
                      </>
                    ) : (
                      <div className={styles.optionRow}>
                        <Button
                          variant="primary"
                          onClick={() => setNewBotOpen(true)}
                        >
                          <Plus />{t("sidebar.newBot")}
                        </Button>
                        <Button
                          variant="secondary"
                          data-testid="home-start-from-goal"
                          onClick={() => setRecipeOpen(true)}
                        >
                          <Target />{t("recipes.entry")}
                        </Button>
                      </div>
                    )}
                  </div>
              )}
            </Card>
          </div>
        ) : null}
        {route.kind === "bot" && !hostEmpty && !hostVersionBlocked ? (
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
            onStartFromGoal={() => setRecipeOpen(true)}
            onDemo={showDemoOffer ? (task) => setDemo({ task }) : undefined}
            extra={
              <GettingStarted
                variant="start"
                hidden={settings.gettingStartedHidden}
                onAction={(target) => openGettingStartedTarget(target, route.botId)}
                onHide={() => updateSettings({ gettingStartedHidden: true })}
                onShow={() => updateSettings({ gettingStartedHidden: false })}
              />
            }
          />
        ) : null}
        {route.kind === "chat" && !hostEmpty && !hostVersionBlocked ? (
          <ChatView
            key={route.chatId}
            chatId={route.chatId}
            external={route.external}
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
            onPopOut={(opts) => {
              const currentChat = chats.find((c) => c.id === route.chatId);
              void openChatPopout(route.chatId, {
                ...opts,
                computerId: activeComputerId,
                title: chatTitleLabel(currentChat?.title, t("bots.newChat")),
                windowFrame: settings.windowFrame
              }).catch(popoutFailed);
            }}
          />
        ) : null}
        {route.kind === "needs" ? <NeedsYouView onOpenChat={openChat} /> : null}
        {route.kind === "notifications" ? <NotificationHistoryView onOpenChat={openChat} onOpenNeeds={openNeeds} privateMode={settings.notifyPrivate} /> : null}
        {route.kind === "myBots" ? <MyBotsView
          control={controlState}
          newChatBusy={newChatBusy}
          onNewBot={() => setNewBotOpen(true)}
          onNewChat={(botId) => void startChat(botId)}
          onOpenChat={openChat}
          onSettings={(botId) => go({ kind: "bot", botId, edit: true })}
          onSchedule={(botId) => go({ kind: "schedules", botId })}
          onWorkflows={(botId) => go({ kind: "workflows", botId })}
          onPause={async (botId) => {
            // Re-read before a global native action: a stale card must not pause another bot.
            const snapshot = await getControlState();
            const action = nativePauseAction(botId, activeComputerId === LOCAL_COMPUTER, snapshot);
            if (!action) throw new Error(t("myBots.pauseUnavailable"));
            setControlState(await (action === "pause" ? pauseBotRun() : resumeBotRun()));
          }}
        /> : null}
        {route.kind === "workflows" ? <WorkflowsView initialBotId={route.botId} /> : null}
        {route.kind === "schedules" ? (
          <SchedulesView onOpenChat={openChat} initialBotId={route.botId} />
        ) : null}
      </div>
      <PaletteHost
        currentBotId={routeBotId}
        onScreen={handlePaletteScreen}
        onBot={goToBot}
        onChat={(chatId, botId) => go({ kind: "chat", chatId, botId })}
        onSettingsPage={(page) => setSettingsOpen(page)}
        onHelp={() => setHelpOpen(true)}
        onSearch={() => setSearchOpen(true)}
      />
      {searchOpen ? <MessageSearch seed={searchSeed} onClose={() => setSearchOpen(false)} onOpenChat={openChat} onOpenBot={goToBot} /> : null}
      <SettingsModal
        open={settingsOpen !== null}
        initialSection={settingsOpen ?? "general"}
        initialTarget={settingsTarget}
        onClose={() => {
          setSettingsOpen(null);
          setSettingsTarget(null);
        }}
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
      <AppsModal open={appsOpen} start={appsStart} onClose={() => { setAppsOpen(false); setAppsStart(null); }} />
      <VaultModal
        open={vaultOpen}
        onClose={() => setVaultOpen(false)}
        onManageEmail={() => setSettingsOpen("mail")}
      />
      <RecipeFlow
        open={recipeOpen}
        recheckKey={recipeRecheck}
        onFix={openRecipeFix}
        onClose={() => setRecipeOpen(false)}
        onDone={(botId, chatId) => {
          setRecipeOpen(false);
          go({ kind: "chat", chatId, botId });
        }}
      />
      <NewBotModal
        open={newBotOpen}
        onStartFromGoal={() => {
          setNewBotOpen(false);
          setRecipeOpen(true);
        }}
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
        initialSection={editSection}
        onClose={() => {
          setBotEditOpen(false);
          setEditSection(review.section);
        }}
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
        onBrowse={() => setMarketOpen(true)}
      />
      <SkillMarketModal
        open={marketOpen && botInView}
        client={client}
        onClose={() => setMarketOpen(false)}
        onChanged={() => void botSkills.reload()}
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
      <RunTimelineModal
        run={linkedRun ? events.runs[linkedRun.id] ?? null : null}
        fallback={linkedRun ? runs.find((run) => run.id === linkedRun.id) ?? linkedRun : undefined}
        botName={linkedRun ? bots.find((bot) => bot.id === linkedRun.bot_id)?.name ?? "" : ""}
        onClose={() => setLinkedRun(null)}
      />
      <HelpModal
        open={helpOpen}
        onClose={() => setHelpOpen(false)}
        stopHotkey={controlState.stopHotkey || settings.stopHotkey}
        sendWith={settings.sendWith}
        onSetup={() => {
          setHelpOpen(false);
          handleOpenOnboarding(false);
        }}
        onDemo={() => {
          setHelpOpen(false);
          setDemo({ task: null });
        }}
        gettingStarted={
          <GettingStarted
            variant="help"
            hidden={settings.gettingStartedHidden}
            onAction={(target) => {
              setHelpOpen(false);
              openGettingStartedTarget(target, selectedBot?.id ?? bots[0]?.id);
            }}
            onHide={() => updateSettings({ gettingStartedHidden: true })}
            onShow={() => updateSettings({ gettingStartedHidden: false })}
          />
        }
      />
      <DemoModal
        open={demo !== null}
        initialTask={demo?.task ?? null}
        onClose={() => setDemo(null)}
        onConnect={() => {
          setDemo(null);
          if (!onboardingBlocked && !settings.onboardingCompleted) {
            handleOpenOnboarding(false);
          } else {
            setSettingsOpen("ai");
          }
        }}
      />
      <LiveViewer
        botId={selectedBot?.id ?? null}
        infoPanel={liveInfoPanel}
        onChatSlots={route.kind === "chat" && !hostEmpty ? setHomeChatSlots : undefined}
        onPopOut={(opts) =>
          void openLivePopout({
            title: t("liveView.title"),
            liveSource: "home",
            botId: selectedBot?.id,
            targetId: opts.targetId,
            nearCursor: opts.nearCursor,
            computerId: activeComputerId,
            windowFrame: settings.windowFrame
          }).catch(popoutFailed)
        }
      />
      {activeComputerId !== LOCAL_COMPUTER ? (
        <LiveViewer
          source="local"
          botId={selectedBot?.id ?? null}
          infoPanel={liveInfoPanel}
          onChatSlots={route.kind === "chat" && !hostEmpty ? setLocalChatSlots : undefined}
          onPopOut={(opts) =>
            void openLivePopout({
            title: t("liveView.title"),
              liveSource: "local",
              botId: selectedBot?.id,
              targetId: opts.targetId,
              nearCursor: opts.nearCursor,
              computerId: LOCAL_COMPUTER,
              windowFrame: settings.windowFrame
            }).catch(popoutFailed)
          }
        />
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
  const [deepLinks] = useState(createDeepLinkDispatcher);
  useEffect(() => onDeepLinkPending(deepLinks), [deepLinks]);
  return (
    <ToastProvider>
      <DaemonProvider>
        <ServiceStateProvider>
        <ServerTerminalProvider>
          <ComputerRequestModal />
          <ComputerScope>
            <LeaveGuardProvider>
              <EngineConsentProvider>
                <AppShell deepLinks={deepLinks} />
              </EngineConsentProvider>
            </LeaveGuardProvider>
          </ComputerScope>
        </ServerTerminalProvider>
        </ServiceStateProvider>
      </DaemonProvider>
    </ToastProvider>
  );
}
