import { type ReactNode, useEffect, useRef, useState } from "react";
import { AppWindow, CalendarClock, Clock, ShieldAlert, ShieldCheck } from "lucide-react";
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
  computerDetails,
  controlGrantDetails,
  egressDetails,
  payloadReviewComplete,
  appActionDetails,
  formatFiring,
  browserSteps,
  brainDetails,
  memoryOffer,
  scheduleDetails,
  type ControlGrantDetails,
  type EgressApprovalDetails,
  type ScheduleApprovalDetails
} from "../lib/approvalDetails";
import { brandIcon } from "../lib/brandIcons";
import { describeTool, isSendTool } from "../lib/toolLabels";
import { StepIcon } from "./StepIcon";
import formStyles from "./Form.module.css";
import styles from "./ChatCards.module.css";
import { humanLine, humanToolError } from "../lib/humanError";
import { actionText, reasonLine, type T } from "../lib/approvalPlain";
import { ErrorNotice } from "./ErrorNotice";

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
          <ErrorNotice compact error={humanToolError(tool.error)} />
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
  /** Name of the server that is the bots' home; `null` when the home is this computer. */
  serverName?: string | null;
  onTrustFolder?: (folder: string) => Promise<void>;
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

function EgressApprovalBody({ d, onReviewed }: {
  d: EgressApprovalDetails;
  onReviewed: (payload: string) => void;
}) {
  const { t } = useTranslation();
  const [expandedPayload, setExpandedPayload] = useState<string | null>(null);
  const expanded = expandedPayload === d.payload;
  const view = useRef<HTMLPreElement>(null);
  useEffect(() => {
    if (view.current && payloadReviewComplete(expanded, view.current)) onReviewed(d.payload);
  }, [expanded, d.payload, onReviewed]);
  const kind = d.payloadKind === "query" || d.payloadKind === "url" ? d.payloadKind : "body";
  return (
    <div data-testid="approval-egress">
      <p className={styles.bodyLabel}>{t("chat.approval.egress.host", { host: d.host })}</p>
      {d.read.length > 0 ? (
        <p className={styles.bodyLabel}>
          {t("chat.approval.egress.read", { list: d.read.join(", ") })}
        </p>
      ) : null}
      <span className={styles.bodyLabel}>{t(`chat.approval.egress.payload.${kind}`)}</span>
      {d.payloadLines != null && d.payloadChars != null ? (
        <p className={styles.bodyLabel}>
          {t("chat.approval.egress.size", {
            lines: d.payloadLines,
            chars: d.payloadChars,
            hidden: d.hiddenChars ?? 0
          })}
        </p>
      ) : null}
      {d.reviewFullPayload ? (
        <>
          <p className={styles.bodyLabel}>{t("chat.approval.egress.large", { bytes: d.payloadBytes ?? 0 })}</p>
          <Button size="sm" variant="secondary" onClick={() => setExpandedPayload(d.payload)}>
            {t("chat.approval.egress.reviewFull")}
          </Button>
        </>
      ) : null}
      {!d.reviewFullPayload || expanded ? (
        <pre
          ref={view}
          className={[styles.egressCode, d.reviewFullPayload ? styles.fullPayload : ""].join(" ")}
          tabIndex={0}
          onScroll={(event) => {
            if (payloadReviewComplete(expanded, event.currentTarget)) onReviewed(d.payload);
          }}
        >
          {d.payload}
        </pre>
      ) : null}
    </div>
  );
}

/** What a run-scoped control grant covers, or why this call cannot get one. */
function ControlGrantBody({ d }: { d: ControlGrantDetails }) {
  const { t } = useTranslation();
  return (
    <div className={styles.controlGrant} data-testid="approval-control-grant">
      <p className={styles.controlHead}>
        {d.offered ? (
          <ShieldCheck size={16} aria-hidden="true" />
        ) : (
          <ShieldAlert size={16} aria-hidden="true" />
        )}
        <span>
          {d.offered
            ? t("chat.approval.control.offered", { program: d.program })
            : t("chat.approval.control.refused", { program: d.program })}
        </span>
      </p>
      <p className={styles.controlLine}>
        <AppWindow size={14} aria-hidden="true" />
        <span>
          {d.program}
          {d.windowTitle ? ` · ${d.windowTitle}` : ""}
        </span>
      </p>
      {d.imagePath ? <p className={styles.controlPath}>{d.imagePath}</p> : null}
      {d.offered ? (
        <>
          <p className={styles.bodyText}>
            {t("chat.approval.control.covers", { program: d.program, path: d.imagePath })}
          </p>
          <p className={styles.bodyText}>{t("chat.approval.control.risk")}</p>
          <p className={styles.bodyText}>{t("chat.approval.control.stillAsks")}</p>
        </>
      ) : (
        <p className={styles.bodyText}>
          {t(`chat.approval.control.reason.${d.refusal}`, {
            defaultValue: t("chat.approval.control.reason.unknown")
          })}
        </p>
      )}
    </div>
  );
}

