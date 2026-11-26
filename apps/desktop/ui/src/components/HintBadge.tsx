import { TriangleAlert } from "lucide-react";
import { useId } from "react";
import styles from "./HintBadge.module.css";

/**
 * A short warning chip ("Shared address") whose one-sentence explanation shows on hover or keyboard
 * focus, instead of an orange paragraph inside the card. Screen readers get the explanation as the
 * chip's description.
 */
export function HintBadge({ label, hint, testId }: { label: string; hint: string; testId?: string }) {
  const id = useId();
  return (
    <button type="button" className={styles.badge} aria-describedby={id} data-testid={testId}>
      <TriangleAlert aria-hidden="true" />
      {label}
      <span id={id} role="tooltip" className={styles.tip}>
        {hint}
      </span>
    </button>
  );
}
