import { Check, ChevronDown, ChevronRight, Globe, Loader2, Minus, X } from "lucide-react";
import { type ReactNode, useEffect, useId, useState } from "react";
import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";
import { brandIcon } from "../lib/brandIcons";
import { formatClock, formatStamp, formatStepDuration, type MessageTimes } from "../lib/messageTime";
import { type Chain, chainSeconds, type ChainStep } from "../lib/thinkingChain";
import { buildRows, capRows, chainSummary, collapsedRows, groupHost, type ChainRow } from "../lib/chainRows";
import { isInFlight, isOrphanStep, isSlowStep } from "../lib/stepStuck";
import { showStepDuration, type StepState, stepState } from "../lib/stepState";
import { humanToolError } from "../lib/humanError";
import { siteIcon } from "../lib/siteIcon";
import { stepReason } from "../lib/stepReason";
import { describeTool, stepPhase } from "../lib/toolLabels";
import type { ToolItem } from "../state/timeline";
import { ToolBody } from "./ChatCards";
import { Markdown } from "./Markdown";
import { StepIcon } from "./StepIcon";
import styles from "./ThinkingChain.module.css";

/**
 * Plural keys (`<name>_one` / `<name>_other`) are looked up through a
 * variable so check:i18n, which only sees literal keys, does not flag them.
 */
function counted(t: TFunction, name: string, count: number): string {
  return t(`chat.${name}`, { count });
}

/** "12 seconds" / "1 minute 5 seconds". */
export function formatSpan(t: TFunction, seconds: number): string {
  if (seconds < 60) {
    return counted(t, "duration.seconds", seconds);
  }
  const m = Math.floor(seconds / 60);
  const s = seconds % 60;
  const mins = counted(t, "duration.minutes", m);
  return s > 0 ? `${mins} ${counted(t, "duration.seconds", s)}` : mins;
}

const brandName = (name: string): string | null => {
  const b = brandIcon(name);
  return b.known ? b.name : null;
};

/** Daemon-measured run time, else request to completion. */
function stepMs(tool: ToolItem): number | undefined {
  if (tool.durationMs !== undefined) {
    return tool.durationMs;
  }
  return tool.completedMs !== undefined ? tool.completedMs - tool.requestedMs : undefined;
}

const STATE_GLYPH = { running: Loader2, done: Check, failed: X, skipped: Minus } as const;

/** One small, consistent mark for the step's state (the words stay in the label). */
function StateMark({ state }: { state: StepState }) {
  const { t } = useTranslation();
  const Glyph = STATE_GLYPH[state];
  return (
    <span
      className={[styles.state, styles[`state_${state}`]].join(" ")}
      role="img"
      aria-label={t(`chat.chain.state.${state}`)}
      data-testid="step-state"
      data-state={state}
    >
      <Glyph aria-hidden="true" />
    </span>
  );
}

