import { ArrowLeft, ShieldCheck, TriangleAlert } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { FillGrantInfo } from "../api/types";
import formStyles from "../components/Form.module.css";
import { formatDay } from "../lib/dateFormat";
import type { GrantGroup } from "../lib/vault";
import styles from "./VaultManager.module.css";

export interface VaultGrantsPanelProps {
  groups: GrantGroup[];
  loading: boolean;
  error: string | null;
  /** Id of the grant being revoked right now, if any. */
  busyId: string | null;
  onRetry: () => void;
  onRevoke: (grant: FillGrantInfo, group: GrantGroup) => void;
  onBack: () => void;
}

/** Standing grants of the vault: what they cover, one row per grant, and a Revoke action. State lives in VaultManager. */
export function VaultGrantsPanel({ groups, loading, error, busyId, onRetry, onRevoke, onBack }: VaultGrantsPanelProps) {
  const { t, i18n } = useTranslation();
  return (
    <div className={styles.drawer} data-testid="vault-grants-panel">
      <div className={styles.drawerHead}>
        <h3 className={styles.drawerTitle}>{t("vault.grants.title")}</h3>
        <Button variant="secondary" size="sm" data-testid="vault-grants-back" onClick={onBack}>
          <ArrowLeft aria-hidden="true" />
          {t("vault.grants.back")}
        </Button>
      </div>

      <div className={styles.card}>
        <p className={formStyles.hint}>{t("vault.grants.explainDoes")}</p>
        <p className={formStyles.hint}>{t("vault.grants.explainDoesNot")}</p>
        <p className={formStyles.hint}>{t("vault.grants.explainRevoke")}</p>
      </div>

      {error ? (
        <div className={styles.drawerEmpty}>
          <TriangleAlert aria-hidden="true" />
          <p role="alert">{error}</p>
          <Button variant="secondary" size="sm" onClick={onRetry}>
            {t("vault.actions.retry")}
          </Button>
        </div>
      ) : loading && groups.length === 0 ? (
        <div className={styles.drawerEmpty}>
          <p>{t("vault.actions.loading")}</p>
        </div>
      ) : groups.length === 0 ? (
        <div className={styles.drawerEmpty} role="status" data-testid="vault-grants-empty">
          <ShieldCheck aria-hidden="true" />
          <p>{t("vault.grants.empty")}</p>
        </div>
      ) : (
        <ul className={styles.grantList} aria-label={t("vault.grants.listLabel")} data-testid="vault-grants-list">
          {groups.map((group) => (
            <li key={group.name} className={styles.card}>
              <div className={styles.fieldRow}>
                <span className={styles.grantName}>{group.label}</span>
                {group.name !== group.label ? <span className={styles.drawerSub}>{group.name}</span> : null}
              </div>
              <ul className={styles.grantList}>
                {group.grants.map((grant) => (
                  <li key={grant.id} className={styles.grantRow}>
                    <div className={styles.fieldRow}>
                      <span className={styles.monoValue}>{grant.origin}</span>
                      <span className={formStyles.hint}>
                        {t("vault.grants.since", { date: formatDay(grant.created_at_ms, i18n.language) })}
                      </span>
                    </div>
                    <Button
                      variant="secondary"
                      size="sm"
                      data-testid={`vault-grant-revoke-${grant.id}`}
                      disabled={busyId === grant.id}
                      aria-label={t("vault.grants.revokeAria", {
                        action: t("vault.grants.revoke"),
                        name: group.label,
                        origin: grant.origin
                      })}
                      onClick={() => onRevoke(grant, group)}
                    >
                      {t("vault.grants.revoke")}
                    </Button>
                  </li>
                ))}
              </ul>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
