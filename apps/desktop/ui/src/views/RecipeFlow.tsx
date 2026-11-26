import {
  ArrowLeft,
  ArrowRight,
  CalendarCheck,
  CircleAlert,
  CircleCheck,
  Eye,
  FileText,
  Inbox,
  Link,
  type LucideIcon,
  RefreshCw,
  Sun,
  Table
} from "lucide-react";
import { type FormEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { EmailAccount, McpServer } from "../api/types";
import { Callout, DialogBody, DialogFooter, DialogHeader, Field } from "../components/DialogParts";
import formStyles from "../components/Form.module.css";
import { Modal } from "../components/Modal";
import { Segmented, Switch } from "../components/SettingsUi";
import { hasRealModel } from "../demo/demoEngine";
import { AUTONOMY_LEVELS, type AutonomyLevel, formatModelRef, resolveAutoExecutorRef } from "../lib/botProfile";
import { humanLine } from "../lib/humanError";
import { type LaunchProgress, launchRecipe, type LaunchStage } from "../lib/recipeLaunch";
import { BOOK } from "../lib/recipeBook";
import { blockingPrereqs, checkPrereqs, type PrereqFacts, type PrereqFix, type PrereqResult } from "../lib/recipePrereqs";
import {
  type AnswerProblem,
  type Answers,
  answerProblems,
  initialAnswers,
  levelRaised,
  pickLang,
  type Recipe,
  type RecipeNeed,
  type RecipeQuestion,
  questionText,
  recipeBotName,
  recipeFolder,
  recipeText
} from "../lib/recipes";
import { useDaemon } from "../state/DaemonContext";
import { useEngineConsent } from "../state/EngineConsent";
import { useMcp } from "../state/useMcp";
import { useProviders } from "../state/useProviders";
import { useToast } from "../components/Toast";
import cardStyles from "./NewBotModal.module.css";
import styles from "./RecipeFlow.module.css";

const ICONS: Record<string, LucideIcon> = {
  link: Link,
  inbox: Inbox,
  sun: Sun,
  eye: Eye,
  table: Table,
  "file-text": FileText
};

const LOCAL_TZ = Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
const SUBTITLE_ID = "recipe-flow-subtitle";

type Step = "pick" | "questions" | "review";

/** The steps this recipe goes through: a recipe without questions skips the form. */
function stepsFor(recipe: Recipe | null): Step[] {
  return recipe && recipe.questions.length === 0 ? ["pick", "review"] : ["pick", "questions", "review"];
}

/** The i18n key of a need's name (static keys keep `check:i18n` honest). */
function needKey(need: RecipeNeed): "model" | "browser" | "emailOwn" | "emailInbox" | "app" {
  if (need.kind === "email") return need.purpose === "own" ? "emailOwn" : "emailInbox";
  return need.kind;
}

function Flow({
  initialRecipeId,
  recheckKey,
  onFix,
  onDone,
  onClose
}: {
  initialRecipeId?: string;
  recheckKey: number;
  onFix: (fix: PrereqFix) => void;
  onDone: (botId: string, chatId: string) => void;
  onClose: () => void;
}) {
  const { t, i18n } = useTranslation();
  const toast = useToast();
  const { client, conn, health, refreshBots, refreshChats } = useDaemon();
  const { askEngine, applyEngine } = useEngineConsent();
  const { dto } = useProviders();
  const mcp = useMcp(true);
  const reloadMcp = mcp.reload;
  const lang = i18n.language;

  const initial = BOOK.recipes.find((r) => r.id === initialRecipeId) ?? null;
  const [recipe, setRecipe] = useState<Recipe | null>(initial);
  const [answers, setAnswers] = useState<Answers>(() => (initial ? initialAnswers(initial) : {}));
  const [step, setStep] = useState<Step>(initial ? stepsFor(initial)[1]! : "pick");
  const [showProblems, setShowProblems] = useState(false);
  const [level, setLevel] = useState<AutonomyLevel>(initial?.level ?? "ask");
  const [repeat, setRepeat] = useState(true);
  const [time, setTime] = useState(initial?.schedule?.time ?? "09:00");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const progress = useRef<LaunchProgress>({});

  // ---- what is connected, read fresh when the review opens, after a fix and when the window returns ----
  const [mailboxes, setMailboxes] = useState<EmailAccount[] | null>(null);
  const [composioApps, setComposioApps] = useState<PrereqFacts["composioApps"]>(null);
  const [loaded, setLoaded] = useState(false);
  const reload = useCallback(async () => {
    if (!client) return;
    const [mail, apps] = await Promise.allSettled([client.listEmailAccounts(), client.composioApps(), reloadMcp()]);
    setMailboxes(mail.status === "fulfilled" ? mail.value : null);
    // Composio not set up is "no apps connected here", and the fix (the Apps screen) is the same.
    setComposioApps(apps.status === "fulfilled" ? apps.value : []);
    setLoaded(true);
  }, [client, reloadMcp]);
  const onReview = step === "review";
  useEffect(() => {
    if (onReview) void reload();
  }, [onReview, reload, recheckKey]);
  useEffect(() => {
    if (!onReview) return undefined;
    const onFocus = () => void reload();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [onReview, reload]);

  const facts: PrereqFacts = {
    hasModel: dto === null ? null : hasRealModel(dto),
    hasBrowser: health?.host?.has_browser ?? null,
    mailboxes,
    servers: mcp.error ? null : (mcp.servers as Pick<McpServer, "name" | "status" | "enabled">[]),
    composioApps
  };
  const results = useMemo(
    () => (recipe && loaded ? checkPrereqs(recipe.needs, facts) : []),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [recipe, loaded, dto, health, mailboxes, mcp.servers, mcp.error, composioApps]
  );
  const blocked = blockingPrereqs(results).length > 0;
  const executor = useMemo(() => resolveAutoExecutorRef(dto, Boolean(conn?.fake_provider)), [dto, conn?.fake_provider]);

  const problems = recipe ? answerProblems(recipe, answers) : {};
  const pick = (r: Recipe) => {
    setRecipe(r);
    setAnswers(initialAnswers(r));
    setLevel(r.level);
    setRepeat(true);
    setTime(r.schedule?.time ?? "09:00");
    setShowProblems(false);
    setError(null);
    progress.current = {};
    setStep(stepsFor(r)[1]!);
  };
  const back = () => {
    if (busy || !recipe) return;
    const steps = stepsFor(recipe);
    setError(null);
    setStep(steps[Math.max(0, steps.indexOf(step) - 1)]!);
  };
  const toReview = () => {
    if (Object.keys(problems).length > 0) {
      setShowProblems(true);
      return;
    }
    setStep("review");
  };

  const start = async () => {
    if (!client || !recipe || busy || blocked || !loaded || executor.model.trim() === "") return;
    setBusy(true);
    setError(null);
    try {
      // A coding-CLI model needs its one consent first, as in the New-bot dialog.
      const answer = await askEngine(formatModelRef(executor));
      if (answer === null) return;
      const res = await launchRecipe(client, {
        recipe,
        book: BOOK,
        answers,
        lang,
        level,
        executor,
        schedule: recipe.schedule && repeat ? { freq: recipe.schedule.freq, time, weekday: recipe.schedule.weekday } : null,
        tz: LOCAL_TZ,
        progress: progress.current,
        onBotCreated: (bot) => applyEngine(bot.id, answer)
      });
      if (!res.ok) {
        progress.current = res.progress;
        setError(t(`recipes.error.${res.stage satisfies LaunchStage}`));
        return;
      }
      await Promise.allSettled([refreshBots(), refreshChats()]);
      if (res.schedule === "failed") toast.show(t("recipes.scheduleFailed"), "info");
      onDone(res.botId, res.chatId);
    } catch (e) {
      setError(humanLine(e));
    } finally {
      setBusy(false);
    }
  };

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    if (step === "questions") toReview();
    else if (step === "review") void start();
  };

  const steps = stepsFor(recipe);
  const text = recipe ? recipeText(recipe, lang) : null;
  const folder = recipe ? recipeFolder(recipe, answers) : null;

  return (
    <form className={styles.form} onSubmit={handleSubmit} noValidate data-step={step} data-testid="recipe-flow">
      <DialogHeader
        title={t("recipes.title")}
        subtitle={`${t(`recipes.hints.${step}`)}${step === "pick" ? "" : ` ${t("recipes.stepOf", { n: steps.indexOf(step) + 1, total: steps.length })}`}`}
        subtitleId={SUBTITLE_ID}
        closeDisabled={busy}
        onClose={onClose}
      />
      <DialogBody className={styles.body} testId="recipe-flow-body">
        {step === "pick" ? (
          <div className={styles.grid} data-testid="recipe-gallery">
            {BOOK.recipes.map((r) => {
              const Icon = ICONS[r.icon] ?? CalendarCheck;
              const rt = recipeText(r, lang);
              return (
                <button key={r.id} type="button" className={cardStyles.card} data-testid={`recipe-${r.id}`} onClick={() => pick(r)}>
                  <span className={cardStyles.cardIcon} aria-hidden="true">
                    <Icon />
                  </span>
                  <span className={cardStyles.cardText}>
                    <span className={cardStyles.cardName}>{rt.name}</span>
                    <span className={cardStyles.cardDesc}>{rt.blurb}</span>
                  </span>
                </button>
              );
            })}
          </div>
        ) : null}

        {step === "questions" && recipe && text ? (
          <>
            <p className={styles.intro}>{t("recipes.questionsIntro", { name: text.name })}</p>
            {recipe.questions.map((q) => (
              <QuestionField
                key={q.id}
                question={q}
                lang={lang}
                value={answers[q.id] ?? ""}
                problem={showProblems ? problems[q.id] : undefined}
                onChange={(v) => setAnswers((a) => ({ ...a, [q.id]: v }))}
              />
            ))}
          </>
        ) : null}

        {step === "review" && recipe && text ? (
          <>
            <section className={styles.section} aria-labelledby="recipe-needs-title">
              <h3 id="recipe-needs-title" className={styles.sectionTitle}>{t("recipes.needsTitle")}</h3>
              {!loaded ? <p className={styles.intro} role="status">{t("recipes.checking")}</p> : null}
              <ul className={styles.needs} data-testid="recipe-needs">
                {results.map((r) => (
                  <NeedRow key={`${r.need.kind}-${needKey(r.need)}`} result={r} lang={lang} onFix={onFix} />
                ))}
              </ul>
              {loaded ? (
                <div>
                  <Button variant="ghost" size="sm" type="button" onClick={() => void reload()} data-testid="recipe-recheck">
                    <RefreshCw aria-hidden="true" />
                    {t("recipes.recheck")}
                  </Button>
                </div>
              ) : null}
            </section>

            <section className={styles.section} aria-labelledby="recipe-create-title">
              <h3 id="recipe-create-title" className={styles.sectionTitle}>{t("recipes.createTitle")}</h3>
              <ul className={styles.facts} data-testid="recipe-preview">
                <li>{t("recipes.preview.bot", { name: recipeBotName(recipe, lang) })}</li>
                <li>{t("recipes.preview.task", { name: text.name })}</li>
                {folder ? <li>{t("recipes.preview.folder", { folder })}</li> : null}
                {recipe.needs.some((n) => n.kind === "email" && n.purpose === "own") ? <li>{t("recipes.preview.ownMail")}</li> : null}
              </ul>
              <Field label={t("recipes.levelTitle")} hint={t(`newBot.autonomyHints.${level}`)}>
                <Segmented
                  label={t("recipes.levelTitle")}
                  value={level}
                  options={AUTONOMY_LEVELS.map((l) => ({ value: l, label: t(`newBot.autonomyLevels.${l}`), testId: `recipe-level-${l}` }))}
                  onChange={setLevel}
                />
              </Field>
              {levelRaised(recipe, level) ? (
                <Callout tone="warning" role="status">
                  <span data-testid="recipe-level-raised">{t("recipes.levelRaised", { level: t(`newBot.autonomyLevels.${recipe.level}`) })}</span>
                </Callout>
              ) : null}
              {recipe.schedule ? (
                <div className={styles.repeat}>
                  <span className={styles.repeatText}>
                    {t(`recipes.repeat.${recipe.schedule.freq}`, { day: weekdayName(recipe.schedule.weekday ?? 1, lang) })}
                  </span>
                  <input
                    type="time"
                    className={[formStyles.input, styles.time].join(" ")}
                    value={time}
                    disabled={!repeat || busy}
                    aria-label={t("recipes.repeat.time")}
                    onChange={(e) => setTime(e.target.value)}
                  />
                  <Switch checked={repeat} onChange={setRepeat} label={t("recipes.repeat.label")} disabled={busy} />
                </div>
              ) : null}
              <p className={styles.intro}>{t("recipes.preview.later")}</p>
            </section>
          </>
        ) : null}

        {error ? (
          <Callout tone="danger" role="alert">
            {error}
          </Callout>
        ) : null}
      </DialogBody>

      <DialogFooter note={step === "review" && loaded && blocked ? t("recipes.blocked") : undefined} noteTone="warning">
        {step === "pick" ? (
          <Button variant="ghost" type="button" onClick={onClose}>
            {t("common.cancel")}
          </Button>
        ) : (
          <Button variant="ghost" type="button" disabled={busy} onClick={back} data-testid="recipe-back">
            <ArrowLeft aria-hidden="true" />
            {t("common.back")}
          </Button>
        )}
        {step === "questions" ? (
          <Button type="submit" variant="primary" data-testid="recipe-next">
            {t("recipes.next")}
            <ArrowRight aria-hidden="true" />
          </Button>
        ) : null}
        {step === "review" ? (
          <Button
            type="submit"
            variant="primary"
            aria-busy={busy}
            disabled={busy || !loaded || blocked || executor.model.trim() === ""}
            data-testid="recipe-start"
          >
            {busy ? t("recipes.starting") : t("recipes.start")}
          </Button>
        ) : null}
      </DialogFooter>
    </form>
  );
}

function problemText(t: (key: string) => string, problem: AnswerProblem, kind: RecipeQuestion["kind"]): string {
  if (problem === "required") return t("recipes.problem.required");
  if (problem === "tooLong") return t("recipes.problem.tooLong");
  if (kind === "url") return t("recipes.problem.invalidUrl");
  if (kind === "folder") return t("recipes.problem.invalidFolder");
  return t("recipes.problem.invalidChoice");
}

function weekdayName(day: number, lang: string): string {
  // 2023-01-01 was a Sunday.
  return new Intl.DateTimeFormat(lang, { weekday: "long" }).format(new Date(2023, 0, 1 + day));
}

function NeedRow({ result, lang, onFix }: { result: PrereqResult; lang: string; onFix: (fix: PrereqFix) => void }) {
  const { t } = useTranslation();
  const { need, state, optional, fix } = result;
  const key = needKey(need);
  const name = need.kind === "app" ? pickLang(need.label, lang) : t(`recipes.need.${key}.name`);
  const Mark = state === "ok" ? CircleCheck : CircleAlert;
  return (
    <li className={styles.need} data-state={state} data-testid={`recipe-need-${key}`}>
      <Mark className={styles.needMark} aria-hidden="true" />
      <span className={styles.needText}>
        <span className={styles.needName}>
          {name}
          {optional ? ` (${t("recipes.optional")})` : ""}
        </span>
        <span className={styles.needState}>
          {state === "ok" ? t("recipes.status.ok") : state === "unknown" ? t("recipes.status.unknown") : t(`recipes.need.${key}.missing`)}
        </span>
      </span>
      {fix ? (
        <Button variant={state === "missing" && !optional ? "primary" : "secondary"} size="sm" type="button" data-testid={`recipe-fix-${key}`} onClick={() => onFix(fix)}>
          {t(`recipes.need.${key}.fix`)}
        </Button>
      ) : null}
    </li>
  );
}

function QuestionField({
  question,
  lang,
  value,
  problem,
  onChange
}: {
  question: RecipeQuestion;
  lang: string;
  value: string;
  problem: AnswerProblem | undefined;
  onChange: (v: string) => void;
}) {
  const { t } = useTranslation();
  const q = questionText(question, lang);
  const id = `recipe-q-${question.id}`;
  const error = problem ? problemText(t, problem, question.kind) : undefined;
  const label = question.required ? q.label : `${q.label} (${t("recipes.optional")})`;
  return (
    <Field label={label} htmlFor={id} hint={q.hint} error={error}>
      {question.kind === "longtext" ? (
        <textarea id={id} className={formStyles.textarea} value={value} placeholder={q.placeholder} aria-invalid={problem ? true : undefined} onChange={(e) => onChange(e.target.value)} />
      ) : question.kind === "choice" ? (
        <Segmented
          label={q.label}
          value={value}
          options={(question.options ?? []).map((o) => ({ value: o, label: q.options?.[o] ?? o }))}
          onChange={onChange}
        />
      ) : (
        <input
          id={id}
          className={formStyles.input}
          type={question.kind === "url" ? "url" : "text"}
          inputMode={question.kind === "url" ? "url" : undefined}
          value={value}
          placeholder={q.placeholder}
          autoComplete="off"
          spellCheck={false}
          aria-invalid={problem ? true : undefined}
          onChange={(e) => onChange(e.target.value)}
        />
      )}
    </Field>
  );
}

/**
 * "Start from a goal": pick what you want, answer a few questions once, see what it needs and what
 * will be created, then one button creates the bot and starts the task.  Fixes for a missing
 * connection open the screen that solves it on top of this dialog, so the answers are kept.
 */
export function RecipeFlow({
  open,
  initialRecipeId,
  recheckKey,
  onFix,
  onDone,
  onClose
}: {
  open: boolean;
  initialRecipeId?: string;
  /** Bumped by the app when a fix screen closes, so the check runs again. */
  recheckKey: number;
  onFix: (fix: PrereqFix) => void;
  onDone: (botId: string, chatId: string) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  return (
    <Modal open={open} onClose={onClose} size="md" label={t("recipes.title")} describedBy={SUBTITLE_ID}>
      {open ? <Flow initialRecipeId={initialRecipeId} recheckKey={recheckKey} onFix={onFix} onDone={onDone} onClose={onClose} /> : null}
    </Modal>
  );
}
