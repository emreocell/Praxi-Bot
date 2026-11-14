import { type ReactNode, useEffect, useState } from "react";
import { CalendarClock, Clock, ShieldAlert } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { Scope } from "../api/types";
import {
  isOutboundSend,
  isSubagentTool,
  type NoticeItem,
  type QuestionItem,
  type RunState,
  subagentStatus,
  type TimelineItem,
  type TodoListItem,
  type ToolItem
} from "../state/timeline";
import {
  formatFiring,
  scheduleDetails,
  type ScheduleApprovalDetails
} from "../lib/approvalDetails";
import { brandIcon } from "../lib/brandIcons";
import { describeTool } from "../lib/toolLabels";
import { StepIcon } from "./StepIcon";
import formStyles from "./Form.module.css";
import styles from "./ChatCards.module.css";

const brandName = (name: string): string | null => {
  const b = brandIcon(name);
  return b.known ? b.name : null;
};

/** One-line summary of a tool input (first few `key: value` pairs). */
export function summarizeInput(input: unknown, max = 90): string {
  if (input === null || input === undefined) {
    return "";
  }
  if (typeof input !== "object") {
    return String(input).slice(0, max);
  }
  const parts = Object.entries(input as Record<string, unknown>).map(
    ([k, v]) =>
      `${k}: ${typeof v === "string" ? v : JSON.stringify(v)}`.replace(
        /\s+/g,
        " "
      )
  );
  const joined = parts.join(", ");
  return joined.length > max ? `${joined.slice(0, max - 1)}…` : joined;
}

function formatDuration(ms: number): string {
  if (ms < 1000) {
    return `${Math.max(ms, 0)} ms`;
  }
  return `${(ms / 1000).toFixed(1)} s`;
}

function prettyJson(v: unknown): string {
  try {
    return JSON.stringify(v, null, 2) ?? "";
  } catch {
    return String(v);
  }
}

// ---------------------------------------------------------------- tool card

export function ToolCard({ tool }: { tool: ToolItem }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const subagent = isSubagentTool(tool.name);
  // The daemon measures the tool alone; without it (old logs) fall back to
  // request-to-completion, which includes any approval wait.
  const duration =
    tool.durationMs !== undefined
      ? formatDuration(tool.durationMs)
      : tool.completedMs !== undefined
        ? formatDuration(tool.completedMs - tool.requestedMs)
        : null;
  const classes = [
    styles.card,
    subagent ? styles.subagent : "",
    tool.status === "error" || tool.status === "denied" ? styles.cardBad : ""
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div className={classes} data-testid="tool-card" data-status={tool.status}>
      <button
        type="button"
        className={styles.cardHeader}
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
      >
        <span className={styles.chevron} aria-hidden="true">
          {open ? "▾" : "▸"}
        </span>
        <span className={styles.toolName}>
          {subagent ? `${t("chat.tool.subagent")} · ` : ""}
          {tool.name}
        </span>
        <span className={styles.inputSummary}>
          {summarizeInput(tool.input)}
        </span>
        <span
          className={[styles.status, styles[`status_${tool.status}`]].join(" ")}
        >
          {t(`chat.tool.status.${tool.status}`)}
        </span>
        {duration ? <span className={styles.duration}>{duration}</span> : null}
      </button>
      {open ? (
        <div className={styles.cardBody}>
          <ToolBody tool={tool} />
        </div>
      ) : null}
    </div>
  );
}

/** Input, summary and result of one tool call (shared by the card and the chain step). */
export function ToolBody({ tool }: { tool: ToolItem }) {
  const { t } = useTranslation();
  return (
    <>
      <span className={styles.bodyLabel}>{t("chat.tool.input")}</span>
      <pre className={styles.code}>{prettyJson(tool.input)}</pre>
      {tool.summary ? <p className={styles.bodyText}>{tool.summary}</p> : null}
      {tool.denyReason ? (
        <p className={styles.bodyText}>
          {t("chat.tool.denyReason", { reason: tool.denyReason })}
        </p>
      ) : null}
      {tool.error ? (
        <>
          <span className={styles.bodyLabel}>{t("chat.tool.result")}</span>
          <pre className={styles.code}>{tool.error}</pre>
        </>
      ) : null}
      {!tool.error && tool.outputPreview ? (
        <>
          <span className={styles.bodyLabel}>{t("chat.tool.result")}</span>
          <pre className={styles.code}>{tool.outputPreview}</pre>
        </>
      ) : null}
    </>
  );
}

