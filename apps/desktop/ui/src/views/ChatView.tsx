import {
  ArrowDown,
  ArrowLeft,
  ArrowUp,
  Bookmark,
  ExternalLink,
  Plus,
  Square,
  CalendarClock,
  Rocket,
  CircleHelp,
  Download,
  LayoutGrid,
  ListOrdered,
  Settings2,
  Brain,
  Search,
  Zap
} from "lucide-react";
import { BotLimitCard } from "../components/BotLimitCard";
import { needsEngineCheck } from "../lib/engineConsent";
import { parseLimitReached } from "../lib/botLimits";
import {
  type CSSProperties,
  type KeyboardEvent,
  type ReactNode,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore
} from "react";
import { useTranslation } from "react-i18next";
import { createPortal } from "react-dom";
import type { LiveChatSlots } from "./LiveViewerChat";
import { ownerKey } from "../lib/ownerKey";
import { chatActivityMs } from "../lib/unread";
import { useChatRead } from "../state/useChatRead";
import { useLeaveGuard } from "../state/LeaveGuard";
import { NewMessagesDivider } from "../components/NewMessagesDivider";
import { BotAvatar, Button } from "ui";
import { isMockMode } from "../api/mockMode";
import { ApiError } from "../api/client";
import type { Bot, Message, Mission } from "../api/types";
import {
  ApprovalCard,
  NoticeLine,
  QuestionCard,
  SubagentCard,
  TodoCard,
  ToolCard
} from "../components/ChatCards";
import { ApprovalBatchCard } from "../components/ApprovalBatchCard";
import { ApprovalDock, type DockPage } from "../components/ApprovalDock";
import { runWasRedirected } from "../lib/approvalSkip";
import { PendingDecisions, useDecisionActions } from "../components/PendingDecisions";
import { ElicitationCard } from "../components/ElicitationCard";
import { HandoverCard } from "../components/HandoverCard";
import { useDragPopOut } from "../components/useDragPopOut";
import { SecretRequestCard } from "../components/SecretRequestCard";
import { PastedSecretWarning } from "../components/PastedSecretWarning";
import { detectSecrets, kindsToWarn, planStore, removeSecrets } from "../lib/secretDetect";
import { autoChatTitle, chatTitleLabel } from "../lib/chatTitle";
import { pendingForChat } from "../lib/pendingDecisions";
import { ChatFindBar } from "../components/ChatFindBar";
import { useChatContextMenu } from "../components/useChatContextMenu";
import { askDraft, draftWithQuote, quoteLines } from "../lib/contextMenu";
import { Markdown } from "../components/Markdown";
import { runEndAnnouncement } from "../lib/replyAnnounce";
import { FoldedBody } from "../components/UserBubble";
import { UserMessageRow, AssistantMessageRow } from "../components/ChatMessageRow";
import { MessageTime } from "../components/MessageTime";
import { ModelPicker } from "../components/ModelPicker";
import { ComposerPlusMenu } from "../components/ComposerPlusMenu";
import { DictateButton } from "../components/DictateButton";
import { useVoiceCall } from "../components/voiceCall/useVoiceCall";
import { VoiceCallPanel } from "../components/voiceCall/VoiceCallPanel";
import { useLiveVoice } from "../components/liveVoice/useLiveVoice";
import { LiveVoicePanel, TalkButton } from "../components/liveVoice/LiveVoicePanel";
import { MessageInfo } from "../components/MessageInfo";
import { MessageActions } from "../components/MessageActions";
import { composerPlaceholderKey, shouldFocusOnBoxClick, shouldTypeToFocus } from "../lib/composerFocus";
import { RegenerateAction, RegenerateRow, SupersededView } from "../components/ResendTurn";
import { IconAction, RunActionBar } from "../components/RunActionBar";
import { CopyButton } from "../components/CopyButton";
import { useAutoSpeak } from "../components/useAutoSpeak";
import { SpeakButton } from "../components/SpeakButton";
import { QueuedMessages } from "../components/QueuedMessages";
import { RunStatusLine } from "../components/RunStatusLine";
import { RunChangesGate } from "../components/RunChangesGate";
import { RunTimelineModal } from "../components/RunTimelineModal";
import { ChatStepGroup } from "../components/ChatStepGroup";
import { WorkflowReview } from "../components/WorkflowReview";
import { useToast } from "../components/Toast";
import { useEngineConsent } from "../state/EngineConsent";
import { OPEN_AI_SETTINGS_EVENT } from "../state/useCliEngineRows";
import {
  botView,
  configFromDraft,
  draftFromBot,
  parseModelRef
} from "../lib/botProfile";
import {
  nextFollow,
  shouldProbeChanges,
  showJumpToLatest,
  showRouteChip
} from "../lib/chatLogic";
import { lastTurn, offersRegenerate, supersededGroups, supersededRunIds, type SupersededGroup } from "../lib/resendTurn";
import { NoPreviousModelError, RollbackFailedError, botWriteMessage, switchExecutor } from "../lib/botWrite";
import { splitMentions } from "../lib/mentionText";
import { saveChatExport } from "../lib/chatHistory";
import { dayHeader, type MessageTimes } from "../lib/messageTime";
import { hasConnectedModel, modelGroups } from "../lib/modelPicker";
import { openSettingsTarget } from "../lib/settingsLink";
import { type ChatRunPhase, runBarModel, runStopUi, STOP_CONFIRM_MS } from "../lib/runBar";
import { activeRunAfterCancel, composerButton, composerKeys, isMacPlatform } from "../lib/composerRun";
import { ATTACHMENT_ACCEPT, filesOf } from "../lib/attachments";
import { AttachmentDraft } from "../lib/attachmentDraft";
import { ComposerFiles } from "../components/Attachments";
import { canAutoDeliver, enqueue, queuesLocally, removeQueued, takeForRunEnd, type QueuedMessage } from "../lib/messageQueue";
import { createQueueDelivery } from "../lib/queueDelivery";
import { groupPending, shouldBatch } from "../lib/approvalBatch";
import { approvalHeadline, type T as PlainT } from "../lib/approvalPlain";
import { useChatQueue } from "../state/useChatQueue";
import {
  giveBackControl,
  type ControlSnapshot,
  pauseBotRun,
  resumeBotRun,
  takeTheWheel
} from "../overlay/events";
import { describeTool, stepPhase } from "../lib/toolLabels";
import { brandIcon } from "../lib/brandIcons";
import { type Chain, chainOrderTs, groupRunItems } from "../lib/thinkingChain";
import { type SendWith, shouldSend } from "../lib/settingsPrefs";
import { isProcessingRunState } from "../lib/runState";
import { commandOptions, mentionOptions, optionGroups } from "../lib/composerMenu";
import {
  applyCompletion,
  detectTrigger,
  APP_COMMAND_EVENT,
  COMPOSE_EVENT,
  isExportCommand,
  appCommandFor,
  missionCommandGoal,
  parseSlashInput,
  unknownSlashCommand
} from "../lib/slash";
import { appendDictation } from "../lib/dictation";
import { FRESH, type HistoryNav, afterEdit, historyKey, userMessageTexts } from "../lib/composerHistory";
import {
  newChatIdFromText,
  routeChip,
  routeHoverValues
} from "../lib/route";
import { useDaemon } from "../state/DaemonContext";
import { appLedger, cancelRun, requestStop, useLedgerVersion } from "../state/appStop";
import { useProviders } from "../state/useProviders";
import { useConnectivity } from "../state/useConnectivity";
import { canAnswer } from "../lib/connectivity";
import { queueHasRoom } from "../lib/messageQueue";
import {
  assignChildRuns,
  itemTs,
  type RunState,
  runText,
  splitTextAtMessages,
  type TimelineItem,
  topLevelRuns
} from "../state/timeline";
import ccStyles from "../components/ChatCards.module.css";
import styles from "./ChatView.module.css";
import { type ErrorAction, type HumanError, humanError, humanLine, humanToolError } from "../lib/humanError";
import type { ApiClient } from "../api/client";
import { sendWithHostRecovery } from "../lib/hostRecovery";
import { BotFilesCard } from "../components/BotFilesCard";
import { ComputerUseOffCard } from "../components/ComputerAccess";
import { ComputerTargetChip, computerTargetOf } from "../components/ComputerTargetChip";
import { ErrorNotice } from "../components/ErrorNotice";
import { missionErrorText } from "../components/MissionBlock";
import { MissionChatHeader, MissionDigestMessage, MissionStartedCard, useChatMission } from "../components/MissionChatHeader";
import { chatMissionIds, rememberChatMission } from "../lib/chatMissions";
import { parseMissionDigest } from "../lib/missionDigestText";
import { isActiveMission, missionsStartedIn } from "../lib/missions";
import { itemsWithoutDuplicateError } from "../lib/runErrorNotice";

/** Empty-chat prompt starters (plain text for the composer, not commands). */
const STARTERS = [
  "research",
  "summarize",
  "plan",
  "email",
  "calendar",
  "code",
  "remind"
] as const;

const SLASH_ICONS: Record<string, typeof Plus> = {
  new: Plus, stop: Square, export: Download, help: CircleHelp,
  settings: Settings2, models: Brain, apps: LayoutGrid, schedules: CalendarClock, mission: Rocket
};

/** A specific, known failure (the same one in two wordings); the catch-all codes say nothing. */
const failureCode = (message: string): string | null => {
  const code = humanToolError(message).code;
  return code === "toolFailed" || code === "generic" || code === "http_error" ? null : code;
};

// Immutable run snapshots retain their grouped rows across unrelated runs' deltas.
// Weak keys release cached Markdown/step data when a chat is left.
const runSegments = new WeakMap<RunState, ReturnType<typeof groupRunItems>>();
function segmentsFor(run: RunState) {
  let segments = runSegments.get(run);
  if (!segments) {
    // A failed run's error is drawn once, as the card under the run (not also as a notice line above it).
    segments = groupRunItems(itemsWithoutDuplicateError(run, failureCode), run.finishedMs !== undefined, run.startedMs);
    runSegments.set(run, segments);
  }
  return segments;
}

type Entry =
  | { kind: "day"; key: string; ts: number }
  | { kind: "unread"; key: string; ts: number }
  | { kind: "user"; key: string; ts: number; msgId: string; text: string; source?: string | null; attachments?: Message["attachments"] }
  | { kind: "superseded"; key: string; ts: number; group: SupersededGroup }
  | { kind: "assistant"; key: string; ts: number; text: string; live: boolean; botId?: string | null; runId?: string | null; source?: string | null }
  | { kind: "imported_note"; key: string; ts: number; text: string }
  | { kind: "item"; key: string; ts: number; item: TimelineItem; runId: string }
  | { kind: "chain"; key: string; ts: number; chain: Chain; runId: string }
  | { kind: "runError"; key: string; ts: number; message: string }
  | { kind: "runStopped"; key: string; ts: number }
  | { kind: "route"; key: string; ts: number; run: RunState }
  | { kind: "changes"; key: string; ts: number; run: RunState }
  | { kind: "timeline"; key: string; ts: number; run: RunState; remember: boolean; workflow: boolean }
  | { kind: "mission"; key: string; ts: number; mission: Mission };

/** `idle` = offer the button; `saved` = done; `hidden` = the daemon said no (409). */
type RememberState = "idle" | "busy" | "saved" | "hidden";

/** A finished, successful turn that did not just run a command (`/new`). */
/** Icon of a slash command in the menu. */
function SlashIcon({ name }: { name: string }) {
  const Icon = SLASH_ICONS[name] ?? Search;
  return <Icon />;
}

/** Counts from the daemon's fixed import note, `null` for any other text. */
function importedNoteCounts(text: string): { steps: string; approvals: string } | null {
  const match = /^Imported chat note: (\d+) tool step\(s\) and (\d+) approval\(s\) were listed in the file but were not imported or executed\.$/.exec(text);
  return match ? { steps: match[1], approvals: match[2] } : null;
}

function canOfferRemember(run: RunState): boolean {
  return (
    run.finishedMs !== undefined &&
    run.error === null &&
    run.route?.route.kind !== "command"
  );
}


