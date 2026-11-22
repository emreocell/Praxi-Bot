import { ArrowLeft, ArrowRight, Check, CircleHelp, LoaderCircle, ServerOff } from "lucide-react";
import { useEffect, useReducer, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { BotShapeId } from "ui";
import { isMockMode } from "../api/mockMode";
import type { Bot } from "../api/types";
import { saveBotAndApps } from "../lib/mcpApps";
import { DEFAULT_STOP_HOTKEY } from "../config/product";
import type { SupportedLanguage } from "../i18n";
import {
  autonomyToPreset,
  type BotDraft,
  configFromDraft,
  draftFromBot,
  parseModelRef
} from "../lib/botProfile";
import {
  canContinue,
  FIRST_RUN_STEPS,
  type FirstRunStep,
  firstRunReducer,
  INITIAL_FIRST_RUN,
  refLabel,
  FIRST_RUN_BOT_COLOR,
  FIRST_RUN_BOT_SHAPE
} from "../lib/firstRun";
import { useDaemon } from "../state/DaemonContext";
import { useEngineConsent } from "../state/EngineConsent";
import type { AppSettings } from "../store/settingsStore";
import { BotStep, type BotLook } from "./firstrun/BotStep";
import { BrainStep } from "./firstrun/BrainStep";
import { FreedomStep } from "./firstrun/FreedomStep";
import styles from "./firstrun/FirstRun.module.css";

const LANGUAGES: readonly SupportedLanguage[] = ["en", "tr", "de"];

export interface OnboardingViewProps {
  settings: AppSettings;
  onLanguageChange: (lang: SupportedLanguage) => void;
  onSkip: () => void;
  /** Opens the Help window with its short answers; omitted where there is no shell around the wizard. */
  onHelp?: () => void;
  onComplete: (result: { botId?: string }) => void;
  /** Design review and tests: open on this screen with a brain already chosen. */
  initialStep?: FirstRunStep;
}

/** Design review (mock mode only): `?frstep=freedom|bot` opens that screen with Claude Code chosen. */
function reviewStep(): FirstRunStep | undefined {
  if (!isMockMode()) {
    return undefined;
  }
  const step = new URLSearchParams(window.location.search).get("frstep");
  return step === "freedom" || step === "bot" ? step : undefined;
}

/**
 * First run: three screens, one decision each (brain, freedom, bot), "Skip for now" on every one.
 * It only ever talks to the daemon this app started on this computer.
 */
export function OnboardingView({ settings, onLanguageChange, onSkip, onHelp, onComplete, initialStep }: OnboardingViewProps) {
  const { t, i18n } = useTranslation();
  const { phase, client, bots, refreshBots, retry } = useDaemon();
  const { askEngine, applyEngine } = useEngineConsent();
  const [state, dispatch] = useReducer(firstRunReducer, undefined, () => {
    const start = initialStep ?? reviewStep();
    if (!start || start === "brain") {
      return INITIAL_FIRST_RUN;
    }
    const seeded = firstRunReducer(INITIAL_FIRST_RUN, { type: "pickCli", id: "claude", model: "cli:claude/default" });
    return start === "bot" ? firstRunReducer(firstRunReducer(seeded, { type: "next" }), { type: "next" }) : firstRunReducer(seeded, { type: "next" });
  });
  const [look, setLook] = useState<BotLook>(() => ({
    name: t("firstRun.bot.defaultName"),
    shape: FIRST_RUN_BOT_SHAPE as BotShapeId,
    color: FIRST_RUN_BOT_COLOR
  }));
  const [creating, setCreating] = useState(false);
  const [botError, setBotError] = useState<string | null>(null);
  const heading = useRef<HTMLHeadingElement | null>(null);
  const first = useRef(true);
  const created = useRef<Bot | null>(null);

  // A new screen starts at its heading, so keyboard and screen-reader users follow the flow.
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    heading.current?.focus();
  }, [state.step]);

  const at = FIRST_RUN_STEPS.indexOf(state.step);
  const last = at === FIRST_RUN_STEPS.length - 1;
  const serviceDown = phase === "error";
  const levelName = t(`newBot.autonomyLevels.${state.level}`);

  const start = async () => {
    if (!client) {
      setBotError(t("firstRun.bot.noService"));
      return;
    }
    const name = look.name.trim();
    if (!name || creating) {
      return;
    }
    setCreating(true);
    setBotError(null);
    try {
      // "Use Claude Code" and the other CLI cards: an engine that only works with its own tools
      // gets its one consent step here, before the bot exists, never as an error later.
      const answer = await askEngine(state.model, created.current?.config);
      if (answer === null) {
        return;
      }
      // Planner, classifier and fallback stay empty: the daemon assigns them from the brain.
      const draft: BotDraft = {
        ...draftFromBot(undefined),
        name,
        shape: look.shape,
        color: look.color,
        executor: parseModelRef(state.model),
        preset: autonomyToPreset(state.level)
      };
      // The same versioned write as the New-bot modal: a retry after a half-done start updates the
      // bot created a moment ago instead of creating a second one.
      const { bot } = await saveBotAndApps(
        client,
        created.current ?? undefined,
        { name, config: configFromDraft(draft, created.current ?? undefined) },
        {},
        {},
        bots.map((b) => b.id)
      );
      created.current = bot;
      await applyEngine(bot.id, answer);
      await refreshBots().catch(() => {});
      dispatch({ type: "started" });
      onComplete({ botId: bot.id });
    } catch {
      setBotError(t("firstRun.bot.createFailed"));
    } finally {
      setCreating(false);
    }
  };

  const proceed = () => {
    if (last) {
      void start();
    } else {
      dispatch({ type: "next" });
    }
  };

  return (
    <div className={styles.pane} data-testid="onboarding-view">
      <div className={styles.frame}>
        <header className={styles.top}>
          <div className={styles.lang} role="group" aria-label={t("firstRun.language")}>
            {LANGUAGES.map((lang) => (
              <button
                key={lang}
                type="button"
                className={`${styles.langBtn} ${i18n.language.startsWith(lang) ? styles.langOn : ""}`}
                aria-pressed={i18n.language.startsWith(lang)}
                onClick={() => onLanguageChange(lang)}
              >
                {lang.toUpperCase()}
              </button>
            ))}
          </div>
          <div className={styles.topActions}>
            {onHelp ? (
              <Button size="sm" variant="ghost" onClick={onHelp} data-testid="onboarding-help" title={t("firstRun.help")}>
                <CircleHelp aria-hidden="true" />
                {t("firstRun.help")}
              </Button>
            ) : null}
            <Button size="sm" variant="ghost" onClick={onSkip} data-testid="onboarding-skip" title={t("firstRun.skipTip")}>
              {t("firstRun.skip")}
            </Button>
          </div>
        </header>

        <div className={styles.surface}>
          <ol className={styles.progress} aria-label={t("firstRun.progress", { current: at + 1, total: FIRST_RUN_STEPS.length })}>
            {FIRST_RUN_STEPS.map((id, i) => (
              <li
                key={id}
                className={`${styles.pip} ${i < at ? styles.pipDone : ""} ${i === at ? styles.pipOn : ""}`}
                aria-current={i === at ? "step" : undefined}
              >
                <span className={styles.pipBar} />
                <span className={styles.pipLabel}>
                  {i < at ? <Check aria-hidden="true" /> : null}
                  {t(`firstRun.steps.${id}`)}
                </span>
              </li>
            ))}
          </ol>

          <div className={styles.head}>
            <p className={styles.eyebrow} data-testid="fr-step-of">
              {t("firstRun.progress", { current: at + 1, total: FIRST_RUN_STEPS.length })}
            </p>
            <h1 ref={heading} tabIndex={-1} className={styles.title}>
              {t(`firstRun.${state.step}.title`)}
            </h1>
            <p className={styles.subtitle}>{t(`firstRun.${state.step}.subtitle`)}</p>
          </div>

          {serviceDown ? (
            <div className={styles.service} role="alert" data-testid="fr-service-down">
              <ServerOff aria-hidden="true" />
              <div>
                <p className={styles.serviceTitle}>{t("firstRun.service.title")}</p>
                <p className={styles.serviceBody}>{t("firstRun.service.body")}</p>
              </div>
              <Button size="sm" variant="secondary" onClick={retry}>
                {t("firstRun.service.retry")}
              </Button>
            </div>
          ) : null}

          <div className={styles.body}>
            {state.step === "brain" ? <BrainStep state={state} dispatch={dispatch} /> : null}
            {state.step === "freedom" ? (
              <FreedomStep
                level={state.level}
                hotkey={settings.stopHotkey || DEFAULT_STOP_HOTKEY}
                onLevel={(level) => dispatch({ type: "setLevel", level })}
              />
            ) : null}
            {state.step === "bot" ? (
              <BotStep
                look={look}
                recap={t("firstRun.bot.recap", { model: refLabel(state.model), level: levelName })}
                error={botError}
                onLook={(patch) => setLook((l) => ({ ...l, ...patch }))}
              />
            ) : null}
          </div>

          <footer className={styles.foot}>
            {at > 0 ? (
              <Button variant="ghost" onClick={() => dispatch({ type: "back" })}>
                <ArrowLeft aria-hidden="true" />
                {t("firstRun.back")}
              </Button>
            ) : (
              <span />
            )}
            <Button
              variant="primary"
              size="lg"
              disabled={!canContinue(state) || creating || (last && look.name.trim() === "")}
              onClick={proceed}
              data-testid="fr-next"
            >
              {creating ? <LoaderCircle className={styles.spin} aria-hidden="true" /> : null}
              {last ? (creating ? t("firstRun.creating") : t("firstRun.start")) : t("firstRun.next")}
              {last ? null : <ArrowRight aria-hidden="true" />}
            </Button>
          </footer>
        </div>
      </div>
    </div>
  );
}
