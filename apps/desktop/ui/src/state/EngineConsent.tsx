import { createContext, type ReactNode, useCallback, useContext, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { Bot } from "../api/types";
import { DialogFooter, dialogStyles } from "../components/DialogParts";
import { Modal } from "../components/Modal";
import { useToast } from "../components/Toast";
import { type ApprovalLevel, engineOf } from "../lib/botEngine";
import {
  type EngineChoice,
  type EngineStatus,
  botEngineStatus,
  engineChoiceRequest,
  engineStatus,
  isFullBot
} from "../lib/engineConsent";
import { humanLine } from "../lib/humanError";
import { useDaemon } from "./DaemonContext";
import styles from "./EngineConsent.module.css";
import { OPEN_AI_SETTINGS_EVENT } from "./useCliEngineRows";

/** `"none"`: nothing to agree to. `null`: the owner said no (or the engine cannot start). */
export type EngineAnswer = EngineChoice | "none" | null;

interface EngineConsentApi {
  /** Ask, in place, for the one consent a CLI model needs before the bot saves it. */
  askEngine: (executor: string, config?: unknown) => Promise<EngineAnswer>;
  /** Apply the answer to a saved bot (after `askEngine`). */
  applyEngine: (botId: string, answer: EngineAnswer) => Promise<void>;
  /**
   * The consent step for a bot whose model cannot run yet (the "Fix" button of a failed turn, and the
   * check before a message is sent). `false`: the owner said no, or the engine cannot start.
   * `quiet`: an engine list that cannot be read is not an error (the message goes out; a failed
   * turn still offers the fix).
   */
  fixBotEngine: (bot: Bot, quiet?: boolean) => Promise<boolean>;
}

/** Without a provider (static renders, tests) nothing is asked and nothing is applied. */
const NO_ENGINE_CONSENT: EngineConsentApi = {
  askEngine: async () => "none",
  applyEngine: async () => undefined,
  fixBotEngine: async () => false
};

const Ctx = createContext<EngineConsentApi>(NO_ENGINE_CONSENT);

export function useEngineConsent(): EngineConsentApi {
  return useContext(Ctx);
}

/** Static keys keep `check:i18n` honest. */
function levelKeys(level: ApprovalLevel): { name: string; text: string } {
  switch (level) {
    case "accept_edits": return { name: "engineConsent.level.accept_edits.name", text: "engineConsent.level.accept_edits.text" };
    case "auto": return { name: "engineConsent.level.auto.name", text: "engineConsent.level.auto.text" };
    case "full": return { name: "engineConsent.level.full.name", text: "engineConsent.level.full.text" };
    default: return { name: "engineConsent.level.ask.name", text: "engineConsent.level.ask.text" };
  }
}

function blockedText(t: (key: string, o?: Record<string, string>) => string, reason: string, cli: string): string {
  switch (reason) {
    case "not_installed": return t("engineConsent.blocked.not_installed", { cli });
    case "outdated": return t("engineConsent.blocked.outdated", { cli });
    case "sign_in": return t("engineConsent.blocked.sign_in", { cli });
    default: return t("engineConsent.blocked.unsupported", { cli });
  }
}

/** The button of a "cannot start" notice: where the fix lives (Settings, AI), or none when there is no fix. */
function blockedAction(t: (key: string) => string, reason: string): { label: string; onClick: () => void } | undefined {
  const label =
    reason === "not_installed" ? t("engineConsent.action.install")
    : reason === "outdated" ? t("engineConsent.action.update")
    : reason === "sign_in" ? t("engineConsent.action.signIn")
    : null;
  return label ? { label, onClick: () => window.dispatchEvent(new CustomEvent(OPEN_AI_SETTINGS_EVENT)) } : undefined;
}

type Pending = Extract<EngineStatus, { kind: "consent" }> & { done: (a: EngineChoice | null) => void };

/**
 * The one consent step of a coding CLI that works with its own tools ("<Engine> will work with its
 * own tools. Level: Ask / Full access"), asked where the model is picked instead of failing later.
 */
export function EngineConsentProvider({ children }: { children: ReactNode }) {
  const { t } = useTranslation();
  const { client, refreshBots } = useDaemon();
  const toast = useToast();
  const [pending, setPending] = useState<Pending | null>(null);
  const clientRef = useRef(client);
  clientRef.current = client;

  const ask = useCallback(
    async (status: EngineStatus): Promise<EngineAnswer> => {
      if (status.kind === "ready") {
        return "none";
      }
      if (status.kind === "blocked") {
        toast.show(blockedText(t, status.reason, status.name), "danger", { action: blockedAction(t, status.reason), durationMs: 12000 });
        return null;
      }
      // Full access asks nothing but money: the engine's own tools are allowed at Full, no question.
      if (status.kind === "auto") {
        return { cliId: status.cliId, level: status.level };
      }
      return new Promise<EngineAnswer>((resolve) => setPending({ ...status, done: resolve }));
    },
    [t, toast]
  );

  const askEngine = useCallback(
    async (executor: string, config?: unknown): Promise<EngineAnswer> => {
      const c = clientRef.current;
      if (!c || !executor.startsWith("cli:")) {
        return "none";
      }
      let status: EngineStatus = { kind: "ready" };
      try {
        status = engineStatus(executor, engineOf(config), await c.cliEngines(), isFullBot(config));
      } catch {
        // The engine list is unavailable: save as before, a failed turn offers the fix.
      }
      return ask(status);
    },
    [ask]
  );

  const applyEngine = useCallback(
    async (botId: string, answer: EngineAnswer) => {
      const c = clientRef.current;
      if (!c || answer === null || answer === "none") {
        return;
      }
      const saved = await c.putBotEngine(botId, engineChoiceRequest(answer));
      // Say where it works, in plain words: the folder is made for the bot, never asked for.
      const folder = engineOf(saved.config).workspace;
      if (folder) {
        toast.show(t("engineConsent.notice.folder", { folder }), "info");
      }
      await refreshBots().catch(() => {});
    },
    [refreshBots, t, toast]
  );

  const fixBotEngine = useCallback(
    async (bot: Bot, quiet = false): Promise<boolean> => {
      const c = clientRef.current;
      if (!c) {
        return quiet;
      }
      let status: EngineStatus;
      try {
        status = botEngineStatus(bot.config, await c.cliEngines());
      } catch (e) {
        if (!quiet) {
          toast.show(humanLine(e), "danger");
        }
        return quiet;
      }
      try {
        const answer = await ask(status);
        if (answer === null) {
          return false;
        }
        await applyEngine(bot.id, answer);
        return true;
      } catch (e) {
        toast.show(humanLine(e), "danger");
        return false;
      }
    },
    [applyEngine, ask, toast]
  );

  const api = useMemo(() => ({ askEngine, applyEngine, fixBotEngine }), [askEngine, applyEngine, fixBotEngine]);
  return (
    <Ctx.Provider value={api}>
      {children}
      {pending ? (
        <ConsentDialog
          key={pending.cliId}
          pending={pending}
          onDone={(choice) => {
            pending.done(choice);
            setPending(null);
          }}
        />
      ) : null}
    </Ctx.Provider>
  );
}

function ConsentDialog({ pending, onDone }: { pending: Pending; onDone: (c: EngineChoice | null) => void }) {
  const { t } = useTranslation();
  const [level, setLevel] = useState<ApprovalLevel>(pending.defaultLevel);
  const [full, setFull] = useState(false);
  const cli = pending.name;
  const needsFull = level === "full";
  return (
    <Modal open onClose={() => onDone(null)} label={t("engineConsent.title", { cli })} size="sm">
      {/* The dialog frame is a row: a column root keeps body above footer (not side by side). */}
      <div className={dialogStyles.root} data-testid="engine-consent">
        <div className={styles.body}>
          <h2 className={styles.title}>{t("engineConsent.title", { cli })}</h2>
          <p className={styles.text}>{t("engineConsent.text", { cli })}</p>
          <div role="radiogroup" aria-label={t("engineConsent.levelLabel")} className={styles.levels}>
            {pending.levels.map((l) => {
              const keys = levelKeys(l);
              return (
                <label key={l} className={styles.level} data-testid={`engine-consent-level-${l}`}>
                  <input type="radio" name="engine-consent-level" checked={level === l} onChange={() => setLevel(l)} />
                  <span>
                    <strong>{t(keys.name)}</strong>
                    <span className={styles.hint}>{t(keys.text, { cli })}</span>
                  </span>
                </label>
              );
            })}
          </div>
          {pending.levels.length === 1 && level === "full" ? (
            <p className={styles.warn} role="note">{t("engineConsent.onlyFull", { cli })}</p>
          ) : null}
          {needsFull ? (
            <label className={styles.full}>
              <input type="checkbox" checked={full} onChange={(e) => setFull(e.target.checked)} data-testid="engine-consent-full" />
              <span>{t("engineConsent.fullConsent", { cli })}</span>
            </label>
          ) : null}
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={() => onDone(null)}>{t("engineConsent.notNow")}</Button>
          <Button
            variant="primary"
            disabled={needsFull && !full}
            data-autofocus
            onClick={() => onDone({ cliId: pending.cliId, level })}
            data-testid="engine-consent-allow"
          >
            {t("engineConsent.allow", { cli })}
          </Button>
        </DialogFooter>
      </div>
    </Modal>
  );
}
