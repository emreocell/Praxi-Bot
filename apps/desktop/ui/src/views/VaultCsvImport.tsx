import { CheckCircle2, FileUp, ShieldAlert, TriangleAlert } from "lucide-react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button } from "ui";
import type { Bot } from "../api/types";
import formStyles from "../components/Form.module.css";
import { Switch } from "../components/SettingsUi";
import { useToast } from "../components/Toast";
import { botView } from "../lib/botProfile";
import { humanLine } from "../lib/humanError";
import { parseVaultCsv, suggestSecretName, type VaultCsvParseResult } from "../lib/vault";
import { useDaemon } from "../state/DaemonContext";
import styles from "./VaultManager.module.css";

/**
 * CSV importer for Chrome and Bitwarden exports.
 * Shows an explicit warning before importing and prompts the owner to
 * securely delete the unencrypted CSV file right after import completes.
 * Never logs or persists parsed passwords.
 */
export function VaultCsvImport({
  existingNames,
  bots,
  onImported,
  onCancel,
}: {
  existingNames: readonly string[];
  bots: Bot[];
  onImported: () => Promise<void>;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const toast = useToast();
  const fileRef = useRef<HTMLInputElement | null>(null);
  const [parsed, setParsed] = useState<VaultCsvParseResult | null>(null);
  const [selectedBots, setSelectedBots] = useState<string[]>([]);
  const [askBeforeUse, setAskBeforeUse] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [doneCount, setDoneCount] = useState<number | null>(null);

  const wipeParsed = () => {
    setParsed((prev) => {
      if (prev) {
        for (const r of prev.rows) r.password = "";
      }
      return null;
    });
    if (fileRef.current) fileRef.current.value = "";
  };

  const onFileChange = async (file: File | undefined) => {
    setError(null);
    setDoneCount(null);
    if (!file) {
      wipeParsed();
      return;
    }
    try {
      const text = await file.text();
      const res = parseVaultCsv(text);
      if (res.format === "unknown" || res.rows.length === 0) {
        wipeParsed();
        setError(t("vault.import.unrecognized"));
        return;
      }
      setParsed(res);
    } catch (e) {
      wipeParsed();
      setError(humanLine(e));
    } finally {
      if (fileRef.current) fileRef.current.value = "";
    }
  };

  const runImport = async () => {
    if (!client || !parsed || parsed.rows.length === 0 || busy) return;
    setBusy(true);
    setError(null);
    const taken = [...existingNames];
    let imported = 0;
    try {
      for (const row of parsed.rows) {
        const name = suggestSecretName(row.siteName, "login", taken);
        taken.push(name);
        await client.createFillSecret({
          name,
          value: row.password,
          kind: "login",
          site_name: row.siteName,
          login_id: row.loginId || undefined,
          note: row.note || undefined,
          origins: row.origin ? [row.origin] : [],
          bot_ids: selectedBots,
          ask_before_use: askBeforeUse,
        });
        imported += 1;
      }
      wipeParsed();
      setDoneCount(imported);
      await onImported();
      toast.show(t("vault.import.toastDone", { count: imported }), "success");
    } catch (e) {
      setError(humanLine(e));
    } finally {
      setBusy(false);
    }
  };

  if (doneCount !== null) {
    return (
      <div className={styles.drawer} data-testid="vault-import-done">
        <div className={styles.drawerHead}>
          <div className={styles.drawerTitleGroup}>
            <span className={styles.identityIcon}>
              <CheckCircle2 aria-hidden="true" />
            </span>
            <div>
              <h3 className={styles.drawerTitle}>{t("vault.import.doneTitle", { count: doneCount })}</h3>
              <span className={styles.subtitle}>{t("vault.import.doneSubtitle")}</span>
            </div>
          </div>
        </div>
        <div className={styles.warnBox} role="alert" data-testid="vault-secure-delete-prompt">
          <ShieldAlert aria-hidden="true" />
          <div>
            <strong>{t("vault.import.deletePromptTitle")}</strong>
            <p className={styles.noteText}>{t("vault.import.deletePromptBody")}</p>
          </div>
        </div>
        <div className={styles.formActions}>
          <Button variant="primary" size="sm" data-testid="vault-confirm-csv-deleted" onClick={onCancel}>
            {t("vault.import.deletePromptAck")}
          </Button>
        </div>
      </div>
    );
  }

  return (
    <div className={styles.drawer} data-testid="vault-import-drawer">
      <div className={styles.drawerHead}>
        <div className={styles.drawerTitleGroup}>
          <span className={styles.identityIcon}>
            <FileUp aria-hidden="true" />
          </span>
          <div>
            <h3 className={styles.drawerTitle}>{t("vault.import.title")}</h3>
            <span className={styles.subtitle}>{t("vault.import.subtitle")}</span>
          </div>
        </div>
      </div>

      <div className={styles.warnBox} role="note">
        <TriangleAlert aria-hidden="true" />
        <div>
          <strong>{t("vault.import.warningTitle")}</strong>
          <p className={styles.noteText}>{t("vault.import.warningBody")}</p>
        </div>
      </div>

      <div className={styles.fileDrop}>
        <label className={formStyles.label} htmlFor="vault-csv-input">
          {t("vault.import.chooseFile")}
        </label>
        <input
          id="vault-csv-input"
          ref={fileRef}
          className={styles.fileInput}
          data-testid="vault-csv-file"
          type="file"
          accept=".csv,text/csv"
          disabled={busy}
          onChange={(e) => void onFileChange(e.target.files?.[0])}
        />
        <span className={formStyles.hint}>{t("vault.import.formatsHint")}</span>
      </div>

      {parsed ? (
        <div className={styles.card}>
          <div className={styles.fieldRow}>
            <span className={styles.fieldLabel}>
              {t("vault.import.previewTitle", {
                format: parsed.format === "bitwarden" ? "Bitwarden" : "Chrome",
                count: parsed.rows.length,
              })}
            </span>
            {parsed.skipped > 0 ? (
              <span className={formStyles.hint}>
                {t("vault.import.skippedCount", { count: parsed.skipped })}
              </span>
            ) : null}
          </div>
          <ul className={styles.previewList} data-testid="vault-import-preview">
            {parsed.rows.slice(0, 20).map((row, idx) => (
              <li key={`${row.siteName}-${idx}`} className={styles.previewRow}>
                <span>
                  <strong>{row.siteName}</strong>
                  {row.loginId ? ` · ${row.loginId}` : ""}
                </span>
                <span className={styles.drawerSub}>{row.origin ?? t("vault.noOrigin")}</span>
              </li>
            ))}
            {parsed.rows.length > 20 ? (
              <li className={styles.previewRow}>
                <span className={formStyles.hint}>
                  {t("vault.import.andMore", { count: parsed.rows.length - 20 })}
                </span>
              </li>
            ) : null}
          </ul>

          <div className={styles.fieldRow}>
            <span className={styles.fieldLabel}>{t("vault.detail.botsLabel")}</span>
            {bots.length === 0 ? (
              <span className={formStyles.hint}>{t("vault.detail.noBots")}</span>
            ) : (
              <ul className={styles.botList}>
                {bots.map((b) => {
                  const view = botView(b);
                  const checked = selectedBots.includes(b.id);
                  return (
                    <li key={b.id}>
                      <label className={styles.botItem}>
                        <input
                          type="checkbox"
                          className={styles.botCheck}
                          checked={checked}
                          onChange={(e) =>
                            setSelectedBots((cur) =>
                              e.target.checked
                                ? [...cur, b.id]
                                : cur.filter((id) => id !== b.id),
                            )
                          }
                        />
                        <BotAvatar bot={view} size="sm" decorative />
                        <span>{view.name}</span>
                      </label>
                    </li>
                  );
                })}
              </ul>
            )}
          </div>

          <div className={styles.switchRow}>
            <div className={styles.switchText}>
              <span className={styles.switchTitle}>{t("vault.detail.askTitle")}</span>
              <span className={styles.switchDesc}>{t("vault.detail.askHintOn")}</span>
            </div>
            <Switch
              label={t("vault.detail.askTitle")}
              checked={askBeforeUse}
              onChange={setAskBeforeUse}
            />
          </div>
        </div>
      ) : null}

      {error ? (
        <p className={formStyles.error} role="alert">
          {error}
        </p>
      ) : null}

      <div className={styles.formActions}>
        <Button
          variant="secondary"
          size="sm"
          disabled={busy}
          onClick={() => {
            wipeParsed();
            onCancel();
          }}
        >
          {t("common.cancel")}
        </Button>
        <Button
          variant="primary"
          size="sm"
          disabled={!parsed || parsed.rows.length === 0 || busy}
          data-testid="vault-import-submit"
          onClick={() => void runImport()}
        >
          {busy
            ? t("vault.import.importing")
            : t("vault.import.confirm", { count: parsed?.rows.length ?? 0 })}
        </Button>
      </div>
    </div>
  );
}
