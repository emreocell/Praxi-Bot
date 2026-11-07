import {
  type KeyboardEvent,
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState
} from "react";
import { useTranslation } from "react-i18next";
import { Avatar, Button } from "ui";
import { ApiError } from "../api/client";
import type { Message, SendMode } from "../api/types";
import {
  ApprovalCard,
  NoticeLine,
  QuestionCard,
  SubagentCard,
  TodoCard,
  ToolCard
} from "../components/ChatCards";
import { Markdown } from "../components/Markdown";
import { useToast } from "../components/Toast";
import { botView } from "../lib/botProfile";
import { rank } from "../lib/fuzzy";
import {
  applyCompletion,
  detectTrigger,
  mentionHandle,
  parseLeadingMention,
  SLASH_COMMANDS
} from "../lib/slash";
import {
  newChatIdFromText,
  routeChip,
  routeHoverValues
} from "../lib/route";
import { useDaemon } from "../state/DaemonContext";
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

const LIVE_STATES = new Set(["pending", "running", "waiting_approval"]);

type Entry =
  | { kind: "user"; key: string; ts: number; text: string }
  | { kind: "assistant"; key: string; ts: number; text: string; live: boolean }
  | { kind: "item"; key: string; ts: number; item: TimelineItem; runId: string }
  | { kind: "runError"; key: string; ts: number; message: string }
  | { kind: "route"; key: string; ts: number; run: RunState }
  | { kind: "remember"; key: string; ts: number; run: RunState };

/** `idle` = offer the button; `saved` = done; `hidden` = the daemon said no (409). */
type RememberState = "idle" | "busy" | "saved" | "hidden";

