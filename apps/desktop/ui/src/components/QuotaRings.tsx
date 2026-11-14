import { type CSSProperties } from "react";
import { useTranslation } from "react-i18next";
import { bareBrandColor, brandIcon } from "../lib/brandIcons";
import { type AccountQuota, quotaTone, RING_RADIUS, ringDash, visibleQuotas } from "../lib/quota";
import { useQuotas } from "../state/useQuotas";
import { BrandIcon } from "./BrandIcon";
import styles from "./QuotaRings.module.css";

function Ring({ quota }: { quota: AccountQuota }) {
  const { t } = useTranslation();
  const brand = brandIcon(quota.brand);
  const tone = quotaTone(quota.remainingPct);
  const arc = tone === "low" ? "var(--color-danger)" : bareBrandColor(brand.color);
  const reset = quota.resetsAtMs
    ? t("quota.resets", { when: new Date(quota.resetsAtMs).toLocaleString() })
    : undefined;
  return (
    <li
      className={styles.ring}
      data-testid={`quota-${quota.id}`}
      data-tone={tone}
      title={reset}
      style={{ "--ring-color": arc } as CSSProperties}
    >
      <span
        className={styles.wheel}
        role="img"
        aria-label={t("quota.remaining", { label: quota.label, pct: quota.remainingPct })}
      >
        <svg className={styles.svg} viewBox="0 0 46 46" aria-hidden="true">
          <circle className={styles.track} cx="23" cy="23" r={RING_RADIUS} />
          <circle
            className={styles.arc}
            cx="23"
            cy="23"
            r={RING_RADIUS}
            strokeDasharray={ringDash(quota.remainingPct)}
          />
        </svg>
        <BrandIcon name={quota.brand} bare size={17} />
      </span>
      <span className={styles.pct} aria-hidden="true">{quota.remainingPct}%</span>
    </li>
  );
}

/** Ring row without chrome; use inside any panel. Renders nothing when empty. */
export function QuotaRings({ quotas }: { quotas: readonly AccountQuota[] }) {
  const list = visibleQuotas(quotas);
  if (list.length === 0) {
    return null;
  }
  return (
    <ul className={styles.rings} data-testid="quota-rings">
      {list.map((q) => <Ring key={q.id} quota={q} />)}
    </ul>
  );
}

/**
 * The "Kalan kota" block: a label and one ring per account in a card.
 * Self-loading and self-hiding (no data, no block), so the app shell and
 * Settings > Usage just place it.
 */
export function QuotaBlock({ className }: { className?: string }) {
  const { t } = useTranslation();
  const quotas = useQuotas();
  if (visibleQuotas(quotas).length === 0) {
    return null;
  }
  return (
    <section className={[styles.block, className ?? ""].filter(Boolean).join(" ")} aria-label={t("quota.title")}>
      <h3 className={styles.label}>{t("quota.title")}</h3>
      <div className={styles.card}>
        <QuotaRings quotas={quotas} />
      </div>
    </section>
  );
}