function ToolStep({ tool, times, live }: { tool: ToolItem; times: MessageTimes; live: boolean }) {
  const { t, i18n } = useTranslation();
  const ms = stepMs(tool);
  const clock = times === "off" ? null : formatClock(tool.requestedMs, i18n.language, true);
  const [open, setOpen] = useState(false);
  const panelId = useId();
  const label = describeTool(tool.name, tool.input, brandName);
  // A step whose end never arrived in a chain that is over is shown as not finished, never as running.
  const orphan = isOrphanStep(tool.status, live);
  const active = isInFlight(tool.status) && !orphan;
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return undefined;
    const id = window.setInterval(() => setNow(Date.now()), 5000);
    return () => window.clearInterval(id);
  }, [active]);
  const slow = active && isSlowStep(tool, now);
  const bad =
    orphan ||
    tool.status === "error" ||
    tool.status === "denied" ||
    tool.status === "expired";
  const state = stepState(tool.status, orphan);
  const phase = orphan ? "failed" : stepPhase(tool.status);
  const text = t(`${label.key}.${phase}`, label.values);
  const reason = stepReason(tool, orphan);
  const reasonText = reason
    ? reason.kind === "timeout"
      ? reason.seconds
        ? t("chat.chain.reason.timeoutAfter", { seconds: reason.seconds })
        : t("chat.chain.reason.timeout")
      : reason.kind === "other"
        ? tool.error
          ? humanToolError(tool.error).title
          : t("chat.chain.reason.other")
        : t(`chat.chain.reason.${reason.kind}`)
    : null;
  const site = label.chips.find((c) => c.icon === "globe");
  const icon = site ? siteIcon(site.text) : null;
  const timeText =
    ms !== undefined && showStepDuration(ms, state)
      ? formatStepDuration(ms, i18n.language, {
          s: t("chat.duration.shortSeconds"),
          m: t("chat.duration.shortMinutes")
        })
      : "";
  return (
    <li
      className={[styles.step, bad ? styles.stepBad : ""].filter(Boolean).join(" ")}
      data-testid="tool-card"
      data-status={tool.status}
    >
      <div className={styles.stepLine}>
        <StateMark state={state} />
        <b className={active ? styles.shim : styles.stepLabel}>{text}</b>
        {site && icon ? (
          <span className={styles.site} data-testid="step-site" title={site.text}>
            <span className={styles.siteMark} aria-hidden="true">
              {icon.kind === "letter" ? icon.letter : <Globe />}
            </span>
            <span className={styles.siteName}>{site.text}</span>
          </span>
        ) : (
          label.chips.slice(0, 1).map((c, i) => (
            <span key={i} className={styles.chip}>
              {c.text}
            </span>
          ))
        )}
        {reasonText && !open ? (
          <span className={styles.reason} data-testid="step-error">
            {reasonText}
          </span>
        ) : null}
        {slow ? (
          <span className={styles.slow} data-testid="step-slow">
            {t("chat.chain.slow")}
          </span>
        ) : null}
        <span className={styles.stepEnd}>
          {clock || timeText ? (
            <span
              className={styles.stepTime}
              title={formatStamp(tool.requestedMs, i18n.language)}
              data-testid="step-time"
            >
              {[clock, timeText].filter(Boolean).join(" · ")}
            </span>
          ) : null}
          <button
            type="button"
            className={styles.more}
            aria-expanded={open}
            aria-controls={panelId}
            aria-label={`${t("chat.chain.details")}: ${text}`}
            title={t("chat.chain.details")}
            onClick={() => setOpen((o) => !o)}
          >
            <ChevronRight className={open ? styles.moreOpen : undefined} />
          </button>
        </span>
      </div>
      {open ? (
        <div id={panelId} className={styles.detail}>
          {reasonText ? <p className={styles.stepError}>{reasonText}</p> : null}
          <span className={styles.rawName}>{tool.name}</span>
          <ToolBody tool={tool} />
        </div>
      ) : null}
    </li>
  );
}

function Step({ step, times, live }: { step: ChainStep; times: MessageTimes; live: boolean }) {
  return step.kind === "thought" ? (
    <li className={styles.thought}>
      <Markdown text={step.text} />
    </li>
  ) : (
    <ToolStep tool={step.tool} times={times} live={live} />
  );
}


/** Live chains up to this many rows stay open as before; longer ones fold. */
const LIVE_OPEN_ROWS = 3;

/** Several finished calls of one app or tool as ONE row ("Playwright - browser evaluate x9 - 12 s"). */
function GroupRow({ tools, sameAction, times, live }: { tools: ToolItem[]; sameAction: boolean; times: MessageTimes; live: boolean }) {
  const { t, i18n } = useTranslation();
  const [open, setOpen] = useState(false);
  const panelId = useId();
  const first = tools[0] as ToolItem;
  const label = describeTool(first.name, first.input, brandName);
  const totalMs = tools.reduce((sum, x) => sum + (stepMs(x) ?? 0), 0);
  const host = groupHost(tools);
  const text = host
    ? t("chat.chain.groupSite", { host })
    : sameAction
      ? t("chat.chain.groupSame", { what: t(`${label.key}.done`, label.values), count: tools.length })
      : t("chat.chain.groupApp", { app: String(label.values.app ?? label.values.name ?? ""), count: tools.length });
  const time =
    times === "off"
      ? null
      : formatStepDuration(totalMs, i18n.language, {
          s: t("chat.duration.shortSeconds"),
          m: t("chat.duration.shortMinutes")
        });
  const icon = host ? siteIcon(host) : null;
  return (
    <li className={styles.step} data-testid="tool-group">
      <div className={styles.stepLine}>
        <span className={styles.groupMark}>
          {icon ? (
            <span className={styles.siteMark} aria-hidden="true">
              {icon.kind === "letter" ? icon.letter : <Globe />}
            </span>
          ) : (
            <StepIcon icon={label.icon} brand={label.brand} size={16} />
          )}
        </span>
        <b className={styles.stepLabel}>{text}</b>
        <span className={styles.reason}>{counted(t, "chain.stepCount", tools.length)}</span>
        <span className={styles.stepEnd}>
          {time ? <span className={styles.stepTime}>{time}</span> : null}
          <button
            type="button"
            className={styles.more}
            aria-expanded={open}
            aria-controls={panelId}
            aria-label={`${t("chat.chain.details")}: ${text}`}
            title={t("chat.chain.details")}
            onClick={() => setOpen((o) => !o)}
          >
            <ChevronRight className={open ? styles.moreOpen : undefined} />
          </button>
        </span>
      </div>
      {open ? (
        <ol id={panelId} className={styles.steps}>
          {tools.map((x) => (
            <ToolStep key={x.key} tool={x} times={times} live={live} />
          ))}
        </ol>
      ) : null}
    </li>
  );
}