/** A finished, successful turn that did not just run a command (`/new`). */
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
  onSwitchChat,
  showAgentRoute = false
}: {
  chatId: string;
  onBack: () => void;
  /** Open another chat (after a `/new` turn created one). */
  onSwitchChat?: (chatId: string) => void;
  /** Debug: also show the chip for a plain `Agent` decision at K4. */
  showAgentRoute?: boolean;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const {
    client,
    chats,
    bots,
    events,
    runs,
    recordAnswer,
    refreshRuns,
    refreshChats
  } = useDaemon();
  const chat = chats.find((c) => c.id === chatId);
  const bot = bots.find((b) => b.id === chat?.bot_id);
  const view = bot ? botView(bot) : null;

  const [messages, setMessages] = useState<Message[]>([]);
  const [text, setText] = useState("");
  const [mode, setMode] = useState<SendMode>("queue");
  const [sending, setSending] = useState(false);
  const [caret, setCaret] = useState(0);
  const [menuIdx, setMenuIdx] = useState(0);
  const [menuDismissed, setMenuDismissed] = useState<string | null>(null);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);
  const [error, setError] = useState<string | null>(null);
  const scrollerRef = useRef<HTMLDivElement | null>(null);
  const stickToBottom = useRef(true);
  const [remember, setRemember] = useState<Record<string, RememberState>>({});
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
      setError(e instanceof Error ? e.message : String(e));
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
  const activeRunId = serverLive?.id ?? eventLive?.id ?? null;
  const pausedRunId = activeRunId ? null : (serverPaused?.id ?? null);
  const active = activeRunId !== null;

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
          live: false
        });
      }
    }
    const lastFinished = [...chatRuns]
      .reverse()
      .find((r) => r.finishedMs !== undefined);
    for (const run of chatRuns) {
      if (run.route && routeChip(run.route, showAgentRoute)) {
        out.push({
          kind: "route",
          key: `${run.id}-route`,
          ts: run.startedMs,
          run
        });
      }
      for (const item of run.items) {
        out.push({
          kind: "item",
          key: `${run.id}-${item.key}`,
          ts: itemTs(item),
          item,
          runId: run.id
        });
      }
      if (run.finishedMs !== undefined && run.error) {
        out.push({
          kind: "runError",
          key: `${run.id}-error`,
          ts: run.finishedMs,
          message: run.error
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
    return out.sort((a, b) => a.ts - b.ts);
  }, [messages, chatRuns, showAgentRoute, remember]);

  const lastItem = entries[entries.length - 1];
  const streaming =
    active && lastItem?.kind === "item" && lastItem.item.kind === "text";

  // ---- keep the view at the bottom unless the user scrolled up
  const lastLen =
    lastItem?.kind === "item" && lastItem.item.kind === "text"
      ? lastItem.item.text.length
      : 0;
  useEffect(() => {
    if (stickToBottom.current) {
      const el = scrollerRef.current;
      if (el) {
        el.scrollTop = el.scrollHeight;
      }
    }
  }, [entries.length, lastLen]);

  const onScroll = () => {
    const el = scrollerRef.current;
    if (el) {
      stickToBottom.current =
        el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    }
  };

  // ---- "/" commands and "@" bot mentions in the composer
  const trigger = detectTrigger(text, caret);
  const triggerKey = trigger ? `${trigger.kind}:${trigger.start}` : null;
  const triggerKind = trigger?.kind ?? null;
  const triggerQuery = trigger?.query ?? "";
  const menuItems = useMemo(() => {
    if (!triggerKind || triggerKey === menuDismissed) {
      return [];
    }
    if (triggerKind === "slash") {
      return rank(SLASH_COMMANDS, triggerQuery, (c) => ({
        text: c.name,
        keywords: [t(c.i18nKey)]
      })).map((r) => ({
        id: r.item.name,
        label: `/${r.item.name}`,
        detail: t(r.item.i18nKey),
        insert: r.item.name
      }));
    }
    return rank(bots, triggerQuery, (b) => ({
      text: mentionHandle(botView(b).name)
    })).map((r) => ({
      id: r.item.id,
      label: `@${mentionHandle(botView(r.item).name)}`,
      detail: "",
      insert: mentionHandle(botView(r.item).name)
    }));
  }, [triggerKind, triggerQuery, triggerKey, menuDismissed, bots, t]);
  const menuOpen = menuItems.length > 0;
  const menuActive = Math.min(menuIdx, Math.max(menuItems.length - 1, 0));

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

  const send = async () => {
    let body = text.trim();
    if (!client || !body || sending) {
      return;
    }
    setSending(true);
    setError(null);
    try {
      // "@Other do x" starts a chat with that bot; "@ThisBot do x" just sends.
      const mention = parseLeadingMention(
        body,
        bots.map((b) => ({ id: b.id, name: botView(b).name }))
      );
      if (mention && mention.bot.id !== chat?.bot_id) {
        const created = await client.createChat(mention.bot.id);
        await client.sendMessage(created.id, mention.rest);
        await refreshChats().catch(() => {});
        setText("");
        onSwitchChat?.(created.id);
        return;
      }
      if (mention) {
        body = mention.rest;
      }
      const res = await client.sendMessage(
        chatId,
        body,
        active ? mode : undefined
      );
      if (res.delivery === "stopped") {
        toast.show(t("chat.stopped"), "info");
      }
      setText("");
      stickToBottom.current = true;
      await Promise.all([loadMessages(), refreshRuns()]);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSending(false);
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
        setMenuDismissed(triggerKey);
        return;
      }
    }
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void send();
    }
  };

  const stop = async () => {
    if (!client || !activeRunId) {
      return;
    }
    try {
      await client.cancelRun(activeRunId);
      await refreshRuns();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
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
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const botAvatar = (
    <Avatar
      name={view?.name ?? "?"}
      src={view?.avatar}
      color={view?.color}
      size="sm"
    />
  );

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
            message: e instanceof Error ? e.message : String(e)
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
    await client.cancelRun(childRunId);
    await refreshRuns();
  };

  const renderItem = (item: TimelineItem, runId: string): ReactNode => {
    const run = events.runs[runId];
    const nested = run?.parentRunId !== undefined;
    switch (item.kind) {
      case "text":
        if (nested) {
          return <p className={ccStyles.nestedText}>{item.text}</p>;
        }
        return (
          <div className={styles.assistantRow}>
            {botAvatar}
            <div className={styles.assistantBubble}>
              <Markdown text={item.text} />
            </div>
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
            onDecide={async (decision, scope, reason) => {
              if (!client) {
                return;
              }
              await client.decideApproval(
                item.callId,
                decision,
                scope,
                reason
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
        <Button variant="secondary" onClick={onBack}>
          {t("common.back")}
        </Button>
      </div>
    );
  }

  return (
    <div className={styles.root}>
      <header className={styles.header}>
        <Button size="sm" variant="ghost" onClick={onBack}>
          {t("common.back")}
        </Button>
        {botAvatar}
        <div className={styles.headerText}>
          <span className={styles.title}>{chat.title}</span>
          <span className={styles.subtitle}>{view?.name}</span>
        </div>
        {active ? (
          <Button size="sm" variant="danger" onClick={() => void stop()}>
            {t("chat.stop")}
          </Button>
        ) : pausedRunId ? (
          <Button size="sm" variant="primary" onClick={() => void resume()}>
            {t("chat.resume")}
          </Button>
        ) : null}
      </header>

      <div
        className={styles.scroller}
        ref={scrollerRef}
        onScroll={onScroll}
        data-testid="chat-timeline"
      >
        <div className={styles.timeline}>
          {entries.length === 0 ? (
            <p className={styles.hint}>{t("chat.emptyHint")}</p>
          ) : null}
          {entries.map((e) => {
            switch (e.kind) {
              case "user":
                return (
                  <div key={e.key} className={styles.userRow}>
                    <div className={styles.userBubble}>{e.text}</div>
                  </div>
                );
              case "assistant":
                return (
                  <div key={e.key} className={styles.assistantRow}>
                    {botAvatar}
                    <div className={styles.assistantBubble}>
                      <Markdown text={e.text} />
                    </div>
                  </div>
                );
              case "runError":
                return (
                  <p key={e.key} className={styles.runError} role="alert">
                    {t("chat.runFailed", { message: e.message })}
                  </p>
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
                        <svg
                          className={styles.routeIcon}
                          viewBox="0 0 16 16"
                          aria-hidden="true"
                        >
                          <path
                            d="M9 1 3 9h4l-1 6 6-8H8z"
                            fill="currentColor"
                          />
                        </svg>
                      ) : null}
                      <span className={styles.routeLabel}>{label}</span>
                    </span>
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
                        {t("chat.remember.button")}
                      </Button>
                    )}
                  </div>
                );
              }
              case "item":
                return (
                  <div key={e.key} className={styles.itemRow}>
                    {renderItem(e.item, e.runId)}
                  </div>
                );
            }
          })}
          {active && !streaming ? (
            <p className={styles.working} role="status">
              <span className={styles.workingDot} aria-hidden="true" />
              {t("chat.working")}
            </p>
          ) : null}
        </div>
      </div>

      <footer className={styles.composer}>
        {error ? (
          <p className={styles.error} role="alert">
            {error}
          </p>
        ) : null}
        {active ? (
          <div
            className={styles.modeRow}
            role="radiogroup"
            aria-label={t("chat.modeLabel")}
          >
            <span className={styles.modeLabel}>{t("chat.modeLabel")}</span>
            {(["queue", "steer"] as const).map((m) => (
              <Button
                key={m}
                size="sm"
                variant="secondary"
                active={mode === m}
                role="radio"
                aria-checked={mode === m}
                title={t(`chat.mode.${m}Hint`)}
                onClick={() => setMode(m)}
              >
                {t(`chat.mode.${m}`)}
              </Button>
            ))}
          </div>
        ) : null}
        {menuOpen ? (
          <ul
            className={styles.menu}
            role="listbox"
            aria-label={
              triggerKind === "slash"
                ? t("chat.slash.menu")
                : t("chat.slash.mentionMenu")
            }
            data-testid="composer-menu"
          >
            {menuItems.map((m, i) => (
              <li
                key={m.id}
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
                <span className={styles.menuLabel}>{m.label}</span>
                {m.detail ? (
                  <span className={styles.menuDetail}>{m.detail}</span>
                ) : null}
              </li>
            ))}
          </ul>
        ) : null}
        <div className={styles.inputRow}>
          <textarea
            ref={inputRef}
            className={styles.input}
            value={text}
            rows={2}
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
          />
          <Button
            variant="primary"
            disabled={sending || !text.trim()}
            onClick={() => void send()}
          >
            {t("chat.send")}
          </Button>
        </div>
      </footer>
    </div>
  );
}
