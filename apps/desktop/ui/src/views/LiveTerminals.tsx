import { ChevronDown, ChevronUp, Eye, SquareTerminal } from "lucide-react";
import { useId, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import type { BotTerminal } from "../api/types";
import { activeTerminal, aliveCount, screenLines, terminalStateKey } from "../lib/liveTerminals";
import styles from "./LiveTerminals.module.css";

/** Windows at least this tall (CSS px) start with the terminal panel open. */
const TALL_WINDOW = 720;

/** Shortest form of a command for a tab: the program name without its folder, plus the first word. */
function tabName(command: string): string {
  const [program = "", ...rest] = command.trim().split(/\s+/);
  const name = program.split(/[\\/]/).pop() || program;
  return [name, ...rest.slice(0, 1)].join(" ");
}

/**
 * The terminals the bot runs, shown next to the screen picture. A program on a pseudo-terminal has
 * no window, so the picture cannot show it (#894). This panel is the view: the text the program
 * printed, as the daemon already masked it. It has no input at all, in watching or in control: the
 * owner's keys go to the screen picture, never into a terminal.
 */
export function LiveTerminals({ terminals }: { terminals: readonly BotTerminal[] }) {
  const { t } = useTranslation();
  // A short window keeps the picture the main thing: the panel starts folded there (its header still
  // says how many terminals run) and the owner opens it with one click.
  const [open, setOpen] = useState(() => typeof window === "undefined" || window.innerHeight >= TALL_WINDOW);
  const [chosen, setChosen] = useState<string | null>(null);
  const bodyId = useId();
  const logRef = useRef<HTMLPreElement | null>(null);
  const stuckRef = useRef(true);
  const tabRefs = useRef(new Map<string, HTMLButtonElement>());

  const current = activeTerminal(terminals, chosen);
  const screen = current?.screen ?? "";
  // Follow the newest lines unless the owner scrolled up to read earlier ones.
  useLayoutEffect(() => {
    const el = logRef.current;
    if (el && stuckRef.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [screen, open, current?.id]);

  if (!current) {
    return null;
  }
  const key = terminalStateKey(current.state);
  const stateText =
    key === "finished" || key === "failed"
      ? t(`liveView.terminals.state.${key}`, { code: current.exit_code ?? "" })
      : t(`liveView.terminals.state.${key}`);
  const alive = aliveCount(terminals);

  const onTabKey = (e: KeyboardEvent<HTMLButtonElement>, index: number) => {
    const last = terminals.length - 1;
    const to =
      e.key === "ArrowRight" ? (index === last ? 0 : index + 1)
      : e.key === "ArrowLeft" ? (index === 0 ? last : index - 1)
      : e.key === "Home" ? 0
      : e.key === "End" ? last
      : null;
    const next = to === null ? undefined : terminals[to];
    if (next) {
      e.preventDefault();
      setChosen(next.id);
      tabRefs.current.get(next.id)?.focus();
    }
  };

  return (
    <section
      className={styles.panel}
      aria-label={t("liveView.terminals.title")}
      data-testid="live-viewer-terminals"
      data-open={open ? "true" : "false"}
    >
      <div className={styles.head}>
        <button
          type="button"
          className={styles.toggle}
          aria-expanded={open}
          aria-controls={bodyId}
          onClick={() => setOpen((v) => !v)}
          data-testid="live-viewer-terminals-toggle"
        >
          <SquareTerminal aria-hidden="true" />
          <span>{t("liveView.terminals.title")}</span>
          <span className={styles.count}>{t("liveView.terminals.count", { count: terminals.length, alive })}</span>
          {open ? <ChevronDown aria-hidden="true" /> : <ChevronUp aria-hidden="true" />}
        </button>
        <span className={styles.readOnly} hidden={!open} data-testid="live-viewer-terminals-readonly">
          <Eye aria-hidden="true" />
          <span>{t("liveView.terminals.readOnly")}</span>
        </span>
      </div>
      <div id={bodyId} hidden={!open} className={styles.body}>
        {terminals.length > 1 ? (
          <div role="tablist" aria-label={t("liveView.terminals.list")} className={styles.tabs}>
            {terminals.map((term, index) => (
              <button
                key={term.id}
                ref={(el) => {
                  if (el) tabRefs.current.set(term.id, el);
                  else tabRefs.current.delete(term.id);
                }}
                type="button"
                role="tab"
                id={`${bodyId}-tab-${term.id}`}
                aria-selected={term.id === current.id}
                aria-controls={`${bodyId}-panel`}
                tabIndex={term.id === current.id ? 0 : -1}
                className={styles.tab}
                title={term.command}
                onClick={() => setChosen(term.id)}
                onKeyDown={(e) => onTabKey(e, index)}
              >
                {tabName(term.command)}
              </button>
            ))}
          </div>
        ) : null}
        <div
          id={`${bodyId}-panel`}
          role={terminals.length > 1 ? "tabpanel" : undefined}
          aria-labelledby={terminals.length > 1 ? `${bodyId}-tab-${current.id}` : undefined}
        >
          <div className={styles.meta}>
            <span className={styles.state} data-state={key} role="status" data-testid="live-viewer-terminal-state">
              {stateText}
            </span>
            <code className={styles.command} title={current.command}>
              {current.command}
            </code>
            <span className={styles.size}>{`${current.cols}×${current.rows}`}</span>
          </div>
          <pre
            ref={logRef}
            className={styles.screen}
            role="log"
            aria-live="off"
            tabIndex={0}
            aria-label={t("liveView.terminals.screenLabel", { command: tabName(current.command) })}
            onScroll={(e) => {
              const el = e.currentTarget;
              stuckRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 8;
            }}
            data-testid="live-viewer-terminal-screen"
          >
            {screenLines(screen).join("\n")}
          </pre>
        </div>
      </div>
    </section>
  );
}