function RowView({ row, times, live }: { row: ChainRow; times: MessageTimes; live: boolean }) {
  if (row.kind === "group") {
    return <GroupRow tools={row.tools} sameAction={row.sameAction} times={times} live={live} />;
  }
  return <Step step={row.kind === "thought" ? row.step : { kind: "tool", key: row.key, tool: row.tool }} times={times} live={live} />;
}

/**
 * Tool calls and short thoughts folded into one line ("Thought for 12
 * seconds"); opening it lists the steps in plain words with source chips.
 * While the run is working it is open and the title shimmers.
 */
export function ThinkingChain({
  chain,
  avatar,
  times = "minutes"
}: {
  chain: Chain;
  /** Settings, General: step times off / on. */
  times?: MessageTimes;
  /** The bot's character, shown at the start of the line. */
  avatar?: ReactNode;
}) {
  const { t, i18n } = useTranslation();
  const [userOpen, setUserOpen] = useState<boolean | null>(null);
  const [showAll, setShowAll] = useState(false);
  const listId = useId();
  const rows = buildRows(chain.steps);
  const open = userOpen ?? (chain.live && rows.length <= LIVE_OPEN_ROWS);
  const summary = chainSummary(chain);
  const brandLabel = (id: string) => {
    const b = brandName(id);
    if (b) return b;
    const h = id.replace(/[_\-.:/]+/g, " ").trim();
    return h.charAt(0).toUpperCase() + h.slice(1);
  };
  const what = summary.names.slice(0, 2).map(brandLabel).join(", ");
  const title = chain.live
    ? t("chat.chain.working")
    : summary.steps >= 2
      ? t(what ? "chat.chain.summary" : "chat.chain.summaryPlain", {
          count: summary.steps,
          what,
          duration: formatSpan(t, chainSeconds(chain))
        })
      : t("chat.chain.thought", {
          duration: formatSpan(t, chainSeconds(chain))
        });
  const capped = capRows(rows, showAll);
  const folded = open ? null : collapsedRows(rows, chain.live);
  const span =
    times === "off"
      ? undefined
      : chain.live
        ? t("chat.chain.started", { start: formatStamp(chain.spanStartMs, i18n.language) })
        : t("chat.chain.startedFinished", {
            start: formatStamp(chain.spanStartMs, i18n.language),
            end: formatStamp(chain.endMs, i18n.language)
          });
  return (
    <div
      className={styles.chain}
      data-testid="thinking-chain"
      data-live={chain.live ? "true" : undefined}
    >
      {/* One polite announcement when the run ends; the live chain stays silent. */}
      <span className={styles.srOnly} role="status" aria-live="polite">
        {chain.live ? "" : title}
      </span>
      <button
        type="button"
        className={styles.head}
        aria-expanded={open}
        aria-controls={listId}
        title={span}
        onClick={() => setUserOpen(!open)}
      >
        {avatar}
        <span className={chain.live ? styles.shim : styles.title}>{title}</span>
        {chain.problems > 0 ? (
          <span className={styles.problems}>
            {counted(t, "chain.problems", chain.problems)}
          </span>
        ) : null}
        <ChevronDown
          className={[styles.chev, open ? styles.chevOpen : ""]
            .filter(Boolean)
            .join(" ")}
          aria-hidden="true"
        />
      </button>
      {open ? (
        <>
          <ol id={listId} className={styles.steps}>
            {capped.shown.map((r) => (
              <RowView key={r.key} row={r} times={times} live={chain.live} />
            ))}
          </ol>
          {capped.hidden > 0 ? (
            <button type="button" className={styles.showAll} onClick={() => setShowAll(true)}>
              {t("chat.chain.showAll", { count: capped.hidden })}
            </button>
          ) : null}
        </>
      ) : folded && folded.shown.length > 0 ? (
        <ol id={listId} className={styles.steps} data-testid="chain-folded">
          {folded.shown.map((r) => (
            <RowView key={r.key} row={r} times={times} live={chain.live} />
          ))}
        </ol>
      ) : null}
    </div>
  );
}
