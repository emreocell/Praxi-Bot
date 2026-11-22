import "@xterm/xterm/css/xterm.css";
import { Terminal as TerminalIcon, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ApiClient } from "../api/client";
import { humanLine } from "../lib/humanError";
import { TerminalLink, type TerminalApiLike } from "../lib/terminalSession";
import styles from "./ServerTerminal.module.css";

type Phase = "starting" | "live" | "ended" | "failed";

/** The terminal's API for one server, over this computer's daemon (owner key added by the client). */
export function terminalApi(api: ApiClient, hostId: string): TerminalApiLike {
  return {
    open: (cols, rows) => api.terminalOpen(hostId, cols, rows),
    output: (id, after, waitMs, signal) => api.terminalOutput(hostId, id, after, waitMs, signal),
    input: (id, data) => api.terminalInput(hostId, id, data),
    resize: (id, cols, rows) => api.terminalResize(hostId, id, cols, rows),
    close: (id) => api.terminalClose(hostId, id)
  };
}

/** A theme token as a colour string for the terminal widget (read from the live theme). */
function token(name: string, fallback: string): string {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v === "" ? fallback : v;
}

/**
 * A shell on the bots' home, over the paired link (no SSH). It lives while this panel is open;
 * closing it ends the shell on the server. A command passed as `prefill` is typed into the input
 * line, never executed: the owner reads it and presses Enter.
 */
export function ServerTerminal({
  api,
  hostId,
  hostName,
  prefill,
  onClose
}: {
  api: ApiClient;
  hostId: string;
  hostName: string;
  prefill: string | null;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const shownName = hostName || t("computerPicker.host");
  const holder = useRef<HTMLDivElement | null>(null);
  const [phase, setPhase] = useState<Phase>("starting");
  const [problem, setProblem] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    const node = holder.current;
    if (!node) {
      return undefined;
    }
    let stopped = false;
    let link: TerminalLink | null = null;
    let observer: ResizeObserver | null = null;
    let term: import("@xterm/xterm").Terminal | null = null;
    setPhase("starting");
    setProblem(null);
    void (async () => {
      const [{ Terminal }, { FitAddon }] = await Promise.all([import("@xterm/xterm"), import("@xterm/addon-fit")]);
      if (stopped) {
        return;
      }
      const background = token("--color-background", "black");
      const fit = new FitAddon();
      term = new Terminal({
        cursorBlink: true,
        fontFamily: token("--font-mono", "monospace"),
        fontSize: 13,
        scrollback: 4000,
        theme: {
          background,
          foreground: token("--color-text", "white"),
          cursor: token("--color-accent", "white")
        }
      });
      term.loadAddon(fit);
      term.open(node);
      node.style.setProperty("--term-background", background);
      fit.fit();
      const live = new TerminalLink(terminalApi(api, hostId), {
        write: (bytes) => term?.write(bytes),
        ended: () => setPhase("ended"),
        failed: (e) => {
          setProblem(humanLine(e));
          setPhase("failed");
        }
      });
      link = live;
      term.onData((d) => live.send(d));
      term.onResize(({ cols, rows }) => live.resize(cols, rows));
      observer = new ResizeObserver(() => {
        try {
          fit.fit();
        } catch {
          // The panel is closing.
        }
      });
      observer.observe(node);
      await live.start(term.cols, term.rows);
      if (stopped) {
        return;
      }
      setPhase((p) => (p === "starting" ? "live" : p));
      if (prefill) {
        // One line, typed but not run.
        // Every control character (newline, escape, backspace...) becomes a space.
        // eslint-disable-next-line no-control-regex
        live.send(prefill.replace(/[\u0000-\u001f\u007f-\u009f]+/g, " ").trim());
      }
      term.focus();
    })();
    return () => {
      stopped = true;
      observer?.disconnect();
      link?.dispose();
      term?.dispose();
    };
  }, [api, hostId, prefill, attempt]);

  return (
    <section className={styles.panel} role="dialog" aria-label={t("terminal.title", { name: shownName })} data-testid="server-terminal">
      <header className={styles.head}>
        <h2 className={styles.title}>
          <TerminalIcon aria-hidden="true" />
          {t("terminal.title", { name: shownName })}
        </h2>
        <span className={styles.state} data-state={phase} role="status">
          <i aria-hidden="true" />
          {t(`terminal.state.${phase}`)}
        </span>
        <button type="button" className={styles.close} aria-label={t("terminal.close")} title={t("terminal.close")} onClick={onClose} data-testid="terminal-close">
          <X aria-hidden="true" />
        </button>
      </header>
      <div className={styles.screen} ref={holder} />
      {phase === "failed" || phase === "ended" ? (
        <p className={styles.note} role="alert">
          {phase === "failed" ? (problem ?? t("terminal.failed")) : t("terminal.ended")}
          <button type="button" className={styles.again} onClick={() => setAttempt((n) => n + 1)}>
            {t("terminal.again")}
          </button>
        </p>
      ) : (
        <p className={styles.note}>{t("terminal.note")}</p>
      )}
    </section>
  );
}
