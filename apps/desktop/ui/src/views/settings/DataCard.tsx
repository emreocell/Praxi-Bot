import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { DataUsage } from "../../api/types";
import { ConfirmDialog } from "../../components/DialogParts";
import { SettingGroup, SettingRow } from "../../components/SettingsUi";
import { DATA_CATEGORY_IDS, formatSize } from "../../lib/dataUsage";
import { humanLine } from "../../lib/humanError";
import { useDaemon } from "../../state/DaemonContext";

/**
 * Settings > Diagnostics > Data: where the disk space goes, by kind of data, and a Clear cache
 * button. Clear cache only removes things the app can rebuild; chats, memory, knowledge,
 * backups and keys are never touched (the daemon decides what is removed, not this screen).
 */
export function DataCard() {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const [usage, setUsage] = useState<DataUsage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [freed, setFreed] = useState<number | null>(null);

  const load = useCallback(async () => {
    if (!client) return;
    try {
      setUsage(await client.getDataUsage());
      setError(null);
    } catch (e) {
      setError(humanLine(e));
    }
  }, [client]);

  useEffect(() => {
    void load();
  }, [load]);

  const clear = async () => {
    if (!client || busy) return;
    setBusy(true);
    try {
      const r = await client.clearCache();
      setFreed(r.freed_bytes);
      setConfirming(false);
      setError(null);
      await load();
    } catch (e) {
      setError(humanLine(e));
    } finally {
      setBusy(false);
    }
  };

  const bytesOf = (id: string) => usage?.categories.find((c) => c.id === id)?.bytes ?? 0;
  const clearable = usage?.clearable_bytes ?? 0;

  return (
    <SettingGroup>
      {error && !confirming ? (
        <p role="alert" data-testid="data-error">
          {error}
        </p>
      ) : null}
      {usage ? (
        <>
          <SettingRow title={t("settingsModal.data.total")} description={t("settingsModal.data.totalHint")}>
            <span data-testid="data-total">{formatSize(usage.total_bytes)}</span>
          </SettingRow>
          {DATA_CATEGORY_IDS.map((id) => (
            <SettingRow
              key={id}
              title={t(`settingsModal.data.category.${id}`)}
              description={t(`settingsModal.data.categoryHint.${id}`)}
            >
              <span data-testid={`data-${id}`}>{formatSize(bytesOf(id))}</span>
            </SettingRow>
          ))}
        </>
      ) : error ? null : (
        <SettingRow title={t("settingsModal.data.loading")} />
      )}
      <SettingRow
        title={t("settingsModal.data.clear.title")}
        description={
          freed !== null
            ? t("settingsModal.data.clear.done", { size: formatSize(freed) })
            : t("settingsModal.data.clear.description")
        }
      >
        <Button
          size="sm"
          variant="secondary"
          disabled={!client || busy || clearable === 0}
          data-testid="data-clear-cache"
          onClick={() => setConfirming(true)}
        >
          {clearable > 0
            ? t("settingsModal.data.clear.button", { size: formatSize(clearable) })
            : t("settingsModal.data.clear.nothing")}
        </Button>
      </SettingRow>
      <ConfirmDialog
        open={confirming}
        title={t("settingsModal.data.clear.confirmTitle")}
        body={
          <>
            <p>{t("settingsModal.data.clear.confirmRemoves")}</p>
            <p>{t("settingsModal.data.clear.confirmKeeps")}</p>
          </>
        }
        confirmLabel={t("settingsModal.data.clear.confirm")}
        busy={busy}
        error={error}
        onCancel={() => setConfirming(false)}
        onConfirm={() => void clear()}
      />
    </SettingGroup>
  );
}
