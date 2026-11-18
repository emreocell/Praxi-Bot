import { isLocalBaseUrl } from "../lib/liveViewState";
import {
  ArrowDown,
  ArrowLeft,
  ArrowUp,
  Bookmark,
  Plus,
  Square,
  Terminal,
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
import { ownerKey } from "../lib/ownerKey";
import { BotAvatar, Button } from "ui";
import { isMockMode } from "../api/mockMode";
import { ApiError } from "../api/client";
import type { Bot, Message, SendMode } from "../api/types";
import {
  ApprovalCard,
  NoticeLine,
  QuestionCard,
  SubagentCard,
  TodoCard,
  ToolCard
} from "../components/ChatCards";
import { Markdown } from "../components/Markdown";
import { MessageTime } from "../components/MessageTime";
import { ModelPicker } from "../components/ModelPicker";
import { RunBar } from "../components/RunBar";
import { RunChangesGate } from "../components/RunChangesGate";
import { ThinkingChain } from "../components/ThinkingChain";
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
import { botWriteMessage, writeBotProfile } from "../lib/botWrite";
import { splitMentions } from "../lib/mentionText";
import { dayHeader, type MessageTimes } from "../lib/messageTime";
import { modelGroups } from "../lib/modelPicker";
import { type ChatRunPhase, runBarModel, runStopUi, STOP_CONFIRM_MS } from "../lib/runBar";
import {
  giveBackControl,
  type ControlSnapshot,
  pauseBotRun,
  resumeBotRun,
  takeTheWheel
} from "../overlay/events";
import { describeTool, stepPhase } from "../lib/toolLabels";
import { brandIcon } from "../lib/brandIcons";
import { useLiveView } from "../state/LiveViewContext";
import { type Chain, chainOrderTs, groupRunItems } from "../lib/thinkingChain";
import { type SendWith, shouldSend } from "../lib/settingsPrefs";
import { rank } from "../lib/fuzzy";
import {
  applyCompletion,
  detectTrigger,
  mentionHandle,
  SLASH_COMMANDS,
  APP_COMMAND_EVENT,
  APP_COMMANDS,
  appCommandFor
} from "../lib/slash";
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
import { type ErrorAction, type HumanError, humanError, humanLine, humanToolError } from "../lib/humanError";
import { ComputerTargetChip } from "../components/ComputerTargetChip";
import { ErrorNotice } from "../components/ErrorNotice";

const LIVE_STATES = new Set(["pending", "running", "waiting_approval"]);

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

const SLASH_ICONS: Record<string, typeof Plus> = { new: Plus, stop: Square };

interface MenuBot {
  id: string;
  name: string;
  color?: string;
  avatar?: string | null;
}

type Entry =
  | { kind: "day"; key: string; ts: number }
  | { kind: "user"; key: string; ts: number; text: string }
  | { kind: "assistant"; key: string; ts: number; text: string; live: boolean; botId?: string | null }
  | { kind: "item"; key: string; ts: number; item: TimelineItem; runId: string }
  | { kind: "chain"; key: string; ts: number; chain: Chain; runId: string }
  | { kind: "runError"; key: string; ts: number; message: string }
  | { kind: "runStopped"; key: string; ts: number }
  | { kind: "route"; key: string; ts: number; run: RunState }
  | { kind: "changes"; key: string; ts: number; run: RunState }
  | { kind: "remember"; key: string; ts: number; run: RunState };

/** `idle` = offer the button; `saved` = done; `hidden` = the daemon said no (409). */
type RememberState = "idle" | "busy" | "saved" | "hidden";

/** A finished, successful turn that did not just run a command (`/new`). */
/** Icon of a slash command in the menu. */
function SlashIcon({ name }: { name: string }) {
  const Icon = SLASH_ICONS[name] ?? Terminal;
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
  onBack,
  control,
  onControl,
  onSwitchChat,
  showAgentRoute = false,
  sendWith = "enter",
  messageTimes = "minutes"
}: {
  chatId: string;
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
}) {
  const { t, i18n } = useTranslation();
  const toast = useToast();
  const {
    client,
    chats,
    bots,
    events,
    runs,
    recordAnswer,
    refreshRuns,
    refreshChats,
    refreshBots,
    restartDaemon,
    home
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
  const [mode, setMode] = useState<SendMode>("queue");
  const [sending, setSending] = useState(false);
  /** The run the daemon named for the message just sent, until the run list shows it. */
  const [ackedRunId, setAckedRunId] = useState<string | null>(null);
  /** Stop was pressed while the send was still unanswered: it is applied when the run is named. */
  const [stopPending, setStopPending] = useState(false);
  const stopWhileSending = useRef(false);
  const activeRunIdRef = useRef<string | null>(null);
  const [caret, setCaret] = useState(0);
  const [menuIdx, setMenuIdx] = useState(0);
  const [menuDismissed, setMenuDismissed] = useState<string | null>(null);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);
  const [inputScroll, setInputScroll] = useState(0);
  // One colour source for mentions: the bot's own colour, for typed text and for sent messages alike.
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
  const stickToBottom = useRef(true);
  const [remember, setRemember] = useState<Record<string, RememberState>>({});
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
  const serverLive = [...serverRuns].reverse().find((r) => LIVE_STATES.has(r.state));
  const serverPaused = [...serverRuns].reverse().find((r) => r.state === "paused");
  const eventLive: RunState | undefined = [...chatRuns]
    .reverse()
    .find(
      (r) =>
        r.finishedMs === undefined && !serverRuns.some((s) => s.id === r.id)
    );
  const ackedLive =
    ackedRunId !== null &&
    !serverRuns.some((r) => r.id === ackedRunId && !LIVE_STATES.has(r.state)) &&
    !chatRuns.some((r) => r.id === ackedRunId && r.finishedMs !== undefined)
      ? ackedRunId
      : null;
  const activeRunId = serverLive?.id ?? eventLive?.id ?? ackedLive;
  activeRunIdRef.current = activeRunId;
  const pausedRunId = activeRunId ? null : (serverPaused?.id ?? null);
  const active = activeRunId !== null;
  /** The run bar (with its Stop) is up from the moment a message is sent, before any event. */
  const running = active || sending;

  const entries = useMemo(() => {
    const out: Entry[] = [];
    const known = new Set(chatRuns.map((r) => r.id));
    for (const m of messages) {
      if (m.role === "user") {
        out.push({
          kind: "user",
          key: `m-${m.id}`,
          ts: m.created_at_ms,
          text: m.content
        });
      } else if (
        m.role === "assistant" &&
        (!m.run_id || !known.has(m.run_id))
      ) {
        out.push({
          kind: "assistant",
          key: `m-${m.id}`,
          ts: m.created_at_ms,
          text: m.content,
          live: false,
          botId: m.bot_id
        });
      }
    }
    const lastFinished = [...chatRuns]
      .reverse()
      .find((r) => r.finishedMs !== undefined);
    for (const run of chatRuns) {
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
    }
    // Array.prototype.sort is stable: equal timestamps keep insertion order.
    const sorted = out.sort((a, b) => a.ts - b.ts);
    return sorted.flatMap((entry, index): Entry[] => index === 0 || new Date(entry.ts).toDateString() !== new Date(sorted[index - 1].ts).toDateString()
      ? [{ kind: "day", key: `day-${entry.key}`, ts: entry.ts }, entry] : [entry]);
  }, [messages, chatRuns, showAgentRoute, remember]);

  const lastItem = entries[entries.length - 1];
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
    if (triggerKind === "slash") {
      return rank([...SLASH_COMMANDS, ...APP_COMMANDS], triggerQuery, (c) => ({
        text: c.name,
        keywords: [t(c.i18nKey)]
      })).map((r) => ({
        id: r.item.name,
        label: `/${r.item.name}`,
        detail: t(r.item.i18nKey),
        insert: r.item.name,
        bot: undefined as MenuBot | undefined
      }));
    }
    return rank(bots, triggerQuery, (b) => ({
      text: mentionHandle(botView(b).name)
    })).map((r) => {
      const v = botView(r.item);
      return {
        id: r.item.id,
        label: `@${mentionHandle(v.name)}`,
        detail: v.executor.split("/").pop() ?? "",
        insert: mentionHandle(v.name),
        bot: { id: r.item.id, name: v.name, color: v.color, avatar: v.avatar } as
          | MenuBot
          | undefined
      };
    });
  }, [triggerKind, triggerQuery, dismissKey, menuDismissed, bots, t]);
  // once the trigger is gone a later one (even an identical one) starts fresh
  useEffect(() => {
    if (dismissKey === null) {
      setMenuDismissed(null);
    }
  }, [dismissKey]);
  const menuId = useId();
  const menuRef = useRef<HTMLDivElement | null>(null);
  const menuOpen = menuItems.length > 0;
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

  const send = async () => {
    const body = text.trim();
    if (!client || !body || sending) {
      return;
    }
    // "/help", "/settings" ...: the app opens the place itself; nothing is sent to the bot.
    const appCommand = appCommandFor(body);
    if (appCommand) {
      setText("");
      window.dispatchEvent(new CustomEvent(APP_COMMAND_EVENT, { detail: appCommand.target }));
      return;
    }
    setSending(true);
    setError(null);
    setSendFailure(null);
    try {
      // "@Other do x": the daemon lets Other answer in THIS chat.
      const res = await client.sendMessage(
        chatId,
        body,
        active ? mode : undefined,
        await ownerKey(client.baseUrl)
      );
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
    if (menuOpen && !e.nativeEvent.isComposing) {
      const n = menuItems.length;
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setMenuIdx((menuActive + 1) % n);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setMenuIdx((menuActive - 1 + n) % n);
        return;
      }
      if (e.key === "Enter" || e.key === "Tab") {
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

  const { openViewer } = useLiveView();
  const nativeActive = control.mode !== "idle";
  const stop = async () => {
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

  const awaitingApproval = serverLive?.state === "waiting_approval" || chatRuns.some(run => run.finishedMs === undefined && run.items.some(item => item.kind === "tool" && item.status === "awaiting_approval"));
  const chatPhase: ChatRunPhase = running
    ? awaitingApproval
      ? "waiting_approval"
      : "live"
    : pausedRunId
      ? "paused"
      : "none";
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
  const barBotName =
    control.bot && control.bot.botId !== chat?.bot_id
      ? control.bot.botName
      : undefined;
  const lastFailed = [...chatRuns].reverse()[0]?.error;
  const avatarState = awaitingApproval ? "waiting-approval" : active ? (lastItem?.kind === "chain" || (lastItem?.kind === "item" && lastItem.item.kind === "tool") ? "working" : "thinking") : lastFailed ? "error" : "idle";
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
        if (nested) {
          return <p className={ccStyles.nestedText}>{item.text}</p>;
        }
        return (
          <div className={styles.assistantRow}>
            <div className={styles.assistantBubble}>
              {guestName(runGuest(runId))}
              <Markdown text={item.text} />
            </div>
            <MessageTime
              ts={item.ts}
              mode={messageTimes}
              finishedTs={run?.finishedMs}
            />
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
        return item.status === "awaiting_approval" ? (
          <ApprovalCard
            tool={item}
            serverName={home.kind === "host" ? home.name : null}
            onTrustFolder={client && isLocalBaseUrl(client.baseUrl) ? async (folder) => {
              const key = await ownerKey(client.baseUrl);
              if (!key) throw new Error(t("folderTrust.ownerRequired"));
              await client.setFolderTrust(folder, true, key);
            } : undefined}
            onDecide={async (decision, scope, reason) => {
              if (!client) {
                return;
              }
              // A standing choice ("always for this chat") carries the owner's authority, which
              // only the desktop app's own window holds; a plain approval needs no key.
              const standing = decision === "approve" && scope !== undefined && scope !== "once" && scope !== "run_control";
              const key = !standing
                ? undefined
                : await ownerKey(client.baseUrl);
              await client.decideApproval(
                item.callId,
                decision,
                scope,
                reason,
                key
              );
            }}
          />
        ) : (
          <ToolCard tool={item} />
        );
      }
      case "question":
        return (
          <QuestionCard
            q={item}
            onAnswer={async (answer) => {
              if (!client) {
                return;
              }
              await client.answerQuestion(item.callId, answer);
              recordAnswer(runId, item.callId, answer);
            }}
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
              !prev || prev.kind === "user" || prev.kind === "day";
            switch (e.kind) {
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
                  <div key={e.key} className={styles.userRow}>
                    <MessageTime ts={e.ts} mode={messageTimes} />
                    <div className={styles.userBubble}>
                      {splitMentions(e.text, mentionBots).map((part, i) =>
                        part.color ? (
                          <span key={i} className={styles.mention} style={{ "--mention-color": part.color } as CSSProperties}>{part.text}</span>
                        ) : (
                          part.text
                        )
                      )}
                    </div>
                  </div>
                );
              case "assistant":
                return (
                  <div key={e.key} className={styles.assistantRow}>
                    <div className={styles.assistantBubble}>
                      {lead ? guestName(guestBot(e.botId)) : null}
                      <Markdown text={e.text} />
                    </div>
                    <MessageTime ts={e.ts} mode={messageTimes} />
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

      <footer className={styles.composer}>
        {error ? (
          <p className={styles.error} role="alert">
            {error}
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
        <div className={styles.dockWrap}>
          <RunBar
            model={barModel}
            botName={barBotName}
            detail={barDetail}
            startedMs={barStartedMs}
            runStop={stopUi}
            humanDriving={control.mode === "human_driving"}
            onStop={() => void stop()}
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
            onBigView={() => openViewer()}
          />
        </div>
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
              <ul
                className={styles.menuList}
                id={menuId}
                role="listbox"
                aria-label={
                  triggerKind === "slash"
                    ? t("chat.slash.menu")
                    : t("chat.slash.mentionMenu")
                }
              >
                {menuItems.map((m, i) => (
                  <li
                    key={m.id}
                    id={`${menuId}-${i}`}
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
                    <span className={styles.menuLabel}>{m.label}</span>
                    {m.detail ? (
                      <span className={styles.menuDetail}>{m.detail}</span>
                    ) : null}
                  </li>
                ))}
              </ul>
              <p className={styles.menuKeys} aria-hidden="true">
                <kbd>↑</kbd>
                <kbd>↓</kbd> {t("chat.menu.navigate")}
                <kbd>↵</kbd> {t("chat.menu.select")}
                <kbd>esc</kbd> {t("chat.menu.close")}
              </p>
            </div>
          ) : null}
          <div className={styles.box}>
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
              menuOpen ? `${menuId}-${menuActive}` : undefined
            }
            placeholder={
              active ? t("chat.placeholderBusy") : t("chat.placeholder")
            }
            aria-label={t("chat.placeholder")}
            onChange={(e) => {
              setText(e.target.value);
              setCaret(e.target.selectionStart);
              setMenuIdx(0);
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
            {bot ? (
              <div>
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
            {active ? (
              <div
                className={styles.mode}
                role="radiogroup"
                aria-label={t("chat.modeLabel")}
                onKeyDown={(e) => {
                  if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
                    e.preventDefault();
                    const next: SendMode = mode === "queue" ? "steer" : "queue";
                    setMode(next);
                    (e.currentTarget.querySelector(`[data-mode="${next}"]`) as HTMLElement | null)?.focus();
                  }
                }}
              >
                {(["queue", "steer"] as const).map((m) => (
                  <button
                    key={m}
                    type="button"
                    role="radio"
                    data-mode={m}
                    aria-checked={mode === m}
                    tabIndex={mode === m ? 0 : -1}
                    className={styles.modeOption}
                    title={t(`chat.mode.${m}Hint`)}
                    onClick={() => setMode(m)}
                  >
                    {t(`chat.mode.${m}`)}
                  </button>
                ))}
              </div>
            ) : null}
            <span className={styles.spacer} />
            <Button
              variant="primary"
              className={styles.send}
              disabled={sending || !text.trim()}
              title={t("chat.send")}
              aria-label={t("chat.send")}
              onClick={() => void send()}
            >
              <ArrowUp />
            </Button>
          </div>
          </div>
        </div>
      </footer>
    </div>
  );
}
