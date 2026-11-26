import { Check } from "lucide-react";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { stepStates } from "../lib/setupChecklist";
import styles from "./SetupChecklist.module.css";

export interface SetupStep {
  id: string;
  title: string;
  /** One sentence, shown only while this is the next step. */
  hint: string;
  done: boolean;
  action?: { label: string; onClick: () => void; icon?: ReactNode; testId?: string };
}

/**
 * Numbered first-run steps with one obvious next step: done steps get a check, the next one its
 * sentence and the only primary button, later ones just their name.
 */
export function SetupChecklist({ steps, label, testId }: { steps: readonly SetupStep[]; label: string; testId?: string }) {
  const { t } = useTranslation();
  const states = stepStates(steps.map((s) => s.done));
  return (
    <ol className={styles.list} aria-label={label} data-testid={testId}>
      {steps.map((step, i) => {
        const state = states[i];
        return (
          <li key={step.id} className={styles.step} data-state={state} data-testid={`setup-step-${step.id}`}>
            <span className={styles.marker} aria-hidden="true">
              {state === "done" ? <Check /> : i + 1}
            </span>
            <div className={styles.text}>
              <span className={styles.title}>
                {step.title}
                <span className={styles.srOnly}>{` - ${t(`setup.state.${state}`)}`}</span>
                {state === "next" ? <span className={styles.nextTag} aria-hidden="true">{t("setup.state.next")}</span> : null}
              </span>
              {state === "next" ? <span className={styles.hint}>{step.hint}</span> : null}
              {state === "next" && step.action ? (
                <div>
                  <Button variant="primary" size="sm" data-testid={step.action.testId} onClick={step.action.onClick}>
                    {step.action.icon}
                    {step.action.label}
                  </Button>
                </div>
              ) : null}
            </div>
          </li>
        );
      })}
    </ol>
  );
}
