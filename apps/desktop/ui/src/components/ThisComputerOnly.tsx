import { Laptop } from "lucide-react";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { useActiveHost } from "../state/useActiveHost";
import styles from "./ThisComputerOnly.module.css";

/**
 * Things that only make sense on the computer where the bot runs (local models, hardware advice).
 * While the app shows a server the server refuses those calls on purpose, so they are not made:
 * a short explanation with a way to switch to this computer is shown instead.
 */
export function ThisComputerOnly({ children }: { children: ReactNode }) {
  const { t } = useTranslation();
  const { isHost, name, switchToThisComputer } = useActiveHost();
  if (!isHost) {
    return <>{children}</>;
  }
  return (
    <section className={styles.box} data-testid="this-computer-only">
      <Laptop size={18} aria-hidden="true" />
      <div className={styles.text}>
        <p className={styles.line}>
          {t("thisComputerOnly.body", { name: name || t("computerPicker.host") })}
        </p>
        <button type="button" className={styles.btn} onClick={switchToThisComputer} data-testid="switch-this-computer">
          {t("thisComputerOnly.switch")}
        </button>
      </div>
    </section>
  );
}