// ------------------------------------------------------------ approval card

export function useCountdown(expiresAtMs?: number): number | null {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (expiresAtMs === undefined) {
      return undefined;
    }
    const id = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(id);
  }, [expiresAtMs]);
  return expiresAtMs === undefined
    ? null
    : Math.max(0, Math.ceil((expiresAtMs - now) / 1000));
}

export interface ApprovalCardProps {
  tool: ToolItem;
  /** Resolves when the daemon accepted the decision; rejects with a message. */
  onDecide: (
    decision: "approve" | "deny",
    scope?: Scope,
    reason?: string
  ) => Promise<void>;
  /** Auto-deny deadline (epoch ms), when the daemon sets a timeout. */
  expiresAtMs?: number;
}

/** What a bot-made schedule will do, shown before the user approves it. */
function ScheduleApprovalBody({ d }: { d: ScheduleApprovalDetails }) {
  const { t, i18n } = useTranslation();
  const perm = d.permission;
  return (
    <div className={styles.scheduleBody} data-testid="approval-schedule">
      <div className={styles.scheduleRow}>
        <CalendarClock size={14} aria-hidden="true" />
        <span className={styles.scheduleWhen}>
          {d.schedule}
          {d.timezone ? ` (${d.timezone})` : ""}
        </span>
      </div>
      {d.name ? <p className={styles.bodyLabel}>{d.name}</p> : null}
      {d.previous ? (
        <div className={styles.scheduleWas} data-testid="approval-schedule-previous">
          <span className={styles.bodyLabel}>{t("chat.approval.schedule.was")}</span>
          <span>
            {d.previous.name} · {d.previous.schedule}
            {d.previous.timezone ? ` (${d.previous.timezone})` : ""}
          </span>
          <pre className={styles.code}>{d.previous.prompt}</pre>
        </div>
      ) : null}
      {d.prompt ? (
        <>
          <span className={styles.bodyLabel}>
            {d.previous
              ? t("chat.approval.schedule.promptNew", { count: d.promptChars })
              : t("chat.approval.schedule.prompt", { count: d.promptChars })}
          </span>
          <pre className={styles.code} tabIndex={0}>
            {d.prompt}
          </pre>
        </>
      ) : null}
      {d.nextRuns.length > 0 && d.action !== "pause" && d.action !== "delete" ? (
        <div className={styles.scheduleRow}>
          <Clock size={14} aria-hidden="true" />
          <span>
            {t("chat.approval.schedule.next")}{" "}
            {d.nextRuns.map((r) => formatFiring(r, i18n.language)).join(" · ")}
          </span>
        </div>
      ) : null}
      {perm && d.prompt ? (
        <div className={styles.scheduleRow} data-testid="approval-schedule-permission">
          <ShieldAlert size={14} aria-hidden="true" />
          <span>
            {t("chat.approval.schedule.permission", {
              level: t(`chat.approval.schedule.level.${perm.level}`, {
                defaultValue: perm.level
              })
            })}
            {perm.allow.length > 0
              ? ` ${t("chat.approval.schedule.allowed", { list: perm.allow.join(", ") })}`
              : ""}
          </span>
        </div>
      ) : null}
      {d.prompt ? <p className={styles.bodyLabel}>{t("chat.approval.schedule.note")}</p> : null}
    </div>
  );
}

