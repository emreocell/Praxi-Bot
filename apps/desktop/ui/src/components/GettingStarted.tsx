import { Check, Circle } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { useGettingStarted } from "../state/useGettingStarted";
import { progressOf, type Step, type StepTarget } from "../lib/gettingStarted";
import styles from "./GettingStarted.module.css";

/** The checklist itself: pure, so it renders the same from any source of steps. */
export function GettingStartedView({
  steps,
  variant,
  hidden,
  onAction,
  onHide,
  onShow
}: {
  steps: readonly Step[];
  /** "start" = the empty chat (quiet, can be put away); "help" = always visible, can be put back. */
  variant: "start" | "help";
  hidden: boolean;
  onAction: (target: StepTarget) => void;
  onHide: () => void;
  onShow: () => void;
}) {
  const { t } = useTranslation();
  const progress = progressOf(steps);
  // Never nag: on the empty chat it is gone once put away.
  if (variant === "start" && hidden) {
    return null;
  }
  const percent = Math.round((progress.done / progress.total) * 100);
  // When every required step is done, an optional one left open is the next step.
  const nextId = progress.next ?? steps.find((st) => !st.done)?.id ?? null;
  const isNext = (step: Step) => !step.done && step.id === nextId;
  const progressText = t("gettingStarted.progress", { done: progress.done, total: progress.total });
  return (
    <section className={styles.root} aria-labelledby={`getting-started-title-${variant}`} data-testid={`getting-started-${variant}`}>
      <div className={styles.head}>
        <h2 id={`getting-started-title-${variant}`} className={styles.title}>
          {t("gettingStarted.title")}
        </h2>
        <span className={styles.count} data-testid="getting-started-count">
          {progressText}
        </span>
      </div>
      <div
        className={styles.bar}
        role="progressbar"
        aria-label={t("gettingStarted.title")}
        aria-valuemin={0}
        aria-valuemax={progress.total}
        aria-valuenow={progress.done}
        aria-valuetext={progressText}
      >
        <div className={styles.fill} style={{ width: `${percent}%` }} />
      </div>
      {progress.complete ? (
        <p className={styles.calm} role="status" data-testid="getting-started-complete">
          <Check aria-hidden="true" />
          {t("gettingStarted.allDone")}
        </p>
      ) : null}
      <ol className={styles.list}>
        {steps.map((step) => (
          <li
            key={step.id}
            className={`${styles.step} ${step.done ? styles.done : ""} ${isNext(step) ? styles.next : ""}`}
            data-testid={`getting-started-step-${step.id}`}
            data-done={step.done}
          >
            {step.done ? <Check className={styles.mark} aria-hidden="true" /> : <Circle className={styles.mark} aria-hidden="true" />}
            <div className={styles.text}>
              <span className={styles.stepTitle}>
                {step.title}
                <span className={styles.srOnly}>{` - ${t(step.done ? "gettingStarted.stateDone" : "gettingStarted.stateOpen")}`}</span>
              </span>
              {/* One obvious next step: only the first open step explains itself and has a button. */}
              {isNext(step) ? <span className={styles.body}>{step.body}</span> : null}
              {isNext(step) ? (
                <div className={styles.actions}>
                  <Button
                    // On the empty chat, "New chat" is the page's main button; in Help this is.
                    variant={variant === "help" ? "primary" : "secondary"}
                    size="sm"
                    data-testid={`getting-started-action-${step.id}`}
                    onClick={() => onAction(step.target)}
                  >
                    {step.action}
                  </Button>
                </div>
              ) : null}
            </div>
          </li>
        ))}
      </ol>
      <div className={styles.actions}>
        {variant === "help" && hidden ? (
          <Button variant="ghost" size="sm" data-testid="getting-started-show" onClick={onShow}>
            {t("gettingStarted.show")}
          </Button>
        ) : (
          <Button variant="ghost" size="sm" data-testid="getting-started-hide" onClick={onHide}>
            {t("gettingStarted.hide")}
          </Button>
        )}
      </div>
    </section>
  );
}

/** The checklist with its steps read from the app's real state. */
export function GettingStarted(props: {
  variant: "start" | "help";
  hidden: boolean;
  onAction: (target: StepTarget) => void;
  onHide: () => void;
  onShow: () => void;
}) {
  const steps = useGettingStarted();
  return <GettingStartedView steps={steps} {...props} />;
}
