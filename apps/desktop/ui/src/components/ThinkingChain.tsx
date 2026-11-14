import { ChevronDown, ChevronRight, Globe } from "lucide-react";
import { type ReactNode, useId, useState } from "react";
import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";
import { brandIcon } from "../lib/brandIcons";
import { type Chain, chainSeconds, type ChainStep } from "../lib/thinkingChain";
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

function ToolStep({ tool }: { tool: ToolItem }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const panelId = useId();
  const label = describeTool(tool.name, tool.input, brandName);
  const active = tool.status === "requested" || tool.status === "running";
  const bad =
    tool.status === "error" ||
    tool.status === "denied" ||
    tool.status === "expired";
  const phase = stepPhase(tool.status);
  const text = t(`${label.key}.${phase}`, label.values);
  return (
    <li
      className={[styles.step, bad ? styles.stepBad : ""]
        .filter(Boolean)
        .join(" ")}
      data-testid="tool-card"
      data-status={tool.status}
    >
      <StepIcon icon={label.icon} brand={label.brand} size={17} />
      <div className={styles.stepMain}>
        <div className={styles.stepLine}>
          <b className={active ? styles.shim : styles.stepLabel}>{text}</b>
          {bad ? (
            <span className={styles.stepStatus}>
              {t(`chat.tool.status.${tool.status}`)}
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
        </div>
        {label.chips.length > 0 ? (
          <div className={styles.chips}>
            {label.chips.map((c, i) => (
              <span key={i} className={styles.chip}>
                {c.icon === "globe" ? <Globe aria-hidden="true" /> : null}
                {c.text}
              </span>
            ))}
          </div>
        ) : null}
        {open ? (
          <div id={panelId} className={styles.detail}>
            <span className={styles.rawName}>{tool.name}</span>
            <ToolBody tool={tool} />
          </div>
        ) : null}
      </div>
    </li>
  );
}

function Step({ step }: { step: ChainStep }) {
  return step.kind === "thought" ? (
    <li className={styles.thought}>
      <Markdown text={step.text} />
    </li>
  ) : (
    <ToolStep tool={step.tool} />
  );
}

/**
 * Tool calls and short thoughts folded into one line ("Thought for 12
 * seconds"); opening it lists the steps in plain words with source chips.
 * While the run is working it is open and the title shimmers.
 */
export function ThinkingChain({
  chain,
  avatar
}: {
  chain: Chain;
  /** The bot's character, shown at the start of the line. */
  avatar?: ReactNode;
}) {
  const { t } = useTranslation();
  const [userOpen, setUserOpen] = useState<boolean | null>(null);
  const listId = useId();
  const open = userOpen ?? chain.live;
  const title = chain.live
    ? t("chat.chain.working")
    : t("chat.chain.thought", {
        duration: formatSpan(t, chainSeconds(chain))
      });
  return (
    <div
      className={styles.chain}
      data-testid="thinking-chain"
      data-live={chain.live ? "true" : undefined}
    >
      <button
        type="button"
        className={styles.head}
        aria-expanded={open}
        aria-controls={listId}
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
        <ol id={listId} className={styles.steps}>
          {chain.steps.map((s) => (
            <Step key={s.key} step={s} />
          ))}
        </ol>
      ) : null}
    </div>
  );
}
