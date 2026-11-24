import { Check, LoaderCircle, Square } from "lucide-react";
import { useEffect, useReducer, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { Markdown } from "../components/Markdown";
import { Modal } from "../components/Modal";
import { DemoCards } from "./DemoCards";
import {
  DEMO_STEP_MS,
  DEMO_TICK_MS,
  type DemoTaskId,
  demoReducer,
  showConnectCard,
  startDemo
} from "./demoEngine";
import { demoData } from "./demoData";
import styles from "./Demo.module.css";

function DemoRun({
  task,
  onPick,
  onClose,
  onConnect
}: {
  task: DemoTaskId;
  onPick: (task: DemoTaskId) => void;
  onClose: () => void;
  onConnect: () => void;
}) {
  const { i18n } = useTranslation();
  const data = demoData(i18n.language);
  const script = data[task];
  const ui = data.ui;
  const [state, dispatch] = useReducer(
    (s: ReturnType<typeof startDemo>, a: Parameters<typeof demoReducer>[1]) => demoReducer(s, a, script),
    task,
    startDemo
  );
  const ticking = state.status === "running";
  // Steps appear calmly, one at a time; the answer then streams quickly.
  const delay = state.steps.length < script.steps.length ? DEMO_STEP_MS : DEMO_TICK_MS;
  useEffect(() => {
    if (!ticking) {
      return undefined;
    }
    const id = window.setInterval(() => dispatch({ type: "tick" }), delay);
    return () => window.clearInterval(id);
  }, [ticking, delay]);
  const finished = showConnectCard(state);
  return (
    <div className={styles.root} data-testid="demo-run">
      <p className={styles.label} data-testid="demo-label">{ui.label}</p>
      <h2 className={styles.title}>{ui.title}</h2>
      <p className={styles.user}>{script.userText}</p>

      <section aria-label={ui.stepsHeading} className={styles.panel}>
        <h3 className={styles.heading}>{ui.stepsHeading}</h3>
        <ul className={styles.steps} aria-live="polite">
          {state.steps.map((s) => (
            <li key={s} className={styles.step}>
              <Check aria-hidden="true" />
              {s}
            </li>
          ))}
        </ul>
        {ticking && state.answer === "" ? (
          <p className={styles.text} role="status">
            <LoaderCircle aria-hidden="true" /> {ui.running}
          </p>
        ) : null}
      </section>

      {state.status === "awaitingApproval" ? (
        <section
          className={`${styles.panel} ${styles.approval}`}
          aria-label={ui.approvalHeading}
          data-testid="demo-approval"
        >
          <h3 className={styles.heading}>{ui.approvalHeading}</h3>
          <p className={styles.approvalTitle}>{script.approval.title}</p>
          <p className={styles.text}>{script.approval.detail}</p>
          <p className={styles.note}>{script.approval.note}</p>
          <div className={styles.row}>
            <Button size="sm" variant="primary" data-autofocus data-testid="demo-allow" onClick={() => dispatch({ type: "allow" })}>
              {ui.allow}
            </Button>
            <Button size="sm" variant="secondary" data-testid="demo-deny" onClick={() => dispatch({ type: "deny" })}>
              {ui.deny}
            </Button>
          </div>
        </section>
      ) : null}

      {state.answer !== "" ? (
        <section aria-label={ui.answerHeading} className={styles.panel} aria-live="polite" data-testid="demo-answer">
          <h3 className={styles.heading}>{ui.answerHeading}</h3>
          <Markdown text={state.answer} />
        </section>
      ) : null}

      {state.status === "stopped" ? (
        <p className={styles.text} role="status" data-testid="demo-stopped">{ui.stopped}</p>
      ) : null}
      {finished ? <p className={styles.note}>{ui.offlineNote}</p> : null}

      {finished ? (
        <section className={`${styles.panel} ${styles.approval}`} aria-label={ui.connectTitle} data-testid="demo-connect">
          <h3 className={styles.approvalTitle}>{ui.connectTitle}</h3>
          <p className={styles.text}>{ui.connectBody}</p>
          <div className={styles.row}>
            <Button variant="primary" size="sm" data-autofocus data-testid="demo-connect-button" onClick={onConnect}>
              {ui.connect}
            </Button>
          </div>
          <DemoCards onPick={onPick} testId="demo-more" />
        </section>
      ) : null}

      <div className={styles.actions}>
        {state.status === "running" || state.status === "awaitingApproval" ? (
          <Button size="sm" variant="secondary" data-testid="demo-stop" onClick={() => dispatch({ type: "stop" })}>
            <Square aria-hidden="true" />
            {ui.stop}
          </Button>
        ) : null}
        <Button size="sm" variant="ghost" onClick={onClose}>
          {ui.close}
        </Button>
      </div>
    </div>
  );
}

/**
 * The offline demo window. Pure screen state: it does not know the daemon, creates no chat and
 * saves nothing, so closing it leaves no trace. "Connect a model" hands over to the real flow.
 */
export function DemoModal({
  open,
  initialTask,
  onClose,
  onConnect
}: {
  open: boolean;
  initialTask: DemoTaskId | null;
  onClose: () => void;
  onConnect: () => void;
}) {
  const { i18n } = useTranslation();
  const [picked, setPicked] = useState<DemoTaskId | null>(null);
  // A new opening starts from its own task, never from the last one's.
  useEffect(() => {
    if (open) {
      setPicked(null);
    }
  }, [open, initialTask]);
  const task = picked ?? initialTask;
  const data = demoData(i18n.language);
  return (
    <Modal open={open} onClose={onClose} label={data.ui.title} size="md">
      {task ? (
        <DemoRun
          key={`${task}-${picked === null ? "i" : "p"}`}
          task={task}
          onPick={setPicked}
          onClose={onClose}
          onConnect={onConnect}
        />
      ) : (
        <div className={styles.root}>
          <h2 className={styles.title}>{data.ui.title}</h2>
          <DemoCards onPick={setPicked} />
        </div>
      )}
    </Modal>
  );
}