export function ApprovalCard({
  tool,
  onDecide,
  expiresAtMs
}: ApprovalCardProps) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [reason, setReason] = useState("");
  const outbound = isOutboundSend(tool.name);
  const remaining = useCountdown(expiresAtMs);
  const schedule = scheduleDetails(tool.details);

  const decide = async (
    decision: "approve" | "deny",
    scope?: Scope,
    why?: string
  ) => {
    setBusy(true);
    setError(null);
    try {
      await onDecide(decision, scope, why);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

  const disabled = busy || remaining === 0;
  const glyph = describeTool(tool.name, tool.input, brandName);
  return (
    <div
      className={[styles.approval, outbound ? styles.outbound : ""]
        .filter(Boolean)
        .join(" ")}
      role="group"
      aria-label={t(outbound ? "chat.approval.sendTitle" : "chat.approval.title")}
      data-testid={outbound ? "approval-card-outbound" : "approval-card"}
    >
      <span className={styles.approvalIcon}>
        <StepIcon icon={glyph.icon} brand={glyph.brand} size={22} />
      </span>
      <div className={styles.approvalMain}>
        <div className={styles.approvalHead}>
          <span className={styles.approvalTitle}>
            {outbound ? t("chat.approval.sendTitle") : t("chat.approval.title")}
          </span>
          {remaining !== null ? (
            <span className={styles.countdown} aria-live="polite">
              {t("chat.approval.countdown", { seconds: remaining })}
            </span>
          ) : null}
        </div>
        {schedule ? (
          <>
            <p className={styles.approvalSummary}>
              {t(`chat.approval.schedule.title.${schedule.action}`)}
            </p>
            <ScheduleApprovalBody d={schedule} />
            <div className={styles.approvalMeta}>
              <span className={styles.approvalTool}>{tool.name}</span>
            </div>
          </>
        ) : (
          <>
            <p className={styles.approvalSummary}>{tool.summary ?? tool.name}</p>
            <div className={styles.approvalMeta}>
              <span className={styles.approvalTool}>{tool.name}</span>
              <span className={styles.inputSummary}>
                {summarizeInput(tool.input, 140)}
              </span>
            </div>
          </>
        )}
        <div className={styles.actions}>
          <Button
            size="sm"
            variant="primary"
            disabled={disabled}
            onClick={() => void decide("approve", "once")}
          >
            {outbound ? t("chat.approval.sendOnce") : t("chat.approval.once")}
          </Button>
          {/* The daemon refuses a standing grant for any card with details. */}
          {tool.details != null ? null : (
            <>
              <Button
                size="sm"
                variant="secondary"
                disabled={disabled}
                onClick={() => void decide("approve", "this_chat")}
              >
                {t("chat.approval.thisChat")}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                disabled={disabled}
                onClick={() => void decide("approve", "always")}
              >
                {t("chat.approval.always")}
              </Button>
            </>
          )}
          <Button
            size="sm"
            variant="ghost"
            className={styles.denyButton}
            disabled={disabled}
            onClick={() => void decide("deny", undefined, reason || undefined)}
          >
            {t("chat.approval.deny")}
          </Button>
        </div>
        <input
          className={[formStyles.input, styles.reasonInput].join(" ")}
          value={reason}
          placeholder={t("chat.approval.reasonPlaceholder")}
          aria-label={t("chat.approval.reasonPlaceholder")}
          onChange={(e) => setReason(e.target.value)}
        />
        {error ? <p className={formStyles.error}>{error}</p> : null}
      </div>
    </div>
  );
}

// ------------------------------------------------------------- question card

export function QuestionCard({
  q,
  onAnswer
}: {
  q: QuestionItem;
  onAnswer: (answer: string) => Promise<void>;
}) {
  const { t } = useTranslation();
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const answered = q.answer !== undefined;
  const locked = answered || q.closed === true;

  const submit = async (answer: string) => {
    if (!answer.trim()) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await onAnswer(answer.trim());
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={styles.question} data-testid="question-card">
      <span className={styles.approvalTitle}>{t("chat.question.title")}</span>
      <p className={styles.approvalSummary}>{q.question}</p>
      {answered ? (
        <p className={styles.bodyText}>
          {t("chat.question.answered", { answer: q.answer })}
        </p>
      ) : q.closed ? (
        <p className={styles.bodyText}>{t("chat.question.closed")}</p>
      ) : null}
      {!locked ? (
        <>
          {q.options.length > 0 ? (
            <div className={styles.actions}>
              {q.options.map((o) => (
                <Button
                  key={o}
                  size="sm"
                  variant="secondary"
                  disabled={busy}
                  onClick={() => void submit(o)}
                >
                  {o}
                </Button>
              ))}
            </div>
          ) : null}
          <form
            className={styles.freeText}
            onSubmit={(e) => {
              e.preventDefault();
              void submit(text);
            }}
          >
            <input
              className={formStyles.input}
              value={text}
              placeholder={t("chat.question.placeholder")}
              aria-label={t("chat.question.placeholder")}
              onChange={(e) => setText(e.target.value)}
            />
            <Button
              size="sm"
              variant="primary"
              type="submit"
              disabled={busy || !text.trim()}
            >
              {t("chat.question.answer")}
            </Button>
          </form>
        </>
      ) : null}
      {error ? <p className={formStyles.error}>{error}</p> : null}
    </div>
  );
}

// ---------------------------------------------------------------- todo/notice

export function TodoCard({ todo }: { todo: TodoListItem }) {
  const { t } = useTranslation();
  return (
    <div className={styles.card}>
      <div className={styles.cardBody}>
        <span className={styles.bodyLabel}>{t("chat.todo.title")}</span>
        <ul className={styles.todoList}>
          {todo.items.map((i, n) => (
            <li
              key={n}
              className={[styles.todoItem, styles[`todo_${i.status}`]].join(
                " "
              )}
            >
              <span aria-hidden="true">
                {i.status === "done" ? "✓" : i.status === "in_progress" ? "…" : "○"}
              </span>
              <span>{i.text}</span>
              <span className={styles.srOnly}>
                {t(`chat.todo.status.${i.status}`)}
              </span>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}

export function NoticeLine({ n }: { n: NoticeItem }) {
  const { t } = useTranslation();
  return (
    <p
      className={[styles.notice, styles[`notice_${n.tone}`]].join(" ")}
      role="status"
    >
      {t(n.i18nKey, n.values)}
    </p>
  );
}

// ------------------------------------------------------------ subagent card

export interface SubagentCardProps {
  /** The child run (its own items form the nested timeline). */
  run: RunState;
  /** Renders one nested timeline item (shared with the top-level view). */
  renderItem: (item: TimelineItem, runId: string) => ReactNode;
  /** Cancel the child run (and, daemon-side, its descendants). */
  onCancel: (runId: string) => Promise<void>;
}

function truncate(text: string, max: number): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}

export function SubagentCard({ run, renderItem, onCancel }: SubagentCardProps) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const status = subagentStatus(run);
  const waiting = status === "waiting_approval";
  // an approval inside the card must never stay hidden
  const expanded = open || waiting;
  const cancellable = status === "running" || waiting;
  const task = run.task ?? "";

  const cancel = async () => {
    setBusy(true);
    setError(null);
    try {
      await onCancel(run.id);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      className={[styles.card, styles.subagent].join(" ")}
      data-testid="subagent-card"
      data-status={status}
    >
      <div className={styles.subagentHead}>
        <button
          type="button"
          className={styles.cardHeader}
          aria-expanded={expanded}
          aria-label={t("chat.subagent.toggle", { task: truncate(task, 60) })}
          onClick={() => setOpen((o) => !o)}
        >
          <span className={styles.chevron} aria-hidden="true">
            {expanded ? "▾" : "▸"}
          </span>
          <span className={styles.toolName}>{t("chat.subagent.title")}</span>
          <span className={styles.inputSummary}>{truncate(task, 90)}</span>
          <span
            className={[styles.status, styles[`subagent_${status}`]].join(" ")}
          >
            {t(`chat.subagent.status.${status}`)}
          </span>
        </button>
        {cancellable ? (
          <Button
            size="sm"
            variant="danger"
            disabled={busy}
            aria-label={t("chat.subagent.cancelLabel", {
              task: truncate(task, 60)
            })}
            onClick={() => void cancel()}
          >
            {t("chat.subagent.cancel")}
          </Button>
        ) : null}
      </div>
      {error ? <p className={formStyles.error}>{error}</p> : null}
      {expanded ? (
        <div className={styles.cardBody}>
          {task ? (
            <>
              <span className={styles.bodyLabel}>
                {t("chat.subagent.task")}
              </span>
              <p className={styles.bodyText}>{task}</p>
            </>
          ) : null}
          <div className={styles.nested}>
            {run.items.map((item) => (
              <div key={item.key}>{renderItem(item, run.id)}</div>
            ))}
          </div>
          {run.summary ? (
            <>
              <span className={styles.bodyLabel}>
                {t("chat.subagent.summary")}
              </span>
              <p className={styles.bodyText}>{run.summary}</p>
            </>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
