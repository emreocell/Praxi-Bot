import { Check, Info, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Card } from "ui";
import type { DetectedProviderDto } from "../../api/types";
import { BrandIcon } from "../../components/BrandIcon";
import formStyles from "../../components/Form.module.css";
import { useToast } from "../../components/Toast";
import {
  configuredCliIds,
  detectedRowAction,
  signInCommand
} from "../../lib/detectedAccounts";
import { useDaemon } from "../../state/DaemonContext";
import { notifyProvidersChanged, useProviders } from "../../state/useProviders";
import styles from "./DetectedAccountsBlock.module.css";

function authLabelKey(auth: DetectedProviderDto["auth"]) {
  switch (auth) {
    case "signed_in":
      return "settingsModal.ai.detected.authSignedIn" as const;
    case "signed_out":
      return "settingsModal.ai.detected.authSignedOut" as const;
    default:
      return "settingsModal.ai.detected.authUnknown" as const;
  }
}

/**
 * Lists coding CLIs detected by the daemon (`GET /v1/providers/detected`)
 * and lets the user enable runnable ones (`POST /v1/providers/cli/enable`).
 */
export function DetectedAccountsBlock() {
  const { t } = useTranslation();
  const toast = useToast();
  const { client } = useDaemon();
  const { dto, reload: reloadProviders } = useProviders();
  const [rows, setRows] = useState<DetectedProviderDto[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [enabling, setEnabling] = useState<string | null>(null);
  const [enableError, setEnableError] = useState<string | null>(null);

  const enabledCli = useMemo(() => configuredCliIds(dto), [dto]);

  const loadDetected = useCallback(
    async (refresh: boolean) => {
      if (!client) {
        setRows([]);
        setLoadError(null);
        return;
      }
      setLoading(true);
      setLoadError(null);
      try {
        setRows(await client.detectedProviders(refresh));
      } catch (e) {
        setLoadError(
          e instanceof Error && e.message
            ? e.message
            : t("settingsModal.ai.detected.detectError")
        );
      } finally {
        setLoading(false);
      }
    },
    [client, t]
  );

  useEffect(() => {
    void loadDetected(false);
  }, [loadDetected]);

  const onEnable = async (row: DetectedProviderDto) => {
    if (!client) {
      return;
    }
    setEnabling(row.id);
    setEnableError(null);
    try {
      await client.enableCliProvider(row.id);
      await reloadProviders();
      notifyProvidersChanged();
      toast.show(
        t("settingsModal.ai.detected.enabledToast", { name: row.display_name }),
        "success"
      );
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setEnableError(msg);
      toast.show(msg, "danger");
    } finally {
      setEnabling(null);
    }
  };

  return (
    <Card padding="md" className={styles.card} data-testid="detected-accounts-block">
      <div className={styles.header}>
        <div className={styles.info}>
          <h2 className={styles.heading}>
            {t("settingsModal.ai.detected.title")}
          </h2>
          <p className={formStyles.hint}>
            {t("settingsModal.ai.detected.description")}
          </p>
        </div>
        <Button
          size="sm"
          variant="secondary"
          disabled={!client || loading}
          onClick={() => void loadDetected(true)}
        >
          <RefreshCw size={14} aria-hidden="true" />
          {t("settingsModal.ai.detected.refresh")}
        </Button>
      </div>

      <p className={styles.notice}>
        <Info size={14} aria-hidden="true" />
        <span>{t("settingsModal.ai.detected.termsNotice")}</span>
      </p>

      {!client ? (
        <p className={formStyles.hint}>
          {t("settingsModal.ai.detected.offline")}
        </p>
      ) : loadError ? (
        <div className={styles.errorRow} role="alert">
          <span>
            {t("settingsModal.ai.detected.detectError")} ({loadError})
          </span>
          <Button
            size="sm"
            variant="secondary"
            disabled={loading}
            onClick={() => void loadDetected(true)}
          >
            {t("settingsModal.ai.detected.retry")}
          </Button>
        </div>
      ) : (
        <ul className={styles.list}>
          {rows.map((row) => {
            const action = detectedRowAction(row, enabledCli);
            const brandName = row.id === "claude" ? "claude" : row.display_name;
            return (
              <li key={row.id} className={styles.row}>
                <div className={styles.identity}>
                  <BrandIcon name={brandName} />
                  <div className={styles.details}>
                    <div className={styles.titleLine}>
                      <span className={styles.name}>{row.display_name}</span>
                      {row.installed && row.version ? (
                        <span className={styles.version}>{row.version}</span>
                      ) : null}
                      {row.installed ? (
                        <span
                          className={`${styles.badge} ${
                            row.auth === "signed_in"
                              ? styles.badgeOk
                              : styles.badgeMuted
                          }`}
                        >
                          {t(authLabelKey(row.auth))}
                        </span>
                      ) : null}
                    </div>
                    {row.account_label ? (
                      <span className={styles.meta}>{row.account_label}</span>
                    ) : null}
                  </div>
                </div>

                <div className={styles.action}>
                  {action.kind === "enable" ? (
                    <Button
                      size="sm"
                      variant="primary"
                      disabled={enabling === row.id}
                      onClick={() => void onEnable(row)}
                    >
                      {t("settingsModal.ai.detected.enable")}
                    </Button>
                  ) : action.kind === "enabled" ? (
                    <Button size="sm" variant="secondary" disabled>
                      <Check size={14} aria-hidden="true" />
                      {t("settingsModal.ai.detected.enabled")}
                    </Button>
                  ) : action.kind === "not_installed" ? (
                    <span className={styles.statusText}>
                      {t("settingsModal.ai.detected.notInstalled")}
                    </span>
                  ) : action.kind === "sign_in_hint" ? (
                    <span className={styles.statusText}>
                      {t("settingsModal.ai.detected.signInHint", {
                        command: signInCommand(row.id)
                      })}
                    </span>
                  ) : action.kind === "codex_unconfinable" ? (
                    <span className={styles.statusWarn}>
                      {t("settingsModal.ai.detected.codexUnconfinable")}
                    </span>
                  ) : (
                    <span className={styles.statusText}>
                      {t("settingsModal.ai.detected.notRunnable")}
                    </span>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      )}

      {enableError ? (
        <p className={formStyles.error} role="alert">
          {enableError}
        </p>
      ) : null}
    </Card>
  );
}
