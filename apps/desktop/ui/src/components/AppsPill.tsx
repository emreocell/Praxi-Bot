import { LayoutGrid } from "lucide-react";
import { useTranslation } from "react-i18next";
import { BrandMark } from "./BrandMark";
import styles from "./AppsPill.module.css";

/** Apps button right of the profile row: icon, the label from a large rail, connected apps' logos on a wide rail. */
export function AppsPill({ connected, onClick }: { connected: readonly string[]; onClick: () => void }) {
  const { t } = useTranslation();
  const label = t("sidebar.apps");
  return (
    <button type="button" className={styles.pill} data-testid="open-apps" title={label} aria-label={label} onClick={onClick}>
      <LayoutGrid className={styles.icon} aria-hidden="true" />
      <span className={styles.label}>{label}</span>
      {connected.length > 0 ? (
        <span className={styles.stack} aria-hidden="true">
          {connected.slice(0, 5).map((name) => (
            <BrandMark key={name} name={name} size={15} />
          ))}
        </span>
      ) : null}
    </button>
  );
}
