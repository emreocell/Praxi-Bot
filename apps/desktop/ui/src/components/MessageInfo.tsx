import { Info } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Run, RunUsage } from "../api/types";
import {
  type InfoRow,
  type MessageSourceKind,
  messageSourceKind,
  replyInfoRows,
  userInfoRows
} from "../lib/messageInfo";
import { useDaemon } from "../state/DaemonContext";
import type { RunState } from "../state/timeline";
import styles from "./MessageInfo.module.css";
import { Popover } from "./Popover";

/** The facts the daemon already keeps about a bot reply: the run's events and its list entry. */
function replyFacts(run: RunState | undefined, dto: Run | undefined, usage: RunUsage | null, computer: string | null) {
  const finished =
    run?.finishedMs ?? (dto && !["queued", "running", "waiting_approval", "paused"].includes(dto.state) ? dto.updated_at_ms : undefined);
  return {
    startedMs: run?.startedMs ?? dto?.created_at_ms,
    finishedMs: finished,
    steps: run ? run.items.filter((i) => i.kind === "tool").length : undefined,
    model: usage?.model ?? null,
    computer: computer ?? dto?.computer_node_name ?? null,
    promptTokens: usage ? usage.prompt_tokens : null,
    completionTokens: usage ? usage.completion_tokens : null
  };
}

/**
 * The faint info button beside a message (design v7): click opens a small popover with the model,
 * source, times with seconds, steps, computer and tokens for a bot reply, or the sent time and
 * source for the owner's own message. Keyboard reachable; stronger on hover and focus.
 */
export function MessageInfo({
  side,
  ts,
  runId,
  run,
  runDto,
  source,
  computer = null
}: {
  side: "bot" | "me";
  /** The message's own time. */
  ts: number;
  runId?: string | null;
  run?: RunState;
  runDto?: Run;
  /** The daemon's record of where a user message came from. */
  source?: string | null;
  /** Name of the computer this run worked on, when known more precisely than the run record. */
  computer?: string | null;
}) {
  const { t, i18n } = useTranslation();
  const { client } = useDaemon();
  const [open, setOpen] = useState(false);
  const [usage, setUsage] = useState<RunUsage | null>(null);
  const ref = useRef<HTMLButtonElement | null>(null);
  // The usage record (model, tokens) is asked for once, the first time the popover opens.
  useEffect(() => {
    if (!open || side !== "bot" || !runId || usage || !client) {
      return undefined;
    }
    let live = true;
    client
      .getRunUsage(runId)
      .then((u) => live && setUsage(u))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [open, side, runId, usage, client]);

  const rows: InfoRow[] =
    side === "bot"
      ? replyInfoRows(replyFacts(run, runDto, usage, computer), i18n.language, {
          s: t("messageInfo.unit.s"),
          m: t("messageInfo.unit.m"),
          steps: (n) => t("messageInfo.steps", { count: n })
        })
      : userInfoRows(ts, source, i18n.language, (k: MessageSourceKind) => t(`messageInfo.sources.${k}`));
  if (side === "bot" && rows.length === 0 && !usage) {
    rows.push(...userInfoRows(ts, null, i18n.language, () => "").map((r) => ({ ...r, key: "finished" as const })));
  }
  if (side === "bot" && source) {
    const kind = messageSourceKind(source);
    if (kind !== "unknown") {
      rows.push({ key: "source", value: t(`messageInfo.sources.${kind}`) });
    }
  }
  return (
    <>
      <button
        ref={ref}
        type="button"
        className={styles.info}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={t("messageInfo.label")}
        title={t("messageInfo.label")}
        data-testid="message-info"
        onClick={() => setOpen((o) => !o)}
      >
        <Info aria-hidden="true" />
      </button>
      <Popover
        open={open}
        anchor={ref}
        onClose={() => setOpen(false)}
        placement={side === "me" ? "top-end" : "top-start"}
        role="dialog"
        label={t("messageInfo.title")}
        width={232}
        className={styles.pop}
      >
        <dl className={styles.list} data-testid="message-info-popover">
          {rows.map((r) => (
            <div key={r.key} className={styles.row}>
              <dt>{t(`messageInfo.rows.${r.key}`)}</dt>
              <dd>{r.value}</dd>
            </div>
          ))}
        </dl>
      </Popover>
    </>
  );
}
