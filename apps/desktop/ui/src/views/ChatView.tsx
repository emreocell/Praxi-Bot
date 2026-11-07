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
import type { Message, SendMode } from "../api/types";
import {
  ApprovalCard,
  NoticeLine,
  QuestionCard,
  SubagentCard,
  TodoCard,
  ToolCard
} from "../components/ChatCards";
import { botView } from "../lib/botProfile";
import { useDaemon } from "../state/DaemonContext";
import {
  assignChildRuns,
  itemTs,
  type RunState,
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
  | { kind: "runError"; key: string; ts: number; message: string };

export function ChatView({
  chatId,
  onBack
}: {
  chatId: string;
  onBack: () => void;
}) {
  const { t } = useTranslation();
  const { client, chats, bots, events, runs, recordAnswer, refreshRuns } =
    useDaemon();
  const chat = chats.find((c) => c.id === chatId);
  const bot = bots.find((b) => b.id === chat?.bot_id);
  const view = bot ? botView(bot) : null;

  const [messages, setMessages] = useState<Message[]>([]);
  const [text, setText] = useState("");
  const [mode, setMode] = useState<SendMode>("queue");
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const scrollerRef = useRef<HTMLDivElement | null>(null);
  const stickToBottom = useRef(true);

  const chatRuns = useMemo(
    () => topLevelRuns(events, chatId),
    [events, chatId]
  );
  const finishedCount = chatRuns.filter(
    (r) => r.finishedMs !== undefined
  ).length;

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
    for (const run of chatRuns) {
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
    }
    // Array.prototype.sort is stable: equal timestamps keep insertion order.
    return out.sort((a, b) => a.ts - b.ts);
  }, [messages, chatRuns]);

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

  const send = async () => {
    const body = text.trim();
    if (!client || !body || sending) {
      return;
    }
    setSending(true);
    setError(null);
    try {
      await client.sendMessage(chatId, body, active ? mode : undefined);
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
            <div className={styles.assistantBubble}>{item.text}</div>
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
                    <div className={styles.assistantBubble}>{e.text}</div>
                  </div>
                );
              case "runError":
                return (
                  <p key={e.key} className={styles.runError} role="alert">
                    {t("chat.runFailed", { message: e.message })}
                  </p>
                );
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
        <div className={styles.inputRow}>
          <textarea
            className={styles.input}
            value={text}
            rows={2}
            placeholder={
              active ? t("chat.placeholderBusy") : t("chat.placeholder")
            }
            aria-label={t("chat.placeholder")}
            onChange={(e) => setText(e.target.value)}
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
