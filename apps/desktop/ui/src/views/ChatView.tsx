import {
  ArrowDown,
  ArrowLeft,
  ArrowUp,
  Bookmark,
  Paperclip,
  Pencil,
  Phone,
  Plus,
  Square,
  CalendarClock,
  CircleHelp,
  Download,
  LayoutGrid,
  Settings2,
  Brain,
  Search,
  Zap
} from "lucide-react";
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
  useState
} from "react";
import { useTranslation } from "react-i18next";
import { createPortal } from "react-dom";
import type { LiveChatSlots } from "./LiveViewerChat";
import { ownerKey } from "../lib/ownerKey";
import { chatActivityMs } from "../lib/unread";
import { useChatRead } from "../state/useChatRead";
import { NewMessagesDivider } from "../components/NewMessagesDivider";
import { BotAvatar, Button } from "ui";
import { isMockMode } from "../api/mockMode";
import { ApiError } from "../api/client";
import type { Bot, Message } from "../api/types";
import {
  ApprovalCard,
  NoticeLine,
  QuestionCard,
  SubagentCard,
  TodoCard,
  ToolCard
} from "../components/ChatCards";
import { ApprovalBatchCard } from "../components/ApprovalBatchCard";
import { PendingDecisions, useDecisionActions } from "../components/PendingDecisions";
import { SecretRequestCard } from "../components/SecretRequestCard";
import { PastedSecretWarning } from "../components/PastedSecretWarning";
import { detectSecrets, kindsToWarn, planStore, removeSecrets } from "../lib/secretDetect";
import { chatTitleLabel } from "../lib/chatTitle";
import { pendingForChat } from "../lib/pendingDecisions";
import { ChatFindBar } from "../components/ChatFindBar";
import { Markdown } from "../components/Markdown";
import { UserBubble } from "../components/UserBubble";
import { MessageTime } from "../components/MessageTime";
import { ModelPicker } from "../components/ModelPicker";
import { DictateButton } from "../components/DictateButton";
import { useVoiceCall } from "../components/voiceCall/useVoiceCall";
import { VoiceCallPanel } from "../components/voiceCall/VoiceCallPanel";
import { MessageInfo } from "../components/MessageInfo";
import { MessageActions } from "../components/MessageActions";
import { EditMessage, RegenerateRow, SupersededView } from "../components/ResendTurn";
import { CopyButton } from "../components/CopyButton";
import { useAutoSpeak } from "../components/useAutoSpeak";
import { SpeakButton } from "../components/SpeakButton";
import { QueuedMessages } from "../components/QueuedMessages";
import { RunStatusLine } from "../components/RunStatusLine";
import { RunChangesGate } from "../components/RunChangesGate";
import { ThinkingChain } from "../components/ThinkingChain";
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
import { lastTurn, supersededGroups, supersededRunIds, type SupersededGroup } from "../lib/resendTurn";
import { botWriteMessage, writeBotProfile } from "../lib/botWrite";
import { splitMentions } from "../lib/mentionText";
import { saveChatExport } from "../lib/chatHistory";
import { dayHeader, type MessageTimes } from "../lib/messageTime";
import { modelGroups } from "../lib/modelPicker";
import { type ChatRunPhase, runBarModel, runStopUi, STOP_CONFIRM_MS } from "../lib/runBar";
import { sendButtonDisabled, sendButtonMode } from "../lib/composerRun";
import { type PendingFile, MAX_ATTACHMENT_BYTES, filesOf, room, uploadErrorKey } from "../lib/attachments";
import { ComposerFiles, MessageFiles } from "../components/Attachments";
import { enqueue, putBack, removeQueued, takeForNow, takeForRunEnd } from "../lib/messageQueue";
import { groupPending, isBatchable, shouldBatch } from "../lib/approvalBatch";
import { toolSentence } from "../lib/runSentence";
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
  isExportCommand,
  appCommandFor
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
import {
  assignChildRuns,
  itemTs,
  type RunState,
  runText,
  type TimelineItem,
  topLevelRuns
} from "../state/timeline";
import ccStyles from "../components/ChatCards.module.css";
import styles from "./ChatView.module.css";
import msgStyles from "../components/MessageActions.module.css";
import { type ErrorAction, type HumanError, humanError, humanLine, humanToolError } from "../lib/humanError";
import type { ApiClient } from "../api/client";
import { sendWithHostRecovery } from "../lib/hostRecovery";
import { ComputerTargetChip } from "../components/ComputerTargetChip";
import { ErrorNotice } from "../components/ErrorNotice";

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
  settings: Settings2, models: Brain, apps: LayoutGrid, schedules: CalendarClock
};

