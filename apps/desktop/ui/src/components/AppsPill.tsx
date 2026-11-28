import { LayoutGrid } from "lucide-react";
import { useTranslation } from "react-i18next";
import { BrandMark } from "./BrandMark";
import styles from "./AppsPill.module.css";

/** Preview logos on the rail Apps control (simple-icons; see docs/design/brand-icons.md). */
const RAIL_PREVIEW_MARKS = ["discord", "gmail", "github"] as const;

/** Apps button right of the profile row: grid icon on the icon rail; three preview logos from medium up. */
export function AppsPill({ onClick }: { onClick: () => void }) {
  const { t } = useTranslation();
  const label = t("sidebar.apps");
  return (
    <button type="button" className={styles.pill} data-testid="open-apps" title={label} aria-label={label} onClick={onClick}>
      <LayoutGrid className={styles.icon} aria-hidden="true" />
      <span className={styles.label}>{label}</span>
      <span className={styles.stack} aria-hidden="true">
        {RAIL_PREVIEW_MARKS.map((name) => (
          <BrandMark key={name} name={name} size={14} monochrome />
        ))}
      </span>
    </button>
  );
}