export function ApprovalCard({
  tool,
  onDecide,
  expiresAtMs,
  serverName = null,
  onTrustFolder
}: ApprovalCardProps) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [reason, setReason] = useState("");
  const outbound = isOutboundSend(tool.name);
  const remaining = useCountdown(expiresAtMs);
  const schedule = scheduleDetails(tool.details);
  const steps = browserSteps(tool.details);
  const egress = egressDetails(tool.details);
  const computer = computerDetails(tool.details);
  const control = controlGrantDetails(tool.details);
  const [reviewedPayload, setReviewedPayload] = useState<string | null>(null);
  const reviewPending = egress?.reviewFullPayload && reviewedPayload !== egress.payload;
  const app = appActionDetails(tool.details);
  const brain = brainDetails(tool.details);
  const unrenderable =
    (tool.details as { kind?: unknown } | null | undefined)?.kind === "app_action" && !app;
  const [trustedFolders, setTrustedFolders] = useState<string[]>([]);
  const folderFacts = (tool.details as { trust_folders?: unknown } | null)?.trust_folders;
  const folders = Array.isArray(folderFacts) ? folderFacts.filter((f): f is string => typeof f === "string") : [];
  const facts = tool.details as { why?: unknown; once_only?: unknown } | null | undefined;
  const why = typeof facts?.why === "string" ? facts.why : null;
  // What "this chat" / "always" would remember: one site, folder or command, never the whole tool.
  const memory = memoryOffer(tool.details);
  const memoryTarget = memory
    ? t(`chat.approval.memory.target.${memory.kind}`, { value: memory.pattern })
    : null;
  // The daemon says whether a standing choice may be offered; older events: any details mean once only.
  const onceOnly = typeof facts?.once_only === "boolean" ? facts.once_only : tool.details != null;

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
      setError(humanLine(e));
      setBusy(false);
    }
  };

  const disabled = busy || remaining === 0;
  const glyph = describeTool(tool.name, tool.input, brandName);
  const tt = t as unknown as T;
  const plainAction = actionText(tool.name, tool.input, { computer, serverName }, tt);
  const plainReason = egress ? reasonLine(egress, tt) : null;
  const detailBody = (
    <>
      {computer && (egress || schedule) ? (
        <p className={styles.bodyLabel} data-testid="approval-computer">
          {t("chat.approval.computer", {
            kind: computer.kind,
            name: computer.name,
            id: computer.nodeId
          })}
        </p>
      ) : null}
      {egress ? <EgressApprovalBody d={egress} onReviewed={setReviewedPayload} /> : null}
    </>
  );
  return (
    <div
      className={[styles.approval, outbound ? styles.outbound : ""]
        .filter(Boolean)
        .join(" ")}
      role="group"
      aria-label={t(outbound ? "chat.approval.sendTitle" : "chat.approval.title")}
      data-testid={outbound ? "approval-card-outbound" : "approval-card"}
      data-call-id={tool.callId}
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
        {app ? (
          <div data-testid="approval-app-action">
            <p className={styles.approvalSummary}>{app.app} · {app.action}</p>
            <p className={styles.bodyLabel}>{app.class} · {app.version}</p>
            <details open={app.count <= 1000}>
              <summary>{t("chat.approval.app.arguments", { count: app.count })}</summary>
              <pre className={styles.code} tabIndex={0}>{app.argumentsText}</pre>
            </details>
            <p className={styles.bodyLabel}>{t("chat.approval.app.footer")}</p>
          </div>
        ) : brain ? (
          <section className={styles.brainReview}>
            <strong>{t("knowledge.brainApproval")}</strong>
            <p className={formStyles.hint}>{t("knowledge.brainApprovalHint")}</p>
            <code>{brain.path}</code>
            <label>{t("knowledge.brainHash")}<code>{brain.sha256}</code></label>
            {brain.sources.length ? <ul aria-label={t("knowledge.brainSources")}>{brain.sources.map((source) => <li key={source.id}>{source.label} · {source.id} · v{source.revision}</li>)}</ul> : null}
            <pre className={styles.brainContent}>{brain.content}</pre>
          </section>
        ) : schedule ? (
          <>
            <p className={styles.approvalSummary}>
              {t(`chat.approval.schedule.title.${schedule.action}`)}
            </p>
            <ScheduleApprovalBody d={schedule} />
            <div className={styles.approvalMeta}>
              <span className={styles.approvalTool}>{tool.name}</span>
            </div>
          </>
        ) : steps ? (
          <>
            {/* The raw input is not shown: it can hold a value typed into a secret field. */}
            <p className={styles.approvalSummary}>
              {t("chat.approval.steps.title", { count: steps.length })}
            </p>
            <ol className={styles.bodyLabel} data-testid="approval-browser-steps">
              {steps.map((s) => (
                <li key={s.n}>
                  {s.action} {s.target}
                  {s.hidden
                    ? ` = ${t("chat.approval.steps.hidden")}`
                    : s.value !== null
                      ? ` = ${s.value}`
                      : ""}
                </li>
              ))}
            </ol>
            <div className={styles.approvalMeta}>
              <span className={styles.approvalTool}>{tool.name}</span>
            </div>
          </>
        ) : (
          <>
            <p className={styles.approvalSummary}>
              {plainAction || plainReason || egress?.reason || tool.summary || tool.name}
            </p>
            {plainAction && plainReason ? (
              <p className={styles.bodyLabel} data-testid="approval-reason">{plainReason}</p>
            ) : null}
            <div className={styles.approvalMeta}>
              <span className={styles.approvalTool}>{tool.name}</span>
              <span className={styles.inputSummary}>
                {summarizeInput(tool.input, 140)}
              </span>
            </div>
          </>
        )}
        {why ? (
          <p className={styles.bodyLabel} data-testid="approval-why">
            {t(`chat.approval.why.${why}`, { defaultValue: "" })}
          </p>
        ) : null}
        {control ? <ControlGrantBody d={control} /> : null}
        {plainAction && (egress || computer) ? (
          <details open={egress?.reviewFullPayload === true} data-testid="approval-details">
            <summary className={styles.bodyLabel}>{t("chat.approval.plain.details")}</summary>
            {detailBody}
          </details>
        ) : (
          detailBody
        )}
        {onTrustFolder ? folders.map((folder) => (
          <Button key={folder} size="sm" variant="secondary" disabled={disabled || trustedFolders.includes(folder)}
            onClick={() => {
              setBusy(true);
              setError(null);
              void onTrustFolder(folder).then(() => setTrustedFolders((prev) => [...prev, folder]))
                .catch((e: unknown) => setError(humanLine(e))).finally(() => setBusy(false));
            }}>
            {t(trustedFolders.includes(folder) ? "folderTrust.saved" : "folderTrust.action", { folder })}
          </Button>
        )) : null}
        <div className={styles.actions}>
          <Button
            size="sm"
            variant="primary"
            disabled={disabled || reviewPending || unrenderable}
            onClick={() => void decide("approve", "once")}
          >
            {isSendTool(tool.name)
              ? t("chat.approval.sendOnce")
              : t("chat.approval.once")}
          </Button>
          {/* One program, until this task ends: only when the daemon offered it. */}
          {control?.offered ? (
            <Button
              size="sm"
              variant="secondary"
              disabled={disabled || reviewPending}
              onClick={() => void decide("approve", "run_control")}
            >
              {t("chat.approval.control.button", { program: control.program })}
            </Button>
          ) : null}
          {/* The daemon refuses a standing grant for a once-only card. */}
          {onceOnly ? null : (
            <>
              <Button
                size="sm"
                variant="secondary"
                disabled={disabled}
                onClick={() => void decide("approve", "this_chat")}
              >
                {memoryTarget
                  ? t("chat.approval.memory.thisChat", { target: memoryTarget })
                  : t("chat.approval.thisChat")}
              </Button>
              {/* A taint-gate card keeps its choice to this chat (tool class and site). */}
              {why === "egress" || why === "executes" ? null : (
              <Button
                size="sm"
                variant="ghost"
                disabled={disabled}
                onClick={() => void decide("approve", "always")}
              >
                {memoryTarget
                  ? t("chat.approval.memory.always", { target: memoryTarget })
                  : t("chat.approval.always")}
              </Button>
              )}
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
      setError(humanLine(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={styles.question} data-testid="question-card" data-call-id={q.callId}>
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
      {n.i18nKey === "chat.notice.error" ? humanLine(n.values?.message ?? "") : t(n.i18nKey, n.values)}
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
  // an approval or a question inside the card must never stay hidden
  const asking = run.items.some(
    (i) => i.kind === "question" && i.answer === undefined && !i.closed
  );
  const expanded = open || waiting || asking;
  const cancellable = status === "running" || waiting;
  const task = run.task ?? "";

  const cancel = async () => {
    setBusy(true);
    setError(null);
    try {
      await onCancel(run.id);
    } catch (e) {
      setError(humanLine(e));
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