type Entry =
  | { kind: "day"; key: string; ts: number }
  | { kind: "unread"; key: string; ts: number }
  | { kind: "user"; key: string; ts: number; msgId: string; text: string; source?: string | null; attachments?: Message["attachments"] }
  | { kind: "superseded"; key: string; ts: number; group: SupersededGroup }
  | { kind: "assistant"; key: string; ts: number; text: string; live: boolean; botId?: string | null; runId?: string | null }
  | { kind: "item"; key: string; ts: number; item: TimelineItem; runId: string }
  | { kind: "chain"; key: string; ts: number; chain: Chain; runId: string }
  | { kind: "runError"; key: string; ts: number; message: string }
  | { kind: "runStopped"; key: string; ts: number }
  | { kind: "route"; key: string; ts: number; run: RunState }
  | { kind: "changes"; key: string; ts: number; run: RunState }
  | { kind: "remember"; key: string; ts: number; run: RunState }
  | { kind: "workflow"; key: string; ts: number; run: RunState };

/** `idle` = offer the button; `saved` = done; `hidden` = the daemon said no (409). */
type RememberState = "idle" | "busy" | "saved" | "hidden";

/** A finished, successful turn that did not just run a command (`/new`). */
/** Icon of a slash command in the menu. */
function SlashIcon({ name }: { name: string }) {
  const Icon = SLASH_ICONS[name] ?? Search;
  return <Icon />;
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
  jumpMessageId,
  jumpRequestId,
  onBack,
  control,
  onControl,
  onSwitchChat,
  showAgentRoute = false,
  sendWith = "enter",
  messageTimes = "minutes",
  liveChatSlots
}: {
  chatId: string;
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
    hostStarting
  } = useDaemon();
  const chat = chats.find((c) => c.id === chatId);
  const bot = bots.find((b) => b.id === chat?.bot_id);
  const view = bot ? botView(bot) : null;
  const { dto: providersDto } = useProviders();
  const { askEngine, applyEngine, fixBotEngine } = useEngineConsent();
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
      // A CLI that only works with its own tools: the one consent step, here, before saving.
      const answer = await askEngine(ref, bot.config);
      if (answer === null) {
        return;
      }
      const saved = await writeBotProfile(client, bot, (current) => {
        const draft = draftFromBot(current);
        draft.executor = parseModelRef(ref);
        return { name: current.name, config: configFromDraft(draft, current) };
      });
      await applyEngine(saved.id, answer);
      await refreshBots();
      toast.show(t("modelPicker.switched", { model: ref }), "success");
    } catch (e) {
      toast.show(botWriteMessage(e, t), "danger");
    }
  };

  // A failed turn: only the actions this view can carry out get a button.
  const runErrorFor = (message: string): HumanError => {
    const h = humanToolError(message);
    return h.action === "fixEngine" || h.action === "settings" ? h : { ...h, action: undefined };
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
  const [queue, updateQueue] = useChatQueue(chatId);
  // Up / Down recall this chat's earlier messages; the draft is kept (lib/composerHistory).
  const [nav, setNav] = useState<HistoryNav>(FRESH);
  const pendingCaret = useRef<number | null>(null);
  const [sending, setSending] = useState(false);
  /** The last user message being edited (inline), and a resend in flight. */
  const [editingId, setEditingId] = useState<string | null>(null);
  const [resending, setResending] = useState(false);
  const [files, setFiles] = useState<PendingFile[]>([]);
  const [dragOver, setDragOver] = useState(false);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  /** The run the daemon named for the message just sent, until the run list shows it. */
  const [ackedRunId, setAckedRunId] = useState<string | null>(null);
  /** Stop was pressed while the send was still unanswered: it is applied when the run is named. */
  const [stopPending, setStopPending] = useState(false);
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
  useLayoutEffect(() => {
    if (liveChatSlots?.composer) inputRef.current?.focus({ preventScroll: true });
  }, [liveChatSlots?.composer]);

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
  const mountedAt = useRef(Date.now());
  const switchedFor = useRef(new Set<string>());

  const chatRuns = useMemo(
    () => topLevelRuns(events, chatId),
    [events, chatId]
  );
  useAutoSpeak(chatId, chatRuns);
  const finishedCount = chatRuns.filter(
    (r) => r.finishedMs !== undefined
  ).length;

  // `/new` typed in the chat: the daemon starts a new chat and says so in its
  // reply; follow it (only for turns that finish while this view is open).
  useEffect(() => {
    for (const run of chatRuns) {
      const r = run.route?.route;
      if (
        r?.kind !== "command" ||
        r.name !== "new" ||
        run.finishedMs === undefined ||
        run.finishedMs < mountedAt.current ||
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
            toast.show(t("chat.newChatStarted"), "success");
            onSwitchChat(id);
          });
      }
    }
  }, [chatRuns, onSwitchChat, refreshChats, toast, t]);

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
  const serverRuns = runs.filter(
    (r) => r.chat_id === chatId && !r.parent_run_id
  );
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
  const activeRunId = serverLive?.id ?? eventLive?.id ?? ackedLive;
  activeRunIdRef.current = activeRunId;
  const pausedRunId = activeRunId ? null : (serverPaused?.id ?? null);
  const active = activeRunId !== null;
  /** The run bar (with its Stop) is up from the moment a message is sent, before any event. */
  const running = active || sending;

  const { seenUntilMs, dismiss: dismissUnread } = useChatRead(
    client?.baseUrl ?? "",
    chatId,
    chatActivityMs(runs, needs, chatId),
    messages
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
          runId: m.run_id
        });
      }
    }
    const lastFinished = [...chatRuns]
      .reverse()
      .find((r) => r.finishedMs !== undefined);
    for (const run of chatRuns) {
      if (replacedRuns.has(run.id)) {
        continue;
      }
      const chip = run.route ? routeChip(run.route, showAgentRoute) : null;
      if (run.route && chip && showRouteChip(chip.cheap, showAgentRoute)) {
        out.push({
          kind: "route",
          key: `${run.id}-route`,
          ts: run.startedMs,
          run
        });
      }
      const segs = groupRunItems(
        run.items,
        run.finishedMs !== undefined,
        run.startedMs
      );
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
      if (run.finishedMs !== undefined && run.stopped) {
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
      if (shouldProbeChanges(run, run === lastFinished)) {
        out.push({
          kind: "changes",
          key: `${run.id}-changes`,
          ts: run.finishedMs ?? run.startedMs,
          run
        });
      }
      if (
        run === lastFinished &&
        canOfferRemember(run) &&
        remember[run.id] !== "hidden"
      ) {
        out.push({
          kind: "remember",
          key: `${run.id}-remember`,
          ts: run.finishedMs ?? run.startedMs,
          run
        });
      }
      if (run === lastFinished && serverRuns.some(s => s.id === run.id && s.state === "succeeded" && !s.parent_run_id)) {
        out.push({ kind: "workflow", key: `${run.id}-workflow`, ts: run.finishedMs ?? run.startedMs, run });
      }
    }
    // Array.prototype.sort is stable: equal timestamps keep insertion order.
    const sorted = out.sort((a, b) => a.ts - b.ts);
    const unreadAt = seenUntilMs === null ? undefined : sorted.find((e) => e.kind !== "user" && e.ts > seenUntilMs)?.key;
    return sorted.flatMap((entry, index): Entry[] => {
      const marker: Entry[] = entry.key === unreadAt ? [{ kind: "unread", key: "unread", ts: entry.ts }] : [];
      return index === 0 || new Date(entry.ts).toDateString() !== new Date(sorted[index - 1].ts).toDateString()
        ? [{ kind: "day", key: `day-${entry.key}`, ts: entry.ts }, ...marker, entry] : [...marker, entry];
    });
  }, [messages, chatRuns, showAgentRoute, remember, seenUntilMs, serverRuns, jumpMessageId]);

  const turn = useMemo(() => lastTurn(messages), [messages]);
  const lastItem = entries[entries.length - 1];
  // The button is the stop button from the moment a message is sent (before any run event) while the box is empty.
  const readyFiles = files.filter((f) => f.state === "ready");
  const uploadingFiles = files.some((f) => f.state === "uploading");
  const sendMode = sendButtonMode({
    runActive: running,
    hasText: !sending && (text.trim().length > 0 || readyFiles.length > 0)
  });
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
  }, [chat?.id]);
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
      }
    });
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

  const dropFiles = (list: PendingFile[]) => {
    for (const f of list) {
      if (f.preview) {
        URL.revokeObjectURL(f.preview);
      }
    }
  };

  // A different chat starts with an empty tray.
  useEffect(
    () => () => {
      setFiles((prev) => {
        dropFiles(prev);
        return [];
      });
    },
    [chatId]
  );

  const removeFile = (key: string) => {
    setFiles((prev) => {
      dropFiles(prev.filter((f) => f.key === key));
      return prev.filter((f) => f.key !== key);
    });
  };

  const addFiles = (incoming: File[]) => {
    if (!client || incoming.length === 0) {
      return;
    }
    const taken = files.filter((f) => f.state !== "failed").length;
    const accepted = incoming.slice(0, room(taken, incoming.length));
    if (accepted.length < incoming.length) {
      toast.show(t("chat.attach.tooMany"), "info");
    }
    for (const file of accepted) {
      const key = crypto.randomUUID();
      const name = file.name || t("chat.attach.pastedName");
      const base: PendingFile = {
        key,
        name,
        size: file.size,
        state: "uploading",
        preview: file.type.startsWith("image/") ? URL.createObjectURL(file) : undefined
      };
      setFiles((prev) => [...prev, base]);
      const patch = (p: Partial<PendingFile>) =>
        setFiles((prev) => prev.map((f) => (f.key === key ? { ...f, ...p } : f)));
      if (file.size > MAX_ATTACHMENT_BYTES) {
        patch({ state: "failed", errorKey: "chat.attach.tooLarge" });
        continue;
      }
      client
        .uploadAttachment(chatId, file, name)
        .then((att) => patch({ state: "ready", id: att.id, kind: att.kind }))
        .catch((e: unknown) => {
          const err = e as { code?: string; status?: number };
          patch({ state: "failed", errorKey: uploadErrorKey(err.code, err.status) });
        });
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
    const attachmentIds = readyFiles.flatMap((f) => (f.id ? [f.id] : []));
    if (!client || (!body && attachmentIds.length === 0) || sending || uploadingFiles) {
      return;
    }
    if (isExportCommand(body)) {
      setText("");
      exportChat();
      return;
    }
    // "/help", "/settings" ...: the app opens the place itself; nothing is sent to the bot.
    const appCommand = attachmentIds.length === 0 ? appCommandFor(body) : null;
    if (appCommand) {
      setText("");
      window.dispatchEvent(new CustomEvent(APP_COMMAND_EVENT, { detail: appCommand.target }));
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
    // A message typed while the bot works joins the queue as a "Sirada." bubble (slash commands such
    // as /stop still go straight to the daemon). It is delivered when the run ends or by "Simdi gonder".
    if (active && !body.startsWith("/") && attachmentIds.length === 0) {
      updateQueue((q) => enqueue(q, body, crypto.randomUUID(), Date.now()));
      setText("");
      scrollToEnd();
      return;
    }
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
          return c.sendMessage(chatId, body, undefined, await ownerKey(c.baseUrl), attachmentIds);
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
        const victim = res.delivery === "stopped" ? null : (res.run_id ?? activeRunIdRef.current);
        if (victim) {
          cancelRun(victim);
        }
      }
      if (res.delivery === "stopped") {
        toast.show(t("chat.stopped"), "info");
      }
      setText("");
      setFiles((prev) => {
        dropFiles(prev);
        return [];
      });
      setNav(FRESH);
      scrollToEnd();
      await Promise.all([loadMessages(), refreshRuns()]);
    } catch (e) {
      stopWhileSending.current = false;
      setStopPending(false);
      setSendFailure(humanError(e));
    } finally {
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
    if (nativeActive) {
      requestStop("banner");
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
    // A second press while the first is still unconfirmed is the hard stop.
    const hard = appLedger().runEntries().some(
      (e) => e.id === activeRunId && (e.phase === "held" || e.phase === "cancel_requested" || e.phase === "failed")
    );
    cancelRun(activeRunId, hard ? { force: true } : undefined);
    try {
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
  const deliverQueued = async (message: { id: string; text: string; createdMs: number }, how: "queue" | "steer") => {
    if (!client) {
      return;
    }
    try {
      await client.sendMessage(chatId, message.text, how, await ownerKey(client.baseUrl));
      await Promise.all([loadMessages(), refreshRuns()]);
    } catch (e) {
      updateQueue((q) => putBack(q, message));
      setSendFailure(humanError(e));
    }
  };
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
  const wasActive = useRef(active);
  useEffect(() => {
    const was = wasActive.current;
    wasActive.current = active;
    if (was && !active && stoppedByUser.current) {
      stoppedByUser.current = false;
      return;
    }
    const { message, rest } = takeForRunEnd(queue, was, active);
    if (message) {
      updateQueue(() => rest);
      void deliverQueued(message, "queue");
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, queue]);
  const sendNow = (id: string) => {
    const { message, rest } = takeForNow(queue, id);
    if (message) {
      updateQueue(() => rest);
      void deliverQueued(message, "steer");
    }
  };
  useLedgerVersion();
  const ledgerRun = activeRunId
    ? appLedger().runEntries().find((e) => e.id === activeRunId)
    : undefined;
  const stopRecord = ledgerRun?.stopId
    ? appLedger().stopRecords().find((r) => r.id === ledgerRun.stopId)
    : undefined;
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
  const firstPending = pending.own[0];
  // The status line says WHAT the run waits for ("Waiting for your approval: Opening example.com").
  const approvalText = waitLost
    ? t("runStatus.waitLost")
    : firstPending?.tool
      ? t("runStatus.waiting", { step: toolSentence(t, firstPending.tool) })
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
  const decisions = useDecisionActions();
  const barBotName =
    control.bot && control.bot.botId !== chat?.bot_id
      ? control.bot.botName
      : undefined;
  const lastFailed = [...chatRuns].reverse()[0]?.error;
  const avatarState = awaitingApproval ? "waiting-approval" : active ? (lastItem?.kind === "chain" || (lastItem?.kind === "item" && lastItem.item.kind === "tool") ? "working" : "thinking") : lastFailed ? "error" : "idle";
  // Hands-free voice call (docs/design/voice-call-mode.md): the transcribed text goes out as an ordinary
  // message; approvals stay click-only (the call only pauses while the card waits).
  const voiceCall = useVoiceCall({
    botId: chat?.bot_id,
    runs: chatRuns,
    awaitingApproval,
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
  // The bot that answered: a mentioned bot's run has its own bot id.
  const guestBot = (botId: string | null | undefined) =>
    botId && botId !== chat?.bot_id ? bots.find((b) => b.id === botId) : undefined;
  const runGuest = (runId: string) => guestBot(runs.find((r) => r.id === runId)?.bot_id);
  const botAvatarFor = (decorative: boolean, size: "xs" | "sm" = "sm", guest?: Bot) => <BotAvatar bot={guest ? botView(guest) : (view ?? { id: chat?.bot_id ?? "unknown", name: "Bot" })} size={size} state={guest ? undefined : avatarState} decorative={decorative} />;
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

  const renderItem = (
    item: TimelineItem,
    runId: string
  ): ReactNode => {
    const run = events.runs[runId];
    const nested = run?.parentRunId !== undefined;
    switch (item.kind) {
      case "text":
        // The selected stored answer has its own focus target; avoid drawing its live copy too.
        if (messages.some((m) => m.id === jumpMessageId && m.run_id === runId && m.content === item.text)) return null;
        if (nested) {
          return <p className={ccStyles.nestedText}>{item.text}</p>;
        }
        return (
          <div className={styles.assistantRow} data-message-row="assistant">
            <div className={styles.assistantBubble}>
              {guestName(runGuest(runId))}
              <Markdown text={item.text} />
            </div>
            <MessageActions
              side="bot"
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
              <MessageInfo side="bot" ts={item.ts} runId={runId} run={run} runDto={runs.find((r) => r.id === runId)} />
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
        // Several plain approvals of one top-level run share one card; the other pending calls of
        // the batch render nothing, so each pending call is drawn exactly once. Cards the user
        // must read (payloads, secrets) stay separate.
        if (run && isBatchable(item)) {
          const rows = groupPending(
            run.items.filter((i): i is Extract<TimelineItem, { kind: "tool" }> => i.kind === "tool")
          );
          if (shouldBatch(rows)) {
            if (rows[0]?.items[0]?.callId !== item.callId) {
              return null;
            }
            return (
              <ApprovalBatchCard
                rows={rows}
                onDecide={async (callIds, decision) => {
                  for (const callId of callIds) {
                    await decisions.decide(callId)(decision, decision === "approve" ? "once" : undefined);
                  }
                }}
              />
            );
          }
        }
        return item.status === "awaiting_approval" ? (
          <ApprovalCard
            tool={item}
            serverName={decisions.serverName}
            onTrustFolder={decisions.trustFolder}
            onDecide={decisions.decide(item.callId)}
          />
        ) : (
          <ToolCard tool={item} />
        );
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
  const placeComposer = (composer: ReactNode) =>
    liveChatSlots?.composer ? createPortal(composer, liveChatSlots.composer) : composer;

  return (
    <div className={styles.root}>
      {/* v6: no title row (the chat is named in the sidebar); a back button only while a page to go back to exists. */}
      {onBack ? (
        <header className={styles.header}>
          <Button size="sm" variant="ghost" title={t("common.back")} aria-label={t("common.back")} onClick={onBack}>
            <ArrowLeft />
          </Button>
        </header>
      ) : null}

      <div className={styles.scrollWrap}>
      {findOpen ? (
        <ChatFindBar rootRef={timelineRef} contentKey={entries} onClose={() => setFindOpen(false)} />
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
      >
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
              <h1>{t("chat.emptyGreeting", { name: view?.name ?? "" })}</h1>
              <p className={styles.hint}>{t("chat.emptySub")}</p>
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
                  <div key={e.key} id={e.key} tabIndex={-1} className={styles.userRow} data-message-row="user">
                    {editingId === e.msgId && canResend ? (
                      <EditMessage
                        initial={e.text}
                        usedWriteTools={turn?.usedWriteTools === true}
                        busy={resending}
                        onSave={(value) => void resendTurn(value)}
                        onCancel={() => setEditingId(null)}
                      />
                    ) : (
                    <UserBubble text={e.text}>
                      <MessageFiles client={client} chatId={chatId} attachments={e.attachments ?? []} />
                      {splitMentions(e.text, mentionBots).map((part, i) =>
                        part.color ? (
                          <span key={i} className={styles.mention} style={{ "--mention-color": part.color } as CSSProperties}>{part.text}</span>
                        ) : (
                          part.text
                        )
                      )}
                    </UserBubble>
                    )}
                    <MessageActions
                      side="me"
                      meta={<MessageTime ts={e.ts} mode={messageTimes} />}
                    >
                      <CopyButton text={e.text} />
                      {canResend && turn?.user.id === e.msgId && editingId !== e.msgId ? (
                        <button
                          type="button"
                          className={msgStyles.copyBtn}
                          aria-label={t("chat.resend.edit")}
                          title={t("chat.resend.edit")}
                          data-testid="edit-message-button"
                          onClick={() => setEditingId(e.msgId)}
                        >
                          <Pencil aria-hidden="true" />
                        </button>
                      ) : null}
                      <MessageInfo side="me" ts={e.ts} source={e.source} />
                    </MessageActions>
                  </div>
                );
              case "superseded":
                return <SupersededView key={e.key} group={e.group} />;
              case "assistant":
                return (
                  <div key={e.key} id={e.key} tabIndex={-1} className={styles.assistantRow} data-message-row="assistant">
                    <div className={styles.assistantBubble}>
                      {lead ? guestName(guestBot(e.botId)) : null}
                      <Markdown text={e.text} />
                    </div>
                    <MessageActions
                      side="bot"
                      meta={<MessageTime ts={e.ts} mode={messageTimes} />}
                    >
                      <CopyButton text={e.text} />
                      <SpeakButton id={e.key} text={e.text} botId={e.botId ?? chat?.bot_id} />
                      <MessageInfo side="bot" ts={e.ts} runId={e.runId} run={e.runId ? events.runs[e.runId] : undefined} runDto={runs.find((r) => r.id === e.runId)} />
                    </MessageActions>
                  </div>
                );
              case "runStopped":
                return (
                  <div key={e.key} className={styles.runStopped} data-testid="run-stopped">
                    {t("chat.runStopped")}
                  </div>
                );
              case "runError":
                return (
                  <div key={e.key} className={styles.runError}>
                    <ErrorNotice compact error={runErrorFor(e.message)} onAction={onRunErrorAction} />
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
              case "workflow":
                return client ? <div key={e.key} className={styles.rememberRow}><WorkflowReview client={client} runId={e.run.id} canRemember={true} /></div> : null;
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
              case "remember": {
                const st = remember[e.run.id] ?? "idle";
                return (
                  <div key={e.key} className={styles.rememberRow}>
                    {st === "saved" ? (
                      <span className={styles.rememberDone} role="status">
                        {t("chat.remember.saved")}
                      </span>
                    ) : (
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={st === "busy"}
                        title={t("chat.remember.hint")}
                        onClick={() => void rememberRun(e.run.id)}
                        data-testid="remember-run"
                      >
                        <Bookmark />{t("chat.remember.button")}
                      </Button>
                    )}
                  </div>
                );
              }
              case "chain":
                return (
                  <div key={e.key} className={styles.chainRow}>
                    <ThinkingChain chain={e.chain} times={messageTimes} avatar={lead ? botAvatarFor(true, "xs", runGuest(e.runId)) : undefined} />
                  </div>
                );
              case "item":
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
          {canResend && turn?.answered ? (
            <RegenerateRow
              key={turn.user.id}
              usedWriteTools={turn.usedWriteTools}
              busy={resending}
              onRegenerate={() => void resendTurn()}
            />
          ) : null}
          <QueuedMessages
            queue={queue}
            onSendNow={sendNow}
            onRemove={(id) => updateQueue((q) => removeQueued(q, id))}
          />
        </div>
      </div>
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
      {placeComposer(
      <footer
        className={styles.composer}
        data-live-view={liveChatSlots?.composer ? "true" : undefined}
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
          if (dropped.length > 0) {
            e.preventDefault();
            addFiles(dropped);
          }
        }}
        data-dragover={dragOver ? "true" : undefined}
      >
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
        {sendFailure ? (
          <ErrorNotice
            compact
            error={sendFailure}
            busy={sending}
            onAction={(a) => (a === "restartService" ? void restartDaemon() : void send())}
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
          entries={pending.pinned}
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
        {!liveChatSlots?.composer ? <div className={styles.dockWrap}>
          <RunStatusLine
            model={barModel}
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
        </div> : null}
        <div className={styles.boxWrap}>
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
          <div className={styles.box}>
          <ComposerFiles files={files} onRemove={removeFile} />
          <div className={styles.inputWrap}>
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
            className={paintsMentions ? `${styles.input} ${styles.inputPainted}` : styles.input}
            onScroll={(e) => setInputScroll(e.currentTarget.scrollTop)}
            value={text}
            rows={1}
            role="combobox"
            aria-autocomplete="list"
            aria-expanded={menuOpen}
            aria-controls={menuOpen ? menuId : undefined}
            aria-activedescendant={
              menuOpen && menuItems.length > 0 ? `${menuId}-${menuActive}` : undefined
            }
            placeholder={
              awaitingApproval
                ? t("chat.placeholderWaiting")
                : active
                  ? t("chat.placeholderBusy")
                  : t("chat.placeholder")
            }
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
            <Button
              variant="ghost"
              size="sm"
              className={styles.plus}
              title={t("chat.addCommand")}
              aria-label={t("chat.addCommand")}
              onClick={() => {
                if (text === "") {
                  setText("/");
                  setCaret(1);
                }
                inputRef.current?.focus();
              }}
            >
              <Plus />
            </Button>
            <input
              ref={fileInputRef}
              type="file"
              multiple
              hidden
              data-testid="attach-input"
              onChange={(e) => {
                addFiles(Array.from(e.target.files ?? []));
                e.target.value = "";
              }}
            />
            <Button
              variant="ghost"
              size="sm"
              className={styles.plus}
              title={t("chat.attach.add")}
              aria-label={t("chat.attach.add")}
              disabled={!client}
              onClick={() => fileInputRef.current?.click()}
            >
              <Paperclip />
            </Button>
            <DictateButton
              className={styles.plus}
              onText={(dictated) => {
                setText((cur) => appendDictation(cur, dictated));
                inputRef.current?.focus();
              }}
            />
            {voiceCall.supported ? (
              <Button
                variant="ghost"
                size="sm"
                className={styles.plus}
                title={t("voice.call.start")}
                aria-label={t("voice.call.start")}
                disabled={!client || voiceCall.state.phase !== "idle"}
                data-testid="voice-call-start"
                onClick={voiceCall.start}
              >
                <Phone aria-hidden="true" />
              </Button>
            ) : null}
            {bot ? (
              <div className={styles.modelSlot}>
                <ModelPicker
                  groups={pickerGroups}
                  value={view?.executor ?? ""}
                  engines
                  disabled={!client}
                  thinking
                  placement="top-start"
                  onPick={(ref) => void pickModel(ref)}
                />
              </div>
            ) : null}
            {bot ? <ComputerTargetChip bot={bot} /> : null}
            <span className={styles.spacer} />
            <button
              type="button"
              className={styles.send}
              data-mode={sendMode}
              data-testid="send-button"
              disabled={sendButtonDisabled({ mode: sendMode, sending: sending || uploadingFiles, stopping: false })}
              title={sendMode === "stop" ? t(stopUi ? "chat.runBar.forceStop" : "chat.stop") : t("chat.send")}
              aria-label={sendMode === "stop" ? t(stopUi ? "chat.runBar.forceStop" : "chat.stop") : t("chat.send")}
              onClick={() => (sendMode === "stop" ? void stopRun() : void send())}
            >
              {sendMode === "stop" ? <span className={styles.stopMark} aria-hidden="true" /> : <ArrowUp aria-hidden="true" />}
            </button>
          </div>
          </div>
        </div>
      </footer>
      )}
      <VoiceCallPanel call={voiceCall} bot={view ?? { id: chat?.bot_id ?? "unknown", name: "Bot" }} />
    </div>
  );
}
