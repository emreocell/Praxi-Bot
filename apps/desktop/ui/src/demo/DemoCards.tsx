import { useTranslation } from "react-i18next";
import { DEMO_TASK_IDS, type DemoTaskId } from "./demoEngine";
import { demoData } from "./demoData";
import styles from "./Demo.module.css";

/**
 * Three sample tasks as cards. They start the offline demo, never a real chat: nothing is
 * sent to a model, so they are safe to press with no key.
 */
export function DemoCards({
  onPick,
  heading,
  testId = "demo-cards"
}: {
  onPick: (task: DemoTaskId) => void;
  heading?: boolean;
  testId?: string;
}) {
  const { i18n } = useTranslation();
  const data = demoData(i18n.language);
  return (
    <section className={styles.section} aria-label={data.ui.title} data-testid={testId}>
      {heading ? <h2 className={styles.cardsHeading}>{data.ui.title}</h2> : null}
      <p className={styles.note}>{data.ui.intro}</p>
      <div className={styles.cards}>
        {DEMO_TASK_IDS.map((id) => (
          <button
            key={id}
            type="button"
            className={styles.card}
            data-testid={`demo-card-${id}`}
            onClick={() => onPick(id)}
          >
            <span className={styles.cardTitle}>{data[id].title}</span>
            <span className={styles.cardHint}>{data[id].hint}</span>
          </button>
        ))}
      </div>
    </section>
  );
}