export function ChatView({
  chatId,
  external = false,
  jumpMessageId,
  jumpRequestId,
  onBack,
  control,
  onControl,
  onSwitchChat,
  showAgentRoute = false,
  sendWith = "enter",
  messageTimes = "minutes",
  liveChatSlots,
  onPopOut,
  onNativeStop,
  hideHeader = false
}: {
  chatId: string;
  external?: boolean;
  jumpMessageId?: string;
  jumpRequestId?: number;
  /** Present only when there is a page to go back to; without it the header has no arrow. */
  onBack?: () => void;
  /** The native (desktop control) state; the run bar shows it next to the chat run. */
  control: ControlSnapshot;
  onControl: (snapshot: ControlSnapshot) => void;
  /** Open another chat (after a `/new` turn created one). */
  onSwitchChat?: (chatId: string) => void;
  /** Debug: also show the chip for a plain `Agent` decision at K4. */
  showAgentRoute?: boolean;
  /** Key chord that sends the message (Settings, General). */
  sendWith?: SendWith;
  /** Settings, General: per-message times off / HH:mm / HH:mm:ss. */
  messageTimes?: MessageTimes;
  liveChatSlots?: LiveChatSlots | null;
  /** Open this chat in a dedicated pop-out window (supports button click or header drag-out). */
  onPopOut?: (opts?: { nearCursor?: boolean }) => void;
  /** Custom native stop handler (used by pop-out windows). */
  onNativeStop?: () => Promise<void> | void;
  /** Hide the inner ChatView header bar (used when a pop-out shell renders its own header). */
  hideHeader?: boolean;
}) {
  const { t, i18n } = useTranslation();
  const toast = useToast();
  const {
    client,
    chats,
    bots,
    events,
    runs,
    needs,
    refreshRuns,
    refreshChats,
    refreshBots,
    restartDaemon,
    ensureHostOnline,
    activeComputerId,
    hostStarting
  } = useDaemon();
  const chat = chats.find((c) => c.id === chatId);
  const bot = bots.find((b) => b.id === chat?.bot_id);
  const view = bot ? botView(bot) : null;
  const { dto: providersDto } = useProviders();
  const { report: connectionReport, state: connectionState } = useConnectivity();
  const modelChain = bot ? [botView(bot).executor, ...draftFromBot(bot).fallback.split(",").map((s) => s.trim()).filter(Boolean)] : [];
  const offline = connectionState === "host_unreachable" || !canAnswer(connectionReport, modelChain);
  const { askEngine, applyEngine, fixBotEngine } = useEngineConsent();
  // Only the internal validation-only provider exists: nothing can answer yet. Sending shows a
  // friendly "connect a model" card instead of a failed run.
  const noModelYet = providersDto !== null && !hasConnectedModel(providersDto);
  const [noModelCard, setNoModelCard] = useState(false);
  const pickerGroups = useMemo(
    () => modelGroups(providersDto, view?.executor),
    [providersDto, view?.executor]
  );

  // Switch the bot's executor model from the header (a profile update).
  const pickModel = async (ref: string) => {
    if (!client || !bot) {
      return;
    }
    try {
      // The bot as the daemon has it now: the consent and the rollback both start from it, never from a stale list entry.
      const fresh = await client.getBot(bot.id);
      // A CLI that only works with its own tools: the one consent step, here, before saving.
      const answer = await askEngine(ref, fresh.config);
      if (answer === null) {
        return;
      }
      const setExecutor = (executor: string) => (current: Bot) => {
        const draft = draftFromBot(current);
        draft.executor = parseModelRef(executor);
        return { name: current.name, config: configFromDraft(draft, current) };
      };
      // If the engine step fails, the bot goes back to the model it had: never left on one that cannot answer.
      await switchExecutor({ client, fresh, ref, setExecutor, apply: (saved) => applyEngine(saved.id, answer) });
      await refreshBots();
      toast.show(t("modelPicker.switched", { model: ref }), "success");
    } catch (e) {
      if (e instanceof NoPreviousModelError) {
        // Nothing was changed: without a current model a failed switch could not be put back.
        toast.show(t("engineConsent.notice.rollbackFailed", { model: ref, reason: t("engineConsent.notice.noPreviousModel") }), "danger", { durationMs: 15000 });
        return;
      }
      if (e instanceof RollbackFailedError) {
        // The bot is still on the model it could not use: say so, and show what is really saved.
        await refreshBots().catch(() => {});
        toast.show(t("engineConsent.notice.rollbackFailed", { model: ref, reason: botWriteMessage(e.reason, t) }), "danger", { durationMs: 15000 });
        return;
      }
      toast.show(botWriteMessage(e, t), "danger");
    }
  };

  // A failed turn: only the actions this view can carry out get a button.
  const runErrorFor = (message: string, latest: boolean): HumanError => {
    const h = humanToolError(message);
    if (h.action !== "fixEngine" && h.action !== "settings") return { ...h, action: undefined };
    // An older failed turn: its hint points at a button it no longer has, so only the title and details stay.
    return latest ? h : { ...h, action: undefined, hint: "" };
  };
  const onRunErrorAction = (action: ErrorAction) => {
    if (action === "fixEngine" && bot) {
      void fixBotEngine(bot).then((ok) => ok && toast.show(t("engineConsent.fixed"), "success"));
    } else if (action === "settings") {
      window.dispatchEvent(new CustomEvent(OPEN_AI_SETTINGS_EVENT));
    }
  };

  const [messages, setMessages] = useState<Message[]>([]);
  const [text, setText] = useState("");
  // The newest draft, for async work that must not act on a stale closure (kept in memory only).
  const draftRef = useRef("");
  draftRef.current = text;
  // A pasted credential found in the draft before send (kinds only; the value stays in `text`).
  const [secretKinds, setSecretKinds] = useState<string[] | null>(null);
  const [queue, updateQueue] = useChatQueue(`${activeComputerId}:${chatId}`);
  const offlineSending = useRef(false);
  const queueSending = useRef(false);
  const delivery = useMemo(() => createQueueDelivery(updateQueue), [updateQueue]);
  const queuedRun = useRef<string | null>(null);
  /** Messages queued in this view. A chat opened from a link delivers only these at run end. */
  const queuedHere = useRef(new Set<string>());
  // Up / Down recall this chat's earlier messages; the draft is kept (lib/composerHistory).
  const [nav, setNav] = useState<HistoryNav>(FRESH);
  const pendingCaret = useRef<number | null>(null);
  const [sending, setSending] = useState(false);
  /** The last user message being edited (inline), and a resend in flight. */
  const [editingId, setEditingId] = useState<string | null>(null);
  const [resending, setResending] = useState(false);
  const [attachments] = useState(() => new AttachmentDraft());
  const files = useSyncExternalStore(attachments.subscribe, attachments.snapshot);
  useLeaveGuard({
    state: () => ({
      dirty: text.length > 0 || files.length > 0 || editingId !== null,
      busy: sending || resending
    }),
    confirm: () => {}
  }, true);
  const [dragOver, setDragOver] = useState(false);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  /** The run the daemon named for the message just sent, until the run list shows it. */
  const [ackedRunId, setAckedRunId] = useState<string | null>(null);
  /** Stop was pressed while the send was still unanswered: it is applied when the run is named. */
  const [stopPending, setStopPending] = useState(false);
  /** Stop was pressed: the round button shows Stopping until the run is no longer active. */
  const [stopPressed, setStopPressed] = useState(false);
  const stopWhileSending = useRef(false);
  const activeRunIdRef = useRef<string | null>(null);
  const [caret, setCaret] = useState(0);
  const [menuIdx, setMenuIdx] = useState(0);
  // Session-local recency; message content and cross-host bot ids are never persisted.
  const [recentCommands, setRecentCommands] = useState<string[]>([]);
  const [menuDismissed, setMenuDismissed] = useState<string | null>(null);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);
  const [inputScroll, setInputScroll] = useState(0);
  // One colour source for mentions: the bot's own colour, for typed text and for sent messages alike.
  const sentTexts = useMemo(() => userMessageTexts(messages), [messages]);
  const mentionBots = useMemo(() => bots.map((b) => ({ name: b.name, color: botView(b).color })), [bots]);
  const mentionParts = useMemo(() => splitMentions(text, mentionBots), [text, mentionBots]);
  const paintsMentions = mentionParts.some((p) => p.color);
  const [error, setError] = useState<string | null>(null);
  /** A message that did not go out: shown with Retry; the text stays in the composer. */
  const [sendFailure, setSendFailure] = useState<HumanError | null>(() =>
    // Design review (mock mode only): `?screen=errors-send` opens with a failed send.
    isMockMode() && new URLSearchParams(window.location.search).get("screen") === "errors-send"
      ? humanError({ status: 0, code: "network", message: "Failed to fetch" })
      : null
  );
  const scrollerRef = useRef<HTMLDivElement | null>(null);
  const [findOpen, setFindOpen] = useState(false);
  /** Text the find bar starts with (a selection sent from the right-click menu); `n` re-seeds an open bar. */
  const [findSeed, setFindSeed] = useState({ q: "", n: 0 });
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "f") {
        e.preventDefault();
        setFindOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  const stickToBottom = useRef(true);
  const [remember, setRemember] = useState<Record<string, RememberState>>({});
  const [timelineRunId, setTimelineRunId] = useState<string | null>(null);
  // The chat moved into (or out of) the live view's card: the DOM is new, so land on the newest message;
  // inside the card the cursor goes to the composer too.
  const liveRoot = liveChatSlots?.root ?? null;
  useLayoutEffect(() => {
    const scroller = scrollerRef.current;
    if (scroller) {
      stickToBottom.current = true;
      scroller.scrollTop = scroller.scrollHeight;
    }
    if (liveRoot) inputRef.current?.focus({ preventScroll: true });
  }, [liveRoot]);

  // Type anywhere in the chat: the first character goes to the composer (nothing else owns the keys).
  useEffect(() => {
    const onType = (e: globalThis.KeyboardEvent) => {
      const el = inputRef.current;
      const sel = window.getSelection();
      const ok = shouldTypeToFocus(e, {
        composerReady: !!el && el.isConnected && !el.disabled && !el.readOnly,
        dialogOpen: document.querySelector("[role='dialog'], [aria-modal='true'], dialog[open]") !== null,
        selectionActive: !!sel && !sel.isCollapsed && sel.toString().length > 0
      });
      if (ok && el && document.activeElement !== el) {
        el.focus({ preventScroll: true });
      }
    };
    window.addEventListener("keydown", onType);
    return () => window.removeEventListener("keydown", onType);
  }, []);
  useEffect(() => {
    const el = inputRef.current;
    if (el && pendingCaret.current !== null) {
      el.setSelectionRange(pendingCaret.current, pendingCaret.current);
      pendingCaret.current = null;
    }
  }, [text]);
  useEffect(() => {
    const el = inputRef.current;
    if (el) {
      el.style.height = "auto";
      el.style.height = `${Math.min(el.scrollHeight, parseFloat(getComputedStyle(el).maxHeight))}px`;
    }
  }, [text]);
  const followNewSince = useRef<number | null>(null);
  useLayoutEffect(() => {
    followNewSince.current = external ? null : Date.now();
    return () => { followNewSince.current = null; };
  }, [external]);
  const switchedFor = useRef(new Set<string>());

  const chatRuns = useMemo(
    () => topLevelRuns(events, chatId),
    [events, chatId]
  );
  useAutoSpeak(chatId, chatRuns, external);

  // A mission's chat: a compact progress header on top and its reports as cards (ux3-missions).
  // Any other chat: a card for each mission started from it, by `/mission` or by the bot itself.
  const [linkedMissions, setLinkedMissions] = useState<string[]>(() => chatMissionIds(chatId));
  useEffect(() => setLinkedMissions(chatMissionIds(chatId)), [chatId]);
  const ownMessages = useMemo(
    () => messages.filter((m) => m.role === "user" && !m.superseded).map((m) => ({ text: m.content, ts: m.created_at_ms })),
    [messages]
  );
  const chatMission = useChatMission(chat?.bot_id, chatId, true, (all) =>
    missionsStartedIn(all, chatId, ownMessages, linkedMissions).some(isActiveMission)
  );
  const startedMissions = useMemo(
    () => missionsStartedIn(chatMission.all, chatId, ownMessages, linkedMissions),
    [chatMission.all, chatId, ownMessages, linkedMissions]
  );
  // The bot started a mission in this chat (`mission_create`): read the missions again for its card.
  const missionCreates = chatRuns.reduce(
    (n, r) => n + r.items.filter((i) => i.kind === "tool" && i.name === "mission_create" && i.completedMs !== undefined).length,
    0
  );
  const refreshChatMission = chatMission.refresh;
  useEffect(() => {
    if (missionCreates > 0) void refreshChatMission();
  }, [missionCreates, refreshChatMission]);
  const finishedCount = chatRuns.filter(
    (r) => r.finishedMs !== undefined
  ).length;

  // `/new` typed in the chat: the daemon starts a new chat and says so in its
  // reply; follow it (only for turns that finish while this view is open).
  useEffect(() => {
    const since = followNewSince.current;
    if (since === null) return;
    for (const run of chatRuns) {
      const r = run.route?.route;
      if (
        r?.kind !== "command" ||
        r.name !== "new" ||
        run.finishedMs === undefined ||
        run.finishedMs < since ||
        run.error !== null ||
        switchedFor.current.has(run.id)
      ) {
        continue;
      }
      switchedFor.current.add(run.id);
      const id = newChatIdFromText(runText(run));
      if (id && onSwitchChat) {
        void refreshChats()
          .catch(() => {})
          .then(() => {
            if (followNewSince.current !== since) return;
            toast.show(t("chat.newChatStarted"), "success");
            onSwitchChat(id);
          });
      }
    }
  }, [chatRuns, external, onSwitchChat, refreshChats, toast, t]);

  // A chat that still carries the default name gets one from the owner's first message once the bot
  // has answered (local text, no model call, and never over a name a person or a bot chose).
  const autoTitled = useRef(new Set<string>());
  useEffect(() => {
    if (!client || external || !chat || autoTitled.current.has(chatId)) return;
    if (chatTitleLabel(chat.title, "") !== "") return;
    const firstUser = messages.find((m) => m.role === "user" && !m.superseded);
    const answered = messages.some((m) => m.role === "assistant" && !m.superseded) || chatRuns.some((r) => r.finishedMs !== undefined);
    const title = answered ? autoChatTitle(firstUser?.content) : null;
    if (!title) return;
    autoTitled.current.add(chatId);
    void client.updateChat(chatId, { title }).then(() => refreshChats()).catch(() => undefined);
  }, [client, external, chat, chatId, messages, chatRuns, refreshChats]);

  const loadMessages = useCallback(async () => {
    if (!client) {
      return;
    }
    try {
      setMessages(await client.listMessages(chatId));
    } catch (e) {
      setError(humanLine(e));
    }
  }, [client, chatId]);

  useEffect(() => {
    void loadMessages();
  }, [loadMessages, finishedCount]);

  // ---- the run to stop / resume
  const serverRuns = useMemo(() => runs.filter(
    (r) => r.chat_id === chatId && !r.parent_run_id
  ), [runs, chatId]);
  const serverLive = [...serverRuns].reverse().find((r) => isProcessingRunState(r.state));
  const serverPaused = [...serverRuns].reverse().find((r) => r.state === "paused");
  const eventLive: RunState | undefined = [...chatRuns]
    .reverse()
    .find(
      (r) =>
        r.finishedMs === undefined && !serverRuns.some((s) => s.id === r.id)
    );
  const ackedLive =
    ackedRunId !== null &&
    !serverRuns.some((r) => r.id === ackedRunId && !isProcessingRunState(r.state)) &&
    !chatRuns.some((r) => r.id === ackedRunId && r.finishedMs !== undefined)
      ? ackedRunId
      : null;
  const finishedByEvent = new Set(
    chatRuns.filter((r) => r.finishedMs !== undefined || r.stopped).map((r) => r.id)
  );
  const activeRunId = activeRunAfterCancel({
    listedLiveId: serverLive?.id ?? null,
    finishedByEvent,
    eventLiveId: eventLive?.id ?? null,
    ackedId: ackedLive
  });
  activeRunIdRef.current = activeRunId;
  const pausedRunId = activeRunId ? null : (serverPaused?.id ?? null);
  const active = activeRunId !== null;
  /** The run bar (with its Stop) is up from the moment a message is sent, before any event. */
  const running = active || sending;

  const { seenUntilMs, dismiss: dismissUnread } = useChatRead(
    client?.baseUrl ?? "",
    chatId,
    chatActivityMs(runs, needs, chatId),
    messages,
    external
  );

  const entries = useMemo(() => {
    const out: Entry[] = [];
    const known = new Set(chatRuns.map((r) => r.id));
    // Replaced / regenerated messages are folded into one "earlier version" row, and their runs'
    // live events no longer draw as the current answer.
    const replacedRuns = supersededRunIds(messages);
    for (const group of supersededGroups(messages)) {
      out.push({ kind: "superseded", key: group.key, ts: group.ts, group });
    }
    for (const m of messages) {
      if (m.superseded) {
        continue;
      }
      if (m.role === "user") {
        out.push({
          kind: "user",
          key: `m-${m.id}`,
          ts: m.created_at_ms,
          msgId: m.id,
          text: m.content,
          source: m.source,
          attachments: m.attachments
        });
      } else if (
        m.role === "assistant" &&
        (!m.run_id || !known.has(m.run_id) || m.id === jumpMessageId)
      ) {
        out.push({
          kind: "assistant",
          key: `m-${m.id}`,
          ts: m.created_at_ms,
          text: m.content,
          live: false,
          botId: m.bot_id,
          runId: m.run_id,
          source: m.source
        });
      } else if (m.role === "imported_note") {
        out.push({
          kind: "imported_note",
          key: `m-${m.id}`,
          ts: m.created_at_ms,
          text: m.content
        });
      }
    }
    const lastFinished = [...chatRuns]
      .reverse()
      .find((r) => r.finishedMs !== undefined);
    const userTimes = messages.filter((m) => m.role === "user" && !m.superseded).map((m) => m.created_at_ms);
    for (const sourceRun of chatRuns) {
      if (replacedRuns.has(sourceRun.id)) {
        continue;
      }
      // A message written while this run was answering starts a new bubble; it never grows the old one.
      const run = splitTextAtMessages(sourceRun, userTimes);
      const chip = run.route ? routeChip(run.route, showAgentRoute) : null;
      if (run.route && chip && showRouteChip(chip.cheap, showAgentRoute)) {
        out.push({
          kind: "route",
          key: `${run.id}-route`,
          ts: run.startedMs,
          run
        });
      }
      const segs = segmentsFor(run);
      segs.forEach((seg, si) => {
        const next = segs[si + 1];
        const nextTs = next
          ? next.kind === "chain"
            ? next.chain.startMs
            : itemTs(next.item)
          : undefined;
        if (seg.kind === "chain") {
          out.push({
            kind: "chain",
            key: `${run.id}-${seg.chain.key}`,
            ts: chainOrderTs(seg.chain, nextTs),
            chain: seg.chain,
            runId: run.id
          });
        } else {
          out.push({
            kind: "item",
            key: `${run.id}-${seg.item.key}`,
            ts: itemTs(seg.item),
            item: seg.item,
            runId: run.id
          });
        }
      });
      if (run.finishedMs !== undefined && run.stopped && !runWasRedirected(run.items)) {
        out.push({ kind: "runStopped", key: `${run.id}-stopped`, ts: run.finishedMs });
      }
      if (run.finishedMs !== undefined && run.error) {
        out.push({
          kind: "runError",
          key: `${run.id}-error`,
          ts: run.finishedMs,
          message: run.error
        });
      }
      if (shouldProbeChanges(run, sourceRun === lastFinished)) {
        out.push({
          kind: "changes",
          key: `${run.id}-changes`,
          ts: run.finishedMs ?? run.startedMs,
          run
        });
      }
      if (sourceRun === lastFinished && run.finishedMs !== undefined) {
        out.push({
          kind: "timeline",
          key: `${run.id}-timeline`,
          ts: run.finishedMs,
          run,
          remember: canOfferRemember(run) && remember[run.id] !== "hidden",
          workflow: serverRuns.some((s) => s.id === run.id && s.state === "succeeded" && !s.parent_run_id)
        });
      }
    }
    for (const mission of startedMissions) {
      out.push({ kind: "mission", key: `mission-${mission.id}`, ts: mission.started_at_ms, mission });
    }
    // Array.prototype.sort is stable: equal timestamps keep insertion order.
    const sorted = out.sort((a, b) => a.ts - b.ts);
    const unreadAt = seenUntilMs === null ? undefined : sorted.find((e) => e.kind !== "user" && e.ts > seenUntilMs)?.key;
    return sorted.flatMap((entry, index): Entry[] => {
      const marker: Entry[] = entry.key === unreadAt ? [{ kind: "unread", key: "unread", ts: entry.ts }] : [];
      return index === 0 || new Date(entry.ts).toDateString() !== new Date(sorted[index - 1].ts).toDateString()
        ? [{ kind: "day", key: `day-${entry.key}`, ts: entry.ts }, ...marker, entry] : [...marker, entry];
    });
  }, [messages, chatRuns, showAgentRoute, remember, seenUntilMs, serverRuns, jumpMessageId, startedMissions]);

  const lastActionsKey = useMemo(() => {
    for (let i = entries.length - 1; i >= 0; i -= 1) {
      if (entries[i].kind === "timeline") return entries[i].key;
    }
    return null;
  }, [entries]);
  // Only the newest run's error offers its fix; an older failed turn keeps the text but no button
  // (the fix may be done already, and a second "Fix" under an old card reads as an open problem).
  const lastRunId = useMemo(() => {
    for (let i = entries.length - 1; i >= 0; i -= 1) {
      const entry = entries[i];
      if (entry.kind === "timeline") return entry.run.id;
    }
    return null;
  }, [entries]);
  const turn = useMemo(() => lastTurn(messages), [messages]);
  // The search jump swaps one run answer for its stored message row (see the entries above).
  const replacedByJump = (runId: string, text: string) =>
    messages.some((m) => m.id === jumpMessageId && m.run_id === runId && m.content === text);
  // The run's actions (steps, remember, retry) sit in the action row of its newest answer, so icons and
  // time share one line; a run without an answer keeps the standalone bar.
  const timelineAt = entries.findIndex((e) => e.key === lastActionsKey);
  const timeline = entries[timelineAt];
  const actionsHost = (() => {
    if (timeline?.kind !== "timeline") return null;
    for (let i = timelineAt - 1; i >= 0; i -= 1) {
      const e = entries[i];
      if (e.kind === "user") return null;
      if (e.kind === "assistant" && e.runId === timeline.run.id) return e.key;
      if (e.kind === "item" && e.item.kind === "text" && e.runId === timeline.run.id
        && events.runs[e.runId]?.parentRunId === undefined && !replacedByJump(e.runId, e.item.text)) return e.key;
    }
    return null;
  })();
  const lastItem = entries[entries.length - 1];
  const blockedFiles = attachments.blocked;
  // ---- follow new content unless the user scrolled up
  const timelineRef = useRef<HTMLDivElement | null>(null);
  const lastTop = useRef(0);
  const [jumpVisible, setJumpVisible] = useState(false);
  const scrollToEnd = useCallback((smooth = false) => {
    const el = scrollerRef.current;
    if (el) {
      stickToBottom.current = true;
      if (smooth && !window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
        el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
      } else {
        el.scrollTop = el.scrollHeight;
      }
    }
  }, []);
  // Content grows (streaming text, a chain opening, an image): follow before paint.
  useLayoutEffect(() => {
    const content = timelineRef.current;
    if (!content || typeof ResizeObserver === "undefined") {
      return undefined;
    }
    const ro = new ResizeObserver(() => {
      if (stickToBottom.current) {
        const el = scrollerRef.current;
        if (el) {
          el.scrollTop = el.scrollHeight;
          lastTop.current = el.scrollTop;
        }
      }
    });
    ro.observe(content);
    // The viewport itself changes height when the run bar opens or closes.
    if (scrollerRef.current) {
      ro.observe(scrollerRef.current);
    }
    return () => ro.disconnect();
  }, [chat?.id, liveRoot]);
  useLayoutEffect(() => {
    if (stickToBottom.current) {
      const el = scrollerRef.current;
      if (el) {
        el.scrollTop = el.scrollHeight;
        lastTop.current = el.scrollTop;
      }
    }
  }, [entries.length]);

  const appliedJump = useRef<string | null>(null);
  useEffect(() => {
    const request = `${jumpMessageId}:${jumpRequestId ?? 0}`;
    if (!jumpMessageId || appliedJump.current === request || !messages.some((m) => m.id === jumpMessageId)) return;
    stickToBottom.current = false;
    const frame = requestAnimationFrame(() => {
      const row = document.getElementById(`m-${jumpMessageId}`);
      if (row) {
        appliedJump.current = request;
        row.scrollIntoView({ block: "center" });
        row.focus({ preventScroll: true });
        // A short glow shows which message the search meant (CSS drops the motion when reduced).
        row.setAttribute("data-flash", "true");
        window.setTimeout(() => row.removeAttribute("data-flash"), 1600);
      }
    });
    // The flash timer is left running on purpose: it only removes the attribute from that row.
    return () => cancelAnimationFrame(frame);
  }, [jumpMessageId, jumpRequestId, messages]);

  const onScroll = () => {
    const el = scrollerRef.current;
    if (!el) {
      return;
    }
    const next = nextFollow(stickToBottom.current, el, lastTop.current);
    lastTop.current = el.scrollTop;
    stickToBottom.current = next;
    const jump = showJumpToLatest(next, el);
    setJumpVisible((prev) => (prev === jump ? prev : jump));
  };

  // ---- "/" commands and "@" bot mentions in the composer
  const trigger = detectTrigger(text, caret);
  const triggerKey = trigger ? `${trigger.kind}:${trigger.start}` : null;
  const triggerKind = trigger?.kind ?? null;
  const triggerQuery = trigger?.query ?? "";
  // a dismissal holds for this exact trigger and query only
  const dismissKey = triggerKey ? `${triggerKey}|${triggerQuery}` : null;
  const menuItems = useMemo(() => {
    if (!triggerKind || dismissKey === menuDismissed) {
      return [];
    }
    const options = triggerKind === "slash"
      ? commandOptions(triggerQuery, recentCommands, t)
      : mentionOptions(bots, chats, triggerQuery, new Map(chats.map((c) => [c.id, chatActivityMs(runs, needs, c.id)])));
    return optionGroups(options).flatMap((group) => options.filter((o) => o.group === group));
  }, [triggerKind, triggerQuery, dismissKey, menuDismissed, bots, chats, runs, needs, recentCommands, t]);
  // once the trigger is gone a later one (even an identical one) starts fresh
  useEffect(() => {
    if (dismissKey === null) {
      setMenuDismissed(null);
    }
  }, [dismissKey]);
  const menuId = useId();
  const menuRef = useRef<HTMLDivElement | null>(null);
  const menuOpen = triggerKind !== null && dismissKey !== menuDismissed;
  const menuActive = Math.min(menuIdx, Math.max(menuItems.length - 1, 0));

  // keep the arrow-key selection visible in a long menu
  useEffect(() => {
    if (menuOpen) {
      document
        .getElementById(`${menuId}-${menuActive}`)
        ?.scrollIntoView({ block: "nearest" });
    }
  }, [menuOpen, menuActive, menuId]);

  const complete = (i: number) => {
    const item = menuItems[i];
    if (!item || !trigger) {
      return;
    }
    if (trigger.kind === "slash") setRecentCommands((prev) => [item.id, ...prev.filter((id) => id !== item.id)].slice(0, 5));
    const next = applyCompletion(text, trigger, item.insert);
    setText(next.text);
    setCaret(next.caret);
    setMenuIdx(0);
    requestAnimationFrame(() => {
      const el = inputRef.current;
      if (el) {
        el.focus();
        el.setSelectionRange(next.caret, next.caret);
      }
    });
  };

  // An empty-chat starter: put its words in the composer, caret at the end.
  const applyStarter = (words: string) => {
    const next = `${words} `;
    setText(next);
    setCaret(next.length);
    requestAnimationFrame(() => {
      const el = inputRef.current;
      if (el) {
        el.focus();
        el.setSelectionRange(next.length, next.length);
      }
    });
  };
  // "Start a mission" in the bot panel puts "/mission " here: the mission starts from the chat.
  const applyStarterRef = useRef(applyStarter);
  applyStarterRef.current = applyStarter;
  useEffect(() => {
    const onCompose = (e: Event) => applyStarterRef.current((e as CustomEvent<string>).detail);
    window.addEventListener(COMPOSE_EVENT, onCompose);
    return () => window.removeEventListener(COMPOSE_EVENT, onCompose);
  }, []);

  // Switching chats/hosts or closing the view cancels pending work and releases previews.
  useEffect(() => () => attachments.clear(), [attachments, chatId, client]);

  const removeFile = (key: string) => {
    attachments.remove(key);
    inputRef.current?.focus();
  };

  const addFiles = (incoming: File[]) => {
    if (!client || sending || attachments.locked) return;
    if (attachments.add(incoming, t("chat.attach.pastedName")) > 0) {
      toast.show(t("chat.attach.tooMany"), "info");
    }
  };

  /** `/export`: use the same bounded, redacted daemon export as the chat menu. */
  const exportChat = () => {
    if (!client) return;
    void saveChatExport(client, chatId, "markdown").then(
      () => toast.show(t("chat.export.done"), "success"),
      (e: unknown) => toast.show(humanLine(e), "danger")
    );
  };

  // `allowSecrets` is the owner's "Send anyway" for THIS call only: it is an argument, never state,
  // so a call that returns early (busy, offline) cannot leave a standing permission behind.
  const send = async (allowSecrets = false) => {
    const body = text.trim();
    const hasFiles = files.length > 0;
    if (!client || (!body && !hasFiles) || sending || attachments.locked || blockedFiles) {
      return;
    }
    if (hasFiles && body.startsWith("/")) {
      toast.show(t("chat.attach.command"), "info");
      return;
    }
    if (isExportCommand(body)) {
      setText("");
      exportChat();
      return;
    }
    // "/help", "/settings" ...: the app opens the place itself; nothing is sent to the bot.
    const appCommand = !hasFiles ? appCommandFor(body) : null;
    if (appCommand) {
      setText("");
      window.dispatchEvent(new CustomEvent(APP_COMMAND_EVENT, { detail: appCommand.target }));
      return;
    }
    // "/foobar": no such command. Say so here instead of paying for a model call that guesses.
    const unknownCommand = !hasFiles ? unknownSlashCommand(body) : null;
    if (unknownCommand) {
      toast.show(t("chat.slash.unknown", { name: unknownCommand }), "info");
      return;
    }
    if (noModelYet && !body.startsWith("/")) {
      setNoModelCard(true);
      return;
    }
    // A draft that looks like it holds a credential is not sent (or queued) until the owner says so.
    // Slash commands that reach the bot are stored in the history too, so they are checked as well.
    if (!allowSecrets) {
      const found = kindsToWarn(text, allowSecrets);
      if (found.length > 0) {
        setSecretKinds(found);
        return;
      }
    }
    if (offline && (hasFiles || (body.startsWith("/") && parseSlashInput(body)?.name !== "stop"))) {
      toast.show(t("offline.keepDraft"), "info");
      return;
    }
    // "/mission the job" (or "/görev"): the app starts the mission itself, as the owner, with no confirmation
    // (on a Full bot nothing asks; below Full the daemon says why it cannot). The card shows up here.
    const missionGoal = !hasFiles ? missionCommandGoal(body) : null;
    if (missionGoal !== null) {
      if (missionGoal === "") {
        toast.show(t("missionChat.needsGoal"), "info");
        return;
      }
      if (!chat) return;
      setSending(true);
      try {
        const started = await client.startMission({ bot_id: chat.bot_id, goal: missionGoal });
        rememberChatMission(chatId, started.mission.id);
        setLinkedMissions(chatMissionIds(chatId));
        setText("");
        setNav(FRESH);
        await chatMission.refresh();
        scrollToEnd();
      } catch (e) {
        toast.show(t("missionChat.startFailed", { reason: missionErrorText(e) }), "danger");
      } finally {
        setSending(false);
      }
      return;
    }
    // A message typed while the bot works joins the queue as a "Sirada." bubble (slash commands such
    // as /stop still go straight to the daemon). It is delivered when the run ends or by "Simdi gonder".
    // While the run waits on an approval the message is not queued behind it: it goes to the daemon,
    // which skips the approval (a denial) and starts the next turn with this message.
    // `waitLost`: the host says the run waits for the user but the card never reached this app (a paired
    // host's events can lag or miss). The message must still go to the daemon, never sit in a local queue
    // behind a run that will not move on its own. Offline, nothing can reach the daemon: queue instead.
    const approvalWaits = pending.own.some((e) => e.kind === "approval") || waitLost;
    if (queuesLocally({ runActive: active || offline, approvalWaits: approvalWaits && !offline, slashCommand: body.startsWith("/"), hasFiles })) {
      if (!queueHasRoom(queue, body)) {
        toast.show(t("offline.full"), "info");
        return;
      }
      const id = crypto.randomUUID();
      queuedHere.current.add(id);
      updateQueue((q) => enqueue(q, body, id, Date.now(), offline, appLedger().stopEpoch()));
      setText("");
      scrollToEnd();
      return;
    }
    // A chat opened from a link never follows earlier `/new` results; a `/new` sent here does.
    if (followNewSince.current === null && parseSlashInput(body)?.name === "new") {
      followNewSince.current = Date.now();
    }
    // A bot whose CLI model works with its own tools (picked in Settings, imported, or created by
    // another route) gets the one consent step now, never a failed first message.
    // Also a bot that already works with its own tools: its CLI can be missing or signed out.
    if (bot && !body.startsWith("/") && needsEngineCheck(bot.config)) {
      if (!(await fixBotEngine(bot, true))) {
        return;
      }
    }
    attachments.locked = true;
    const attachmentSignal = attachments.signal;
    setSending(true);
    setError(null);
    setSendFailure(null);
    try {
      // "@Other do x": the daemon lets Other answer in THIS chat.
      // Nothing answers: this computer's service is started and the message goes out once it
      // is up; for a paired server the draft stays and the user is told what to do.
      const outcome = await sendWithHostRecovery({
        send: async (fresh) => {
          const c = (fresh as ApiClient | null) ?? client;
          attachmentSignal.throwIfAborted();
          const attachmentIds = await attachments.uploadAll((blob, name, signal) => c.uploadAttachment(chatId, blob, name, signal));
          const key = await ownerKey(c.baseUrl);
          attachmentSignal.throwIfAborted();
          return c.sendMessage(chatId, body, undefined, key, attachmentIds);
        },
        ensure: ensureHostOnline
      });
      if (outcome.kind === "cancelled") {
        return; // client disposed (computer switched): nothing to report, the draft stays
      }
      if (outcome.kind === "remote_offline") {
        setSendFailure({
          code: "host_offline",
          title: t("hostRecovery.remoteOffline.title"),
          hint: t("hostRecovery.remoteOffline.hint"),
          details: humanError(outcome.error).details
        });
        return;
      }
      if (outcome.kind !== "sent") {
        throw outcome.error;
      }
      const res = outcome.value;
      if (res.run_id) {
        setAckedRunId(res.run_id);
      }
      if (stopWhileSending.current) {
        // Stop was pressed before the daemon named the run: stop it now. A queued message
        // has no run of its own; Stop then ends the turn it waits behind (and drops the queue).
        stopWhileSending.current = false;
        setStopPending(false);
        const victim =
          res.delivery === "stopped" || res.delivery === "redirected"
            ? null
            : (res.run_id ?? activeRunIdRef.current);
        if (victim) {
          cancelRun(victim);
        }
      }
      if (res.delivery === "stopped") {
        toast.show(t("chat.stopped"), "info");
      }
      if (res.delivery === "redirected") {
        toast.show(t("chat.approvalSkipped"), "info");
        // The next turn stores the message a moment after the stopped run ends.
        for (const ms of [400, 1500, 4000]) window.setTimeout(() => void loadMessages(), ms);
      }
      setText("");
      attachments.clear();
      setNav(FRESH);
      scrollToEnd();
      await Promise.all([loadMessages(), refreshRuns()]);
    } catch (e) {
      stopWhileSending.current = false;
      setStopPending(false);
      setSendFailure(humanError(e));
    } finally {
      attachments.locked = false;
      setSending(false);
      setAckedRunId(null);
    }
  };

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (!menuOpen) {
      const recalled = historyKey(nav, sentTexts, {
        key: e.key,
        shift: e.shiftKey,
        ctrl: e.ctrlKey,
        meta: e.metaKey,
        alt: e.altKey,
        composing: e.nativeEvent.isComposing,
        menuOpen,
        text,
        selStart: e.currentTarget.selectionStart,
        selEnd: e.currentTarget.selectionEnd
      });
      if (recalled) {
        e.preventDefault();
        setNav(recalled.nav);
        setText(recalled.text);
        setCaret(recalled.caret);
        pendingCaret.current = recalled.caret;
        return;
      }
    }
    if (menuOpen && !e.nativeEvent.isComposing) {
      const n = menuItems.length;
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setMenuIdx((menuActive + 1) % Math.max(n, 1));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setMenuIdx((menuActive - 1 + n) % Math.max(n, 1));
        return;
      }
      if (n > 0 && (e.key === "Enter" || e.key === "Tab")) {
        e.preventDefault();
        complete(menuActive);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setMenuDismissed(dismissKey);
        return;
      }
    }
    if (
      shouldSend(
        {
          key: e.key,
          shiftKey: e.shiftKey,
          ctrlKey: e.ctrlKey,
          metaKey: e.metaKey,
          isComposing: e.nativeEvent.isComposing
        },
        sendWith
      )
    ) {
      e.preventDefault();
      void send();
    }
  };

  const nativeActive = control.mode !== "idle";
  // Stop was pressed in this chat: the queue is held (nothing new starts right after a Stop).
  const stoppedByUser = useRef(false);
  /** THE stop function of the chat: the round button calls only this (the Stop owner swaps it in). */
  const stopRun = async () => {
    stoppedByUser.current = true;
    // Through the Stop ledger (PR #94 owns the logic): the run is held (cannot act) at
    // once and the cancel is retried with the same stop id until the daemon reports it
    // terminal. A desktop-driving run is stopped with the app-wide Stop.
    // A second press while Stopping is already showing is the hard stop.
    const hard =
      stopPressed ||
      appLedger().runEntries().some(
        (e) => e.id === activeRunId && (e.phase === "held" || e.phase === "cancel_requested" || e.phase === "failed")
      );
    setStopPressed(true);
    if (nativeActive) {
      try {
        if (onNativeStop) await onNativeStop();
        else requestStop("banner");
      } catch (e) {
        setError(humanLine(e));
      }
      return;
    }
    if (!client) {
      return;
    }
    if (!activeRunId) {
      if (sending) {
        // The daemon has not named the run yet: remember the Stop, send() applies it.
        stopWhileSending.current = true;
        setStopPending(true);
      }
      return;
    }
    try {
      cancelRun(activeRunId, hard ? { force: true } : undefined);
      await refreshRuns();
    } catch (e) {
      setError(humanLine(e));
    }
  };

  const resume = async () => {
    if (!client || !pausedRunId) {
      return;
    }
    try {
      await client.resumeRun(pausedRunId);
      await refreshRuns();
    } catch (e) {
      setError(humanLine(e));
    }
  };

  // What waits for the user in this chat, whichever run it belongs to (a subagent's included); the
  // cards are drawn inline, or pinned above the composer when the timeline cannot draw them.
  const pending = pendingForChat(needs, events, { id: chatId, botId: chat?.bot_id });
  const askedHere = pending.own.length > 0;
  // The daemon says the run waits for the user but no card is known: said plainly, never as a plain wait.
  const waitLost = serverLive?.state === "waiting_approval" && !askedHere;
  const awaitingApproval = askedHere || waitLost;
  const chatPhase: ChatRunPhase = running
    ? awaitingApproval
      ? "waiting_approval"
      : "live"
    : pausedRunId
      ? "paused"
      : "none";
  // Queued messages: delivered when the run ends (the daemon's queue path), or at once by "Simdi gonder"
  // (its steer path). A Stop pressed here holds them, so nothing new starts right after a Stop.
  // The message leaves the queue only when its delivery starts (atomically, see queueDelivery) and only
  // that delivery puts it back. Returns whether a delivery started; otherwise the queue is untouched.
  const deliverQueued = (message: QueuedMessage, how: "queue" | "steer"): boolean =>
    delivery.deliver(message, !!client && !offline && !queueSending.current, async (m, hold) => {
      queueSending.current = true;
      setSending(true);
      const epoch = appLedger().stopEpoch();
      try {
        const key = await ownerKey(client!.baseUrl);
        if (epoch !== appLedger().stopEpoch()) {
          hold({ stopEpoch: epoch });
          return;
        }
        const response = await client!.sendMessage(chatId, m.text, how, key);
        queuedRun.current = response.run_id ?? null;
        if (response.run_id) setAckedRunId(response.run_id);
        if (stopWhileSending.current || epoch !== appLedger().stopEpoch()) {
          stopWhileSending.current = false;
          setStopPending(false);
          const victim = response.run_id ?? activeRunIdRef.current;
          if (victim) cancelRun(victim);
        }
        await Promise.all([loadMessages(), refreshRuns()]);
      } catch (e) {
        // A timeout may have delivered the message. Keep it for a deliberate retry;
        // never automatically replay an uncertain send after the next health check.
        hold({ offline: false, stopEpoch: -1 });
        offlineSending.current = false;
        stopWhileSending.current = false;
        setStopPending(false);
        setSendFailure(humanError(e));
      } finally {
        queueSending.current = false;
        setSending(false);
        setAckedRunId(null);
      }
    });
  /** Edit & resend (`text`) or regenerate the last turn: a new run; the old turn stays as "earlier version". */
  const resendTurn = async (text?: string) => {
    if (!client || !turn || resending) {
      return;
    }
    setResending(true);
    try {
      const res = await client.resendLastTurn(chatId, turn.user.id, await ownerKey(client.baseUrl), text);
      setEditingId(null);
      setAckedRunId(res.run_id);
      scrollToEnd();
      await Promise.all([loadMessages(), refreshRuns()]);
    } catch (e) {
      toast.show(humanLine(e), "danger");
      void loadMessages();
    } finally {
      setResending(false);
    }
  };
  /** Nothing may be running, paused, queued or waiting for an answer while the last turn is redone. */
  const canResend = turn?.resendable === true && !running && !pausedRunId && queue.length === 0 && !awaitingApproval;
  // Right-click menus of the timeline.  Quote / Ask only fill the composer; nothing is sent.
  const prefillComposer = (next: string) => {
    setText(next);
    setCaret(next.length);
    requestAnimationFrame(() => {
      const el = inputRef.current;
      if (el) {
        el.focus();
        el.setSelectionRange(next.length, next.length);
      }
    });
  };
  const lastAnswerKey = canResend && turn?.answered ? ([...entries].reverse().find((e) => e.kind === "assistant" && !e.live)?.key ?? null) : null;
  const chatMenu = useChatContextMenu({
    rootRef: timelineRef,
    entries,
    chatId,
    client,
    botId: chat?.bot_id,
    editableMsgId: canResend ? (turn?.user.id ?? null) : null,
    regenerateKey: lastAnswerKey,
    onQuote: (quoted) => prefillComposer(draftWithQuote(draftRef.current, quoteLines(quoted))),
    onAsk: (quoted) => prefillComposer(askDraft(draftRef.current, t("contextMenu.askIntro"), quoted)),
    onFind: (phrase) => {
      setFindSeed((s) => ({ q: phrase, n: s.n + 1 }));
      setFindOpen(true);
    },
    onEdit: setEditingId,
    onRegenerate: () => void resendTurn()
  });
  const wasActive = useRef(active);
  useEffect(() => {
    const was = wasActive.current;
    wasActive.current = active;
    if (active) offlineSending.current = false;
    if (was && !active && stoppedByUser.current) {
      stoppedByUser.current = false;
      return;
    }
    // A link only opens the chat: what was queued before it stays queued until the user acts.
    if (offline) return;
    const { message } = takeForRunEnd(queue, was, active, (m) => canAutoDeliver(m, appLedger().stopEpoch()) && (!external || queuedHere.current.has(m.id)));
    if (message) deliverQueued(message, "queue");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, queue, external, offline]);
  useEffect(() => {
    const message = queue[0];
    if (queuedRun.current && serverRuns.some((r) => r.id === queuedRun.current && !isProcessingRunState(r.state))) {
      offlineSending.current = false;
      queuedRun.current = null;
    }
    if (!connectionReport || offline || active || sending || offlineSending.current || stoppedByUser.current ||
        !client || !message?.offline || !canAutoDeliver(message, appLedger().stopEpoch()) || (external && !queuedHere.current.has(message.id))) return;
    // Only a delivery that really started blocks the next one; run end may have taken this message already.
    offlineSending.current = true;
    if (!deliverQueued(message, "queue")) offlineSending.current = false;
    // A sent message waits for its run to become active before another can start.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connectionReport, offline, active, sending, queue, client, external, serverRuns]);
  const sendNow = (id: string) => {
    if (offline || sending || queueSending.current) return;
    const message = queue.find((m) => m.id === id);
    if (message) deliverQueued(message, "steer");
  };
  useLedgerVersion();
  const ledgerRun = activeRunId
    ? appLedger().runEntries().find((e) => e.id === activeRunId)
    : undefined;
  const stopRecord = ledgerRun?.stopId
    ? appLedger().stopRecords().find((r) => r.id === ledgerRun.stopId)
    : undefined;
  useEffect(() => {
    if (!running) setStopPressed(false);
  }, [running]);
  const stopUi = runStopUi({
    ledgerPhase: ledgerRun?.phase,
    pending: stopPending,
    decidedMono: stopRecord?.decidedMono ?? null,
    now: performance.now(),
    error: ledgerRun?.error ?? null,
    unreachable: stopRecord?.phase === "retrying" && stopRecord.lastError !== null
  });
  // Re-render once the daemon has had STOP_CONFIRM_MS to confirm, so "not confirmed" shows up.
  const stopWaiting = stopUi?.phase === "stopping" && stopRecord !== undefined;
  // Something to send (text or a file): Send is pressable and the key hint shows; an empty box has a quiet Send.
  const hasDraft = text.trim() !== "" || files.length > 0;
  const keys = composerKeys(sendWith, isMacPlatform());
  const buttonMode = composerButton({
    runActive: running,
    hasText: files.length > 0 || (!sending && text.trim().length > 0),
    stopping: stopPressed || stopUi?.phase === "stopping"
  });
  const stopDecided = stopRecord?.decidedMono;
  const [, setStopTick] = useState(0);
  useEffect(() => {
    if (!stopWaiting || stopDecided === undefined) {
      return undefined;
    }
    const left = Math.max(STOP_CONFIRM_MS - (performance.now() - stopDecided), 0) + 50;
    const id = window.setTimeout(() => setStopTick((n) => n + 1), left);
    return () => window.clearTimeout(id);
  }, [stopWaiting, stopDecided]);
  const barModel = runBarModel({ chat: chatPhase, native: control.mode });
  const liveRunState = chatRuns.find((r) => r.id === activeRunId);
  const barStartedMs =
    liveRunState?.startedMs ?? serverLive?.created_at_ms ?? null;
  const liveTool = liveRunState
    ? [...liveRunState.items]
        .reverse()
        .find(
          (i): i is Extract<TimelineItem, { kind: "tool" }> =>
            i.kind === "tool" &&
            (i.status === "requested" || i.status === "running")
        )
    : undefined;
  let barDetail: string | null = nativeActive ? control.currentStep : null;
  if (!barDetail && liveTool) {
    const label = describeTool(liveTool.name, liveTool.input, (n) => {
      const b = brandIcon(n);
      return b.known ? b.name : null;
    });
    barDetail = t(`${label.key}.${stepPhase(liveTool.status)}`, label.values);
  }
  const decisions = useDecisionActions();
  const firstPending = pending.own[0];
  // The status line says WHAT the run waits for ("Waiting for your approval: Open example.com").
  const approvalText = waitLost
    ? t("runStatus.waitLost")
    : firstPending?.tool
      ? t("runStatus.waiting", {
          step: approvalHeadline(
            firstPending.tool.name,
            firstPending.tool.input,
            firstPending.tool.details,
            { computer: null, serverName: decisions.serverName },
            t as unknown as PlainT
          )
        })
      : firstPending?.question
        ? t("runStatus.waitAnswer", { question: firstPending.question.question.slice(0, 80) })
        : null;
  const jumpToPending = () => {
    const call = firstPending?.callId;
    const el = call ? document.querySelector(`[data-call-id="${call}"]`) : null;
    const instant =
      typeof window !== "undefined" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    el?.scrollIntoView({ block: "center", behavior: instant ? "instant" : "smooth" });
  };
  // ---- the approval dock: every open tool approval of this chat (and of the bot's other chats), asked
  // in ONE place above the composer. Several plain approvals of one run share one batch page.
  const dockEntries = [
    ...new Map([...pending.own, ...pending.pinned].map((e) => [e.callId, e] as const)).values()
  ]
    .filter((e) => e.kind === "approval" && e.tool !== undefined)
    .sort((a, b) => a.sinceMs - b.sinceMs);
  const dockPages: DockPage[] = [];
  {
    const batched = new Set<string>();
    for (const runId of new Set(dockEntries.map((e) => e.runId))) {
      const run = events.runs[runId];
      if (!run || run.parentRunId !== undefined) continue;
      const rows = groupPending(
        run.items.filter((i): i is Extract<TimelineItem, { kind: "tool" }> => i.kind === "tool")
      );
      if (!shouldBatch(rows)) continue;
      rows.forEach((r) => r.items.forEach((i) => batched.add(i.callId)));
      dockPages.push({
        key: `batch-${runId}`,
        render: () => (
          <ApprovalBatchCard
            variant="dock"
            rows={rows}
            onDecide={async (callIds, decision) => {
              for (const callId of callIds) {
                await decisions.decide(callId)(decision, decision === "approve" ? "once" : undefined);
              }
            }}
          />
        )
      });
    }
    for (const entry of dockEntries) {
      const tool = entry.tool;
      if (!tool || batched.has(entry.callId)) continue;
      dockPages.push({
        key: entry.callId,
        caption:
          entry.chatId === chatId
            ? null
            : t("chat.pendingElsewhere", {
                chat: chatTitleLabel(
                  chats.find((c) => c.id === entry.chatId)?.title ?? "",
                  t("botPanel.untitled")
                )
              }),
        render: (active) => (
          <ApprovalCard
            variant="dock"
            keyboard={active}
            tool={tool}
            serverName={decisions.serverName}
            onTrustFolder={decisions.trustFolder}
            botId={entry.botId}
            onDecide={decisions.decide(entry.callId)}
          />
        )
      });
    }
  }
  const approvalDocked = dockPages.length > 0;
  const barBotName =
    control.bot && control.bot.botId !== chat?.bot_id
      ? control.bot.botName
      : undefined;
  const lastFailed = [...chatRuns].reverse()[0]?.error;
  // Polite live region: one short line when a run ends (never the streaming text itself).
  const [announcement, setAnnouncement] = useState("");
  const runWasActive = useRef(false);
  const endedWithText = lastItem?.kind === "item" && lastItem.item.kind === "text";
  useEffect(() => {
    const said = runEndAnnouncement(runWasActive.current, active, endedWithText, Boolean(lastFailed));
    runWasActive.current = active;
    if (said) setAnnouncement(t(`chat.announce.${said}`));
    else if (active) setAnnouncement("");
  }, [active, endedWithText, lastFailed, t]);
  const avatarState = awaitingApproval ? "waiting-approval" : active ? (lastItem?.kind === "chain" || (lastItem?.kind === "item" && lastItem.item.kind === "tool") ? "working" : "thinking") : lastFailed ? "error" : "idle";
  // Hands-free voice call (docs/design/voice-call-mode.md): the transcribed text goes out as an ordinary
  // message; approvals stay click-only (the call only pauses while the card waits).
  const voiceCall = useVoiceCall({
    botId: chat?.bot_id,
    runs: chatRuns,
    awaitingApproval,
    runActive: active,
    sendText: async (spoken) => {
      // Nobody is looking at a screen to confirm a warning during a call: a credential is not sent.
      if (detectSecrets(spoken).length > 0) {
        toast.show(t("pastedSecret.voiceBlocked"), "danger");
        return;
      }
      if (client) {
        await client.sendMessage(chatId, spoken, undefined, await ownerKey(client.baseUrl));
      }
    }
  });
  // Live voice conversation (docs/design/voice-live.md): what needs the bot goes out as an ordinary
  // message, approvals stay click-only, and the words are written into this chat when it ends.
  const liveVoice = useLiveVoice({
    chatId,
    botId: chat?.bot_id,
    botName: view?.name ?? bot?.name ?? "",
    runs: chatRuns,
    awaitingApproval,
    runActive: active,
    callOpen: voiceCall.state.phase !== "idle",
    sendText: async (spoken) => {
      if (client) {
        await client.sendMessage(chatId, spoken, undefined, await ownerKey(client.baseUrl));
      }
    },
    onSaved: () => void loadMessages()
  });
  const liveOpen = liveVoice.state.phase !== "idle";
  // The bot that answered: a mentioned bot's run has its own bot id.
  const guestBot = (botId: string | null | undefined) =>
    botId && botId !== chat?.bot_id ? bots.find((b) => b.id === botId) : undefined;
  const runGuest = (runId: string) => guestBot(runs.find((r) => r.id === runId)?.bot_id);
  // Another bot's reply: its own avatar (as in the sidebar) and its name in its colour above the bubble.
  const guestName = (guest?: Bot) => {
    if (!guest) return null;
    const gv = botView(guest);
    return (
      <span className={styles.authorName} style={{ "--mention-color": gv.color } as CSSProperties} data-testid="guest-author">
        <BotAvatar bot={gv} size="xs" decorative />
        <span className={styles.authorLabel}>{gv.name}</span>
      </span>
    );
  };

  const rememberRun = async (runId: string) => {
    if (!client) {
      return;
    }
    setRemember((m) => ({ ...m, [runId]: "busy" }));
    try {
      await client.rememberRun(runId);
      setRemember((m) => ({ ...m, [runId]: "saved" }));
      toast.show(t("chat.remember.done"), "success");
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        // not eligible: the button goes away
        setRemember((m) => ({ ...m, [runId]: "hidden" }));
        toast.show(t("chat.remember.notEligible"), "info");
      } else {
        setRemember((m) => ({ ...m, [runId]: "idle" }));
        toast.show(
          t("chat.remember.failed", {
            message: humanLine(e)
          }),
          "danger"
        );
      }
    }
  };

  const cancelChild = async (childRunId: string) => {
    if (!client) {
      return;
    }
    cancelRun(childRunId);
    await refreshRuns();
  };

  const headerDrag = useDragPopOut({
    onPopOut: onPopOut ? (opts) => onPopOut(opts) : undefined,
    ignoreSelector: "button, a, input, textarea, select"
  });

  const runActions = timeline?.kind === "timeline" ? (
    <>
      <IconAction
        icon={ListOrdered}
        label={t("runReport.button")}
        onClick={() => setTimelineRunId(timeline.run.id)}
        testId="run-timeline"
      />
      {timeline.remember ? (
        (remember[timeline.run.id] ?? "idle") === "saved" ? (
          <span className={styles.rememberDone} role="status">
            {t("chat.remember.saved")}
          </span>
        ) : (
          <IconAction
            icon={Bookmark}
            label={t("chat.remember.button")}
            disabled={remember[timeline.run.id] === "busy"}
            onClick={() => void rememberRun(timeline.run.id)}
            testId="remember-run"
          />
        )
      ) : null}
      {timeline.workflow && client ? (
        <WorkflowReview client={client} runId={timeline.run.id} canRemember={true} />
      ) : null}
      {canResend && offersRegenerate(turn, Boolean(timeline.run.error)) ? (
        <RegenerateAction
          key={turn?.user.id}
          usedWriteTools={turn?.usedWriteTools === true}
          failed={turn?.answered !== true}
          busy={resending}
          onRegenerate={() => void resendTurn()}
        />
      ) : null}
    </>
  ) : null;

  const renderItem = (
    item: TimelineItem,
    runId: string
  ): ReactNode => {
    const run = events.runs[runId];
    const nested = run?.parentRunId !== undefined;
    switch (item.kind) {
      case "text":
        // The selected stored answer has its own focus target; avoid drawing its live copy too.
        if (replacedByJump(runId, item.text)) return null;
        if (nested) {
          return <p className={ccStyles.nestedText}>{item.text}</p>;
        }
        return (
          <div className={styles.assistantRow} data-message-row="assistant">
            <div className={styles.assistantBubble}>
              {guestName(runGuest(runId))}
              {/* Folding waits for the run to finish: a streaming answer must not collapse under the reader. */}
              <FoldedBody text={run?.finishedMs !== undefined ? item.text : ""} className={styles.foldBody} answer>
                <Markdown text={item.text} />
              </FoldedBody>
            </div>
            <MessageActions
              side="bot"
              runActions={actionsHost === `${runId}-${item.key}` ? runActions : undefined}
              meta={
                <MessageTime
                  ts={item.ts}
                  mode={messageTimes}
                  finishedTs={run?.finishedMs}
                />
              }
            >
              <CopyButton text={item.text} />
              <SpeakButton id={`${runId}:${item.ts}`} text={item.text} botId={chat?.bot_id} />
              <MessageInfo client={client} side="bot" ts={item.ts} runId={runId} run={run} runDto={runs.find((r) => r.id === runId)} />
            </MessageActions>
          </div>
        );
      case "tool": {
        const child =
          run && item.status !== "awaiting_approval"
            ? assignChildRuns(events, run)[item.callId]
            : undefined;
        if (child) {
          return (
            <SubagentCard
              run={child}
              renderItem={renderItem}
              onCancel={cancelChild}
            />
          );
        }
        if (item.status === "awaiting_approval" && nested) {
          // A subagent's card is pinned above the composer (never inside a folded card).
          return <p className={ccStyles.nestedText}>{t("chat.pendingPinned")}</p>;
        }
        // A pending approval is asked in the dock above the composer, never in the stream; once it is
        // answered the step line of the run is its trace.
        if (item.status === "awaiting_approval") {
          return null;
        }
        return <ToolCard tool={item} />;
      }
      case "question":
        if (nested && item.answer === undefined && !item.closed) {
          return <p className={ccStyles.nestedText}>{t("chat.pendingPinned")}</p>;
        }
        return (
          <QuestionCard
            q={item}
            onAnswer={decisions.answer(runId, item.callId)}
          />
        );
      case "secret":
        if (nested && item.outcome === undefined && !item.closed) {
          return <p className={ccStyles.nestedText}>{t("chat.pendingPinned")}</p>;
        }
        return (
          <SecretRequestCard
            item={item}
            botName={bots.find((b) => b.id === run?.botId)?.name ?? bot?.name ?? ""}
            onSubmit={decisions.submitSecret(runId, item.callId)}
            onCancel={decisions.cancelSecret(runId, item.callId)}
          />
        );
      case "handover":
        if (nested && item.outcome === undefined && !item.closed) {
          return <p className={ccStyles.nestedText}>{t("chat.pendingPinned")}</p>;
        }
        return (
          <HandoverCard
            item={item}
            botName={bots.find((b) => b.id === run?.botId)?.name ?? bot?.name ?? ""}
            onSubmitCode={decisions.submitCode(runId, item.callId)}
            onDone={decisions.handoverDone(runId, item.callId)}
            onCancel={decisions.cancelHandover(runId, item.callId)}
            onTakeControl={decisions.takeControl}
          />
        );
      case "elicitation":
        if (nested && item.outcome === undefined && !item.closed) {
          return <p className={ccStyles.nestedText}>{t("chat.pendingPinned")}</p>;
        }
        return (
          <ElicitationCard
            item={item}
            onAnswer={decisions.answerElicitation(runId, item.callId)}
            onCancel={decisions.cancelElicitation(runId, item.callId)}
          />
        );
      case "todo":
        return <TodoCard todo={item} />;
      case "notice":
        return <NoticeLine n={item} />;
    }
  };

  if (!chat) {
    return (
      <div className={styles.empty}>
        <p>{t("chat.notFound")}</p>
        {onBack ? (
          <Button variant="secondary" onClick={onBack}>
            <ArrowLeft />{t("common.back")}
          </Button>
        ) : null}
      </div>
    );
  }


  const liveStatus = awaitingApproval
    ? t("chat.runBar.waiting")
    : running
      ? lastItem?.kind === "item" && lastItem.runId === activeRunId && lastItem.item.kind === "text"
        ? t("liveView.chat.replying")
        : barDetail ?? t("liveView.chat.working")
      : null;
  // In the full-screen live view the whole chat (history, run status, composer) moves into the viewer's
  // chat card or column; the composer is one piece of it, so a dock above it works in every state.
  const chatRoot = (
    <div className={styles.root} data-live-dock={liveChatSlots?.root ? "true" : undefined}>
      {/* v6: no title row (the chat is named in the sidebar); a back button only while a page to go back to exists. */}
      {!hideHeader && (onBack || onPopOut) && !liveChatSlots?.root ? (
        <header className={styles.header} {...headerDrag.dragProps}>
          {onBack ? (
            <Button size="sm" variant="ghost" className={styles.back} onClick={onBack}>
              <ArrowLeft aria-hidden="true" />
              {t("common.back")}
            </Button>
          ) : null}
          {onPopOut ? (
            <div className={styles.headerActions}>
              <Button
                size="sm"
                variant="ghost"
                title={t("popout.openChatTooltip")}
                aria-label={t("popout.openChat")}
                data-testid="chat-popout-button"
                onClick={() => onPopOut()}
              >
                <ExternalLink aria-hidden="true" />
              </Button>
            </div>
          ) : null}
        </header>
      ) : null}

      {!liveChatSlots?.root ? <MissionChatHeader chat={chatMission} /> : null}
      <div className={styles.scrollWrap}>
      <LiveVoicePanel live={liveVoice} botName={view?.name ?? bot?.name ?? ""} />
      {findOpen ? (
        <ChatFindBar key={findSeed.n} rootRef={timelineRef} contentKey={entries} initialQuery={findSeed.q} onClose={() => setFindOpen(false)} />
      ) : null}
      <div
        className={styles.scroller}
        ref={scrollerRef}
        onScroll={onScroll}
        data-testid="chat-timeline"
        role="log"
        aria-label={t("chat.timelineLabel")}
        aria-live="off"
        tabIndex={0}
        {...chatMenu.bind}
      >
        {chatMenu.menu}
        {/* One Tab from the transcript reaches the message box instead of walking every control of every past message. */}
        <button
          type="button"
          className={styles.skipToComposer}
          data-testid="skip-to-composer"
          onClick={() => inputRef.current?.focus()}
        >
          {t("chat.skipToComposer")}
        </button>
        <div
          ref={timelineRef}
          className={[
            styles.timeline,
            entries.length === 0 ? styles.timelineEmpty : ""
          ]
            .filter(Boolean)
            .join(" ")}
        >
          {entries.length > 0 && view?.name ? <h1 className={styles.srOnly}>{view.name}</h1> : null}
          {entries.length === 0 ? (
            <div className={styles.emptyConversation}>
              <BotAvatar
                bot={view ?? { id: "bot", name: "Bot" }}
                size="hero"
                state="idle"
              />
              {/* One readable surface for the greeting and its line (over a wallpaper), not a pill per line. */}
              <div className={styles.welcomeCopy} data-testid="welcome-copy">
                <h1>{t("chat.emptyGreeting", { name: view?.name ?? "" })}</h1>
                <p className={styles.hint}>{t("chat.emptySub")}</p>
              </div>
              <div className={styles.starters}>
                {STARTERS.map((k) => (
                  <button
                    key={k}
                    type="button"
                    className={styles.starter}
                    onClick={() => applyStarter(t(`chat.starters.${k}`))}
                  >
                    {t(`chat.starters.${k}`)}
                  </button>
                ))}
              </div>
            </div>
          ) : null}
          {entries.map((e, index) => {
            // The bot's avatar leads a block of bot output; later rows of the block line up under it.
            const prev = entries[index - 1];
            const lead =
              !prev || prev.kind === "user" || prev.kind === "day" || prev.kind === "unread";
            switch (e.kind) {
              case "unread":
                return <NewMessagesDivider key={e.key} onPassed={dismissUnread} />;
              case "day":
                return (
                  <div key={e.key} className={styles.conversationLabel}>
                    {dayHeader(
                      e.ts,
                      isMockMode() ? e.ts : Date.now(),
                      i18n.language,
                      { today: t("chat.conversationLabel"), yesterday: t("chat.yesterdayLabel") },
                      messageTimes === "off"
                    )}
                  </div>
                );
              case "user":
                return (
                  <UserMessageRow key={e.key} id={e.key} msgId={e.msgId} ts={e.ts} text={e.text}
                    source={e.source} attachments={e.attachments} client={client} chatId={chatId}
                    mentionBots={mentionBots} times={messageTimes}
                    editable={canResend && turn?.user.id === e.msgId}
                    editing={editingId === e.msgId && canResend}
                    busy={editingId === e.msgId && resending}
                    usedWriteTools={editingId === e.msgId && turn?.usedWriteTools === true}
                    onEdit={setEditingId}
                    onSave={editingId === e.msgId ? (value) => void resendTurn(value) : undefined}
                    onCancel={editingId === e.msgId ? () => setEditingId(null) : undefined} />
                );
              case "superseded":
                return <SupersededView key={e.key} group={e.group} />;
              case "assistant": {
                // The report a mission posts when it ends or waits for the owner, as a card.
                const report = chatMission.mission && !e.runId ? parseMissionDigest(e.text) : null;
                if (report) {
                  return (
                    <div key={e.key} id={e.key} data-message-row="assistant">
                      <MissionDigestMessage digest={report} />
                    </div>
                  );
                }
                return (
                  <AssistantMessageRow key={e.key} id={e.key} ts={e.ts} text={e.text} source={e.source}
                    botId={e.botId ?? chat?.bot_id} runId={e.runId}
                    guest={lead ? guestBot(e.botId) : undefined} times={messageTimes} client={client}
                    run={e.runId ? events.runs[e.runId] : undefined}
                    runDto={runs.find((r) => r.id === e.runId)}
                    runActions={actionsHost === e.key ? runActions : undefined} />
                );
              }
              case "mission":
                // A mission started from this chat: its progress and pause/stop, one click from its own chat.
                return (
                  <div key={e.key} id={e.key} className={styles.missionRow} data-testid="chat-mission-row">
                    <MissionStartedCard mission={e.mission} onChanged={chatMission.refresh} />
                  </div>
                );
              case "imported_note": {
                const counts = importedNoteCounts(e.text);
                return (
                  <div key={e.key} className={styles.importedNote} data-message-row="imported_note">
                    {counts ? t("chat.importedNote", counts) : e.text}
                  </div>
                );
              }
              case "runStopped":
                return (
                  <div key={e.key} className={styles.runStopped} data-testid="run-stopped">
                    {t("chat.runStopped")}
                  </div>
                );
              case "runError":
                return (
                  <div key={e.key} className={styles.runError}>
                    {parseLimitReached(e.message) ? (
                      <BotLimitCard limit={parseLimitReached(e.message)!} />
                    ) : (
                      <ErrorNotice compact error={runErrorFor(e.message, e.key === `${lastRunId}-error`)} onAction={onRunErrorAction} />
                    )}
                  </div>
                );
              case "route": {
                const route = e.run.route;
                const chip = route ? routeChip(route, showAgentRoute) : null;
                if (!route || !chip) {
                  return null;
                }
                const hover = t("chat.route.hover", routeHoverValues(route));
                const label = t(chip.i18nKey, chip.values);
                return (
                  <div key={e.key} className={styles.routeRow}>
                    <span
                      className={[
                        styles.routeChip,
                        chip.cheap ? styles.routeChipCheap : ""
                      ]
                        .filter(Boolean)
                        .join(" ")}
                      title={hover}
                      aria-label={`${label}. ${hover}`}
                      tabIndex={0}
                      data-testid="route-chip"
                    >
                      {chip.cheap ? (
                        <Zap className={styles.routeIcon} />
                      ) : null}
                      <span className={styles.routeLabel}>{label}</span>
                    </span>
                  </div>
                );
              }
              case "changes": {
                const restoreSignal = e.run.items.filter(
                  (item) =>
                    item.kind === "notice" &&
                    ["chat.notice.checkpointRestored", "chat.notice.checkpointRestorePartial"].includes(item.i18nKey ?? "")
                ).length;
                return (
                  <div key={e.key} className={styles.laneRow}>
                    <RunChangesGate
                      runId={e.run.id}
                      client={client}
                      restoreSignal={restoreSignal}
                      safetyCheckpointIds={e.run.items.flatMap((item) => item.kind === "notice" && ["chat.notice.checkpointRestored", "chat.notice.checkpointRestorePartial"].includes(item.i18nKey ?? "") && item.values?.safetyId ? [String(item.values.safetyId)] : [])}
                    />
                  </div>
                );
              }
              case "timeline":
                return actionsHost === null ? (
                  <div key={e.key} className={styles.rememberRow}>
                    <RunActionBar label={t("chat.runActions")}>{runActions}</RunActionBar>
                  </div>
                ) : null;
              case "chain":
                return (
                  <ChatStepGroup key={e.key} chain={e.chain} times={messageTimes} lead={lead}
                    bot={runGuest(e.runId) ?? bot} botId={chat?.bot_id}
                    state={runGuest(e.runId) ? undefined : avatarState} />
                );
              case "item":
                if (
                  e.item.kind === "tool" &&
                  e.item.status === "awaiting_approval" &&
                  events.runs[e.runId]?.parentRunId === undefined
                ) {
                  return null;
                }
                return (
                  <div
                    key={e.key}
                    className={[
                      styles.itemRow,
                      e.item.kind === "text" ? styles.itemRowText : ""
                    ]
                      .filter(Boolean)
                      .join(" ")}
                  >
                    {renderItem(e.item, e.runId)}
                  </div>
                );
            }
          })}
          {canResend && turn?.answered && lastActionsKey === null ? (
            <RegenerateRow
              key={turn.user.id}
              usedWriteTools={turn.usedWriteTools}
              busy={resending}
              onRegenerate={() => void resendTurn()}
            />
          ) : null}
          <QueuedMessages
            queue={queue}
            offline={offline}
            stopEpoch={appLedger().stopEpoch()}
            onSendNow={sendNow}
            onRemove={(id) => updateQueue((q) => removeQueued(q, id))}
          />
        </div>
      </div>
      <p className={styles.srOnly} role="status" aria-live="polite" data-testid="chat-announce">
        {announcement}
      </p>
      {jumpVisible ? (
        <button
          type="button"
          className={styles.jump}
          onClick={() => scrollToEnd(true)}
        >
          <ArrowDown aria-hidden="true" />
          {t("chat.jumpToLatest")}
        </button>
      ) : null}
      </div>

      {liveChatSlots?.status && liveStatus ? createPortal(
        <p className={styles.liveReplyStatus} role="status">{liveStatus}</p>,
        liveChatSlots.status
      ) : null}
      <footer
        className={styles.composer}
        onDragOver={(e) => {
          if (e.dataTransfer.types.includes("Files")) {
            e.preventDefault();
            setDragOver(true);
          }
        }}
        onDragLeave={(e) => {
          if (!e.currentTarget.contains(e.relatedTarget as Node | null)) {
            setDragOver(false);
          }
        }}
        onDrop={(e) => {
          const dropped = filesOf(e.dataTransfer);
          setDragOver(false);
          if (e.dataTransfer.types.includes("Files")) e.preventDefault();
          if (dropped.length > 0) {
            e.preventDefault();
            addFiles(dropped);
          }
        }}
        data-dragover={dragOver ? "true" : undefined}
      >
        {bot && computerTargetOf(bot) === "mine" ? <ComputerUseOffCard /> : null}
        {bot ? <BotFilesCard bot={bot} /> : null}
        {error ? (
          <p className={styles.error} role="alert">
            {error}
          </p>
        ) : null}
        {hostStarting ? (
          <p className={styles.error} role="status" data-testid="host-starting">
            {t("hostRecovery.starting")}
          </p>
        ) : null}
        {noModelCard && noModelYet ? (
          <div className={styles.error} role="status" data-testid="no-model-card">
            <strong>{t("modelPicker.connectTitle")}</strong>
            <p>{t("modelPicker.connectBody")}</p>
            <Button
              size="sm"
              onClick={() => {
                setNoModelCard(false);
                openSettingsTarget({ page: "models", card: "keys", field: "api-key" });
              }}
            >
              {t("modelPicker.connectAction")}
            </Button>
          </div>
        ) : null}
        {sendFailure ? (
          <ErrorNotice
            compact
            error={sendFailure}
            busy={sending}
            onAction={(a) => (a === "restartService" ? void restartDaemon(undefined, "retry-chat-notice") : void send())}
          />
        ) : null}
        {secretKinds ? (
          <PastedSecretWarning
            kinds={secretKinds}
            onDismiss={() => setSecretKinds(null)}
            onSendAnyway={() => {
              setSecretKinds(null);
              void send(true);
            }}
            onRemove={() => {
              setText((cur) => removeSecrets(cur, detectSecrets(cur)));
              setSecretKinds(null);
              inputRef.current?.focus();
            }}
            onStore={async (name) => {
              if (!client) throw new Error(t("pastedSecret.notConnected"));
              const analysed = text;
              const plan = planStore(analysed, detectSecrets(analysed), name);
              const stored: string[] = [];
              try {
                for (const entry of plan.entries) {
                  await client.createFillSecret({ name: entry.name, value: entry.value });
                  stored.push(entry.name);
                }
              } catch (e) {
                // The draft stays exactly as typed; say which entries already are in the vault.
                if (stored.length > 0) {
                  toast.show(t("pastedSecret.partialStored", { names: stored.join(", ") }), "danger");
                }
                throw e;
              } finally {
                plan.entries.length = 0;
              }
              // The plan fits only the text it was made from; an edit made meanwhile is never overwritten.
              if (draftRef.current === analysed) {
                setText(plan.text);
              } else {
                toast.show(t("pastedSecret.editedMeanwhile", { names: stored.join(", ") }), "info");
              }
              setSecretKinds(null);
              inputRef.current?.focus();
            }}
          />
        ) : null}
        <PendingDecisions
          entries={pending.pinned.filter((e) => e.kind !== "approval")}
          caption={(entry) =>
            entry.chatId === chatId
              ? null
              : t("chat.pendingElsewhere", {
                  chat: chatTitleLabel(
                    chats.find((c) => c.id === entry.chatId)?.title ?? "",
                    t("botPanel.untitled")
                  )
                })
          }
        />
        <div className={styles.dockWrap}>
          <RunStatusLine
            model={
              // The approval is asked in the dock: no second strip saying the same thing.
              approvalDocked && barModel?.kind === "waiting" && !barModel.native && !stopUi
                ? null
                : barModel
            }
            botName={barBotName}
            sentence={barDetail}
            approvalText={approvalText}
            onJump={firstPending && !waitLost ? jumpToPending : undefined}
            startedMs={barStartedMs}
            runStop={stopUi}
            onStop={() => void stopRun()}
            humanDriving={control.mode === "human_driving"}
            onPause={() => void pauseBotRun().then(onControl).catch(() => {})}
            onResume={() => {
              if (barModel?.native) {
                void resumeBotRun().then(onControl).catch(() => {});
              } else {
                void resume();
              }
            }}
            onWheel={() => {
              const next =
                control.mode === "human_driving" ? giveBackControl() : takeTheWheel();
              void next.then(onControl).catch(() => {});
            }}
          />
        </div>
        <div className={styles.boxWrap} data-docked={approvalDocked ? "true" : undefined}>
          <ApprovalDock pages={dockPages} />
          {menuOpen ? (
            <div
              className={styles.menu}
              data-testid="composer-menu"
              ref={menuRef}
              onMouseDown={(e) => e.preventDefault()}
            >
              <p className={styles.menuHead}>
                {triggerKind === "slash"
                  ? t("chat.slash.menu")
                  : t("chat.slash.mentionMenu")}
              </p>
              <div
                className={styles.menuList}
                id={menuId}
                role="listbox"
                aria-label={
                  triggerKind === "slash"
                    ? t("chat.slash.menu")
                    : t("chat.slash.mentionMenu")
                }
              >
                {optionGroups(menuItems).map((group) => (
                  <div key={group} role="group" aria-label={t(`chat.menu.groups.${group}`)}>
                    <p className={styles.menuGroup} aria-hidden="true">{t(`chat.menu.groups.${group}`)}</p>
                    <div className={styles.menuGroupList}>
                {menuItems.map((m, i) => m.group !== group ? null : (
                  <div
                    key={m.id}
                    id={`${menuId}-${i}`}
                    data-bot-id={m.bot?.id}
                    role="option"
                    aria-selected={i === menuActive}
                    className={[
                      styles.menuItem,
                      i === menuActive ? styles.menuItemActive : ""
                    ]
                      .filter(Boolean)
                      .join(" ")}
                    onMouseDown={(e) => {
                      e.preventDefault();
                      complete(i);
                    }}
                    onMouseMove={() => setMenuIdx(i)}
                  >
                    <span
                      className={[styles.menuIcon, m.bot ? styles.menuIconBot : ""].filter(Boolean).join(" ")}
                      aria-hidden="true"
                    >
                      {m.bot ? (
                        <BotAvatar bot={m.bot} size="sm" decorative />
                      ) : (
                        <SlashIcon name={m.id} />
                      )}
                    </span>
                    <span className={styles.menuCopy}>
                      <span className={styles.menuLabel}>{Array.from(m.label).map((char, index) =>
                        m.indices.includes(index) ? <mark key={index}>{char}</mark> : <span key={index}>{char}</span>
                      )}</span>
                      {m.detail ? <span className={styles.menuDetail}>{m.detail}</span> : null}
                    </span>
                  </div>
                ))}
                    </div>
                  </div>
                ))}
              </div>
              {menuItems.length === 0 ? <p className={styles.menuEmpty} role="status"><Search aria-hidden="true" />{t("chat.menu.empty")}</p> : null}
              <p className={styles.menuKeys} aria-hidden="true">
                <kbd>↑</kbd>
                <kbd>↓</kbd> {t("chat.menu.navigate")}
                <kbd>↵</kbd> {t("chat.menu.select")}
                <kbd>esc</kbd> {t("chat.menu.close")}
              </p>
            </div>
          ) : null}
          <div
            className={styles.box}
            data-testid="composer-box"
            onMouseDown={(e) => {
              // Empty surface: focus the field. Controls and the field itself keep their own behaviour.
              if (e.button === 0 && shouldFocusOnBoxClick(e.target, e.currentTarget)) {
                e.preventDefault();
                inputRef.current?.focus();
              }
            }}
          >
          <ComposerFiles files={files} onRemove={removeFile} onRetry={(key) => attachments.retry(key)} disabled={sending} />
          {/* ARIA 1.1 combobox: a <textarea> may not carry role="combobox", so the wrapper does and owns the list. */}
          <div
            className={styles.inputWrap}
            role="combobox"
            aria-haspopup="listbox"
            aria-expanded={menuOpen}
            aria-owns={menuOpen ? menuId : undefined}
            aria-controls={menuOpen ? menuId : undefined}
            aria-label={t("chat.placeholder")}
          >
          {paintsMentions ? (
            <div className={styles.mirror} aria-hidden="true">
              <div style={{ transform: `translateY(-${inputScroll}px)` }}>
                {mentionParts.map((p, i) =>
                  p.color ? (
                    <span key={i} className={styles.typedMention} style={{ color: p.color }}>{p.text}</span>
                  ) : (
                    <span key={i}>{p.text}</span>
                  )
                )}
                {"​"}
              </div>
            </div>
          ) : null}
          <textarea
            ref={inputRef}
            data-composer-input=""
            className={paintsMentions ? `${styles.input} ${styles.inputPainted}` : styles.input}
            onScroll={(e) => setInputScroll(e.currentTarget.scrollTop)}
            value={text}
            rows={1}
            aria-autocomplete="list"
            aria-controls={menuOpen ? menuId : undefined}
            aria-activedescendant={
              menuOpen && menuItems.length > 0 ? `${menuId}-${menuActive}` : undefined
            }
            placeholder={t(composerPlaceholderKey({ awaitingApproval, runActive: active }))}
            aria-label={t("chat.placeholder")}
            onChange={(e) => {
              setNav((n) => afterEdit(n, e.target.value));
              setText(e.target.value);
              setSecretKinds(null);
              setCaret(e.target.selectionStart);
              setMenuIdx(0);
            }}
            onPaste={(e) => {
              const pasted = filesOf(e.clipboardData);
              if (pasted.length > 0) {
                e.preventDefault();
                addFiles(pasted);
              }
            }}
            onSelect={(e) => setCaret(e.currentTarget.selectionStart)}
            onKeyDown={onKeyDown}
            onFocus={() => setMenuDismissed(null)}
            onBlur={(e) => {
              // focus moving into the menu (scrollbar, heading) is not a dismissal
              if (
                e.relatedTarget instanceof Node &&
                menuRef.current?.contains(e.relatedTarget)
              ) {
                return;
              }
              setMenuDismissed(dismissKey);
            }}
          />
          </div>
          <div className={styles.tools}>
            <ComposerPlusMenu
              className={styles.plus}
              attachDisabled={!client || sending}
              attachTitle={`${t("chat.attach.add")} — ${t("chat.attach.limits")}`}
              onCommands={() => {
                if (text === "") {
                  setText("/");
                  setCaret(1);
                }
                inputRef.current?.focus();
              }}
              onAttach={() => fileInputRef.current?.click()}
              voiceTitle={t(liveOpen ? "voice.live.blockedByLive" : (voiceCall.blockedReason ?? "voice.call.start"))}
              voiceBlocked={liveOpen || ((voiceCall.state.phase === "idle" || voiceCall.state.phase === "failed") && Boolean(voiceCall.blockedReason))}
              voiceNotice={liveOpen ? t("voice.live.blockedByLive") : voiceCall.state.notice ? t(voiceCall.state.notice) : voiceCall.blockedReason ? t(voiceCall.blockedReason) : undefined}
              voiceExpanded={voiceCall.state.phase !== "idle"}
              voiceControls={voiceCall.state.phase !== "idle" ? "voice-call-panel" : undefined}
              onVoice={() => {
                if (liveOpen) return;
                if (voiceCall.state.phase === "idle" || voiceCall.state.phase === "failed") voiceCall.start();
                else document.querySelector<HTMLElement>('[data-testid="voice-call"]')?.focus();
              }}
            />
            <input
              ref={fileInputRef}
              type="file"
              multiple
              accept={ATTACHMENT_ACCEPT}
              disabled={sending}
              hidden
              data-testid="attach-input"
              onChange={(e) => {
                addFiles(Array.from(e.target.files ?? []));
                e.target.value = "";
              }}
            />
            <TalkButton live={liveVoice} name={view?.name ?? bot?.name ?? ""} className={styles.plus} />
            <DictateButton
              className={styles.plus}
              onText={(dictated) => {
                setText((cur) => appendDictation(cur, dictated));
                inputRef.current?.focus();
              }}
            />
            {bot ? (
              <div className={styles.modelSlot}>
                <ModelPicker
                  groups={pickerGroups}
                  value={view?.executor ?? ""}
                  engines
                  disabled={!client}
                  thinking
                  recents
                  placement="top-start"
                  onPick={(ref) => void pickModel(ref)}
                />
              </div>
            ) : null}
            {bot ? <ComputerTargetChip bot={bot} /> : null}
            <span className={styles.spacer} />
            {hasDraft && buttonMode === "send" ? (
              <span className={styles.keyHint} aria-hidden="true" data-testid="composer-keys">
                <kbd>{keys.send}</kbd>
                {t(running ? "chat.keys.queue" : "chat.keys.send")}
                <kbd>{keys.newline}</kbd>
                {t("chat.keys.newline")}
              </span>
            ) : null}
            <button
              type="button"
              className={styles.send}
              data-mode={buttonMode}
              data-testid="send-button"
              disabled={buttonMode === "send" && (sending || blockedFiles || !hasDraft)}
              data-empty={buttonMode === "send" && !hasDraft ? "true" : undefined}
              title={
                buttonMode === "stopping"
                  ? t("runStatus.stopping")
                  : buttonMode === "stop"
                    ? t(stopUi ? "chat.runBar.forceStop" : "chat.stop")
                    : hasDraft
                      ? `${t("chat.send")} (${keys.sendName})`
                      : t("chat.keys.empty")
              }
              aria-label={
                buttonMode === "stopping"
                  ? t("runStatus.stopping")
                  : buttonMode === "stop"
                    ? t(stopUi ? "chat.runBar.forceStop" : "chat.stop")
                    : t("chat.send")
              }
              onClick={() => (buttonMode === "send" ? void send() : void stopRun())}
            >
              {buttonMode === "send" ? (
                <ArrowUp aria-hidden="true" />
              ) : (
                <>
                  <span className={styles.stopMark} aria-hidden="true" />
                  {buttonMode === "stopping" ? <span className={styles.srOnly}>{t("runStatus.stopping")}</span> : null}
                </>
              )}
            </button>
          </div>
          </div>
        </div>
      </footer>
      <VoiceCallPanel call={voiceCall} bot={view ?? { id: chat?.bot_id ?? "unknown", name: "Bot" }} />
      <RunTimelineModal
        run={chatRuns.find((r) => r.id === timelineRunId) ?? null}
        botName={bot?.name ?? ""}
        onClose={() => setTimelineRunId(null)}
      />
    </div>
  );
  return liveChatSlots?.root ? createPortal(chatRoot, liveChatSlots.root) : chatRoot;
}
