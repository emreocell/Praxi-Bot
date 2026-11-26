import { FolderOpen, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { isTauriRuntime, openWorkspaceFolder } from "../../api/connection";
import type {
  BotWorkspaceSummaryDto,
  CleanupTarget,
  WorkspacesOverviewDto
} from "../../api/types";
import { SettingGroup, SettingRow } from "../../components/SettingsUi";
import { useToast } from "../../components/Toast";
import { humanLine } from "../../lib/humanError";
import { bytesToGbInput, formatBytes, gbInputToBytes } from "../../lib/workspaceFormat";
import { useDaemon } from "../../state/DaemonContext";
import styles from "./WorkspaceStorageCard.module.css";

interface LimitDraft {
  soft: string;
  hard: string;
}

export function WorkspaceStorageCard() {
  const { t } = useTranslation();
  const toast = useToast();
  const { client, activeComputerId } = useDaemon();
  const [overview, setOverview] = useState<WorkspacesOverviewDto | null>(null);
  const [loading, setLoading] = useState(false);
  const [busyKey, setBusyKey] = useState<string | null>(null);
  const [limitDrafts, setLimitDrafts] = useState<Record<string, LimitDraft>>({});
  const [w1, setW1] = useState("80");
  const [w2, setW2] = useState("90");
  const [w3, setW3] = useState("95");
  const [minFreeGb, setMinFreeGb] = useState("5");
  const [dlDays, setDlDays] = useState("30");
  const [logDays, setLogDays] = useState("30");

  const syncForm = (data: WorkspacesOverviewDto) => {
    setW1(String(data.settings.warn_percent_1));
    setW2(String(data.settings.warn_percent_2));
    setW3(String(data.settings.warn_percent_3));
    setMinFreeGb(bytesToGbInput(data.settings.min_free_bytes) || "5");
    setDlDays(data.settings.downloads_max_age_days ? String(data.settings.downloads_max_age_days) : "");
    setLogDays(data.settings.logs_max_age_days ? String(data.settings.logs_max_age_days) : "");
    const nextDrafts: Record<string, LimitDraft> = {};
    for (const bot of data.bots) {
      nextDrafts[bot.bot_id] = {
        soft: bytesToGbInput(bot.soft_limit_bytes),
        hard: bytesToGbInput(bot.hard_limit_bytes)
      };
    }
    setLimitDrafts(nextDrafts);
  };

  const load = useCallback(async () => {
    if (!client) return;
    setLoading(true);
    try {
      const data = await client.listWorkspaces();
      setOverview(data);
      syncForm(data);
    } catch (err) {
      toast.show(humanLine(err), "danger");
    } finally {
      setLoading(false);
    }
  }, [client, toast]);

  useEffect(() => {
    void load();
  }, [load]);

  const openRootFolder = async (folderName?: string) => {
    if (isTauriRuntime() && activeComputerId === "local") {
      try {
        await openWorkspaceFolder(folderName);
        return;
      } catch (err) {
        toast.show(humanLine(err), "danger");
        return;
      }
    }
    if (overview?.root_path) {
      toast.show(folderName ? `${overview.root_path}/${folderName}` : overview.root_path, "info");
    }
  };

  const handleCleanup = async (target: CleanupTarget) => {
    if (!client) return;
    setBusyKey(`clean:${target}`);
    try {
      const res = await client.runWorkspaceCleanup({ target });
      toast.show(
        t("workspace.cleanedSummary", { size: formatBytes(res.freed_bytes) }),
        "success"
      );
      await load();
    } catch (err) {
      toast.show(humanLine(err), "danger");
    } finally {
      setBusyKey(null);
    }
  };

  const saveBotLimits = async (bot: BotWorkspaceSummaryDto) => {
    if (!client) return;
    const draft = limitDrafts[bot.bot_id] ?? { soft: "", hard: "" };
    const soft = gbInputToBytes(draft.soft);
    const hard = gbInputToBytes(draft.hard);
    if (soft === undefined || hard === undefined) {
      toast.show(t("workspace.invalidLimitValue"), "danger");
      return;
    }
    if (soft !== null && hard !== null && soft > hard) {
      toast.show(t("workspace.softExceedsHard"), "danger");
      return;
    }
    setBusyKey(`bot:${bot.bot_id}`);
    try {
      await client.putBotWorkspaceLimits(bot.bot_id, {
        soft_limit_bytes: soft,
        hard_limit_bytes: hard
      });
      toast.show(t("workspace.saved"), "success");
      await load();
    } catch (err) {
      toast.show(humanLine(err), "danger");
    } finally {
      setBusyKey(null);
    }
  };

  const saveSettings = async () => {
    if (!client) return;
    const p1 = Number(w1);
    const p2 = Number(w2);
    const p3 = Number(w3);
    const minFree = gbInputToBytes(minFreeGb);
    const dl = dlDays.trim() ? Number(dlDays) : null;
    const lg = logDays.trim() ? Number(logDays) : null;
    if (
      !Number.isInteger(p1) ||
      !Number.isInteger(p2) ||
      !Number.isInteger(p3) ||
      p1 < 1 ||
      p1 >= p2 ||
      p2 >= p3 ||
      p3 > 99 ||
      !minFree
    ) {
      toast.show(t("workspace.invalidThresholds"), "danger");
      return;
    }
    if ((dl !== null && (!Number.isInteger(dl) || dl < 1)) || (lg !== null && (!Number.isInteger(lg) || lg < 1))) {
      toast.show(t("workspace.invalidThresholds"), "danger");
      return;
    }
    setBusyKey("settings");
    try {
      await client.updateWorkspaceSettings({
        warn_percent_1: p1,
        warn_percent_2: p2,
        warn_percent_3: p3,
        min_free_bytes: minFree,
        downloads_max_age_days: dl,
        logs_max_age_days: lg
      });
      toast.show(t("workspace.saved"), "success");
      await load();
    } catch (err) {
      toast.show(humanLine(err), "danger");
    } finally {
      setBusyKey(null);
    }
  };

  const disk = overview?.disk;
  const meterClass = [
    styles.meterFill,
    disk && overview && disk.used_percent >= overview.settings.warn_percent_3
      ? styles.meterFillCritical
      : disk && overview && disk.used_percent >= overview.settings.warn_percent_1
        ? styles.meterFillWarn
        : ""
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div className={styles.wrap} data-testid="workspace-storage-card">
      <SettingGroup>
        <SettingRow
          title={t("workspace.storageHeading")}
          description={overview?.root_path ?? undefined}
          stacked
        >
          {disk ? (
            <>
              <div className={styles.meter} aria-hidden="true">
                <div className={meterClass} style={{ width: `${Math.min(100, disk.used_percent)}%` }} />
              </div>
              <div className={styles.diskMeta}>
                <span>
                  {t("workspace.diskSummary", {
                    percent: disk.used_percent,
                    free: formatBytes(disk.available_bytes),
                    total: formatBytes(disk.total_bytes),
                    botsTotal: formatBytes(overview?.total_workspace_bytes ?? 0)
                  })}
                </span>
                {overview?.root_path ? (
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => void openRootFolder()}
                    data-testid="workspace-open-root-folder"
                  >
                    <FolderOpen aria-hidden="true" />
                    {t("workspace.openFolder")}
                  </Button>
                ) : null}
              </div>
            </>
          ) : (
            <p className={styles.hint}>
              {loading ? t("workspace.loading") : t("workspace.unconfiguredHint")}
            </p>
          )}
        </SettingRow>

        {overview && (overview.suggestions.length > 0 || overview.largest_items.length > 0) ? (
          <SettingRow title={t("workspace.cleanupHeading")} description={t("workspace.cleanupDesc")} stacked>
            <div className={styles.cleanupList}>
              {overview.suggestions.map((s) => (
                <Button
                  key={s.target}
                  size="sm"
                  variant="ghost"
                  disabled={busyKey !== null}
                  onClick={() => void handleCleanup(s.target)}
                  data-testid={`workspace-cleanup-${s.target}`}
                >
                  <Trash2 aria-hidden="true" />
                  {t(`workspace.cleanupTarget.${s.target}`, { size: formatBytes(s.reclaimable_bytes) })}
                </Button>
              ))}
            </div>
            {overview.largest_items.length > 0 ? (
              <div className={styles.bigList}>
                {overview.largest_items.slice(0, 3).map((item) => (
                  <div key={`${item.folder_name}/${item.relative_path}`} className={styles.bigRow}>
                    <span>{`${item.folder_name}/${item.relative_path}`}</span>
                    <span>{formatBytes(item.size_bytes)}</span>
                  </div>
                ))}
              </div>
            ) : null}
          </SettingRow>
        ) : null}
      </SettingGroup>

      {overview && overview.bots.length > 0 ? (
        <SettingGroup>
          {overview.bots.map((bot) => {
            const draft = limitDrafts[bot.bot_id] ?? { soft: "", hard: "" };
            return (
              <SettingRow
                key={bot.bot_id}
                title={bot.bot_name}
                description={t("workspace.botRowDesc", {
                  folder: bot.folder_name,
                  used: formatBytes(bot.used_bytes),
                  files: bot.file_count
                })}
              >
                <div className={styles.botLimits}>
                  <label className={styles.hint}>
                    {t("workspace.softLimitGb")}{" "}
                    <input
                      type="text"
                      inputMode="decimal"
                      className={styles.compactInput}
                      value={draft.soft}
                      placeholder={t("workspace.limitOff")}
                      onChange={(e) =>
                        setLimitDrafts((prev) => ({
                          ...prev,
                          [bot.bot_id]: { ...draft, soft: e.target.value }
                        }))
                      }
                      aria-label={`${bot.bot_name} ${t("workspace.softLimitGb")}`}
                    />
                  </label>
                  <label className={styles.hint}>
                    {t("workspace.hardLimitGb")}{" "}
                    <input
                      type="text"
                      inputMode="decimal"
                      className={styles.compactInput}
                      value={draft.hard}
                      placeholder={t("workspace.limitOff")}
                      onChange={(e) =>
                        setLimitDrafts((prev) => ({
                          ...prev,
                          [bot.bot_id]: { ...draft, hard: e.target.value }
                        }))
                      }
                      aria-label={`${bot.bot_name} ${t("workspace.hardLimitGb")}`}
                    />
                  </label>
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={busyKey !== null}
                    onClick={() => void saveBotLimits(bot)}
                  >
                    {t("common.save")}
                  </Button>
                </div>
              </SettingRow>
            );
          })}
        </SettingGroup>
      ) : null}

      <SettingGroup>
        <SettingRow
          title={t("workspace.thresholdsTitle")}
          description={t("workspace.thresholdsDesc")}
          stacked
        >
          <div className={styles.thresholdInputs}>
            <label className={styles.hint}>
              %
              <input
                type="number"
                min={1}
                max={99}
                className={styles.compactInput}
                value={w1}
                onChange={(e) => setW1(e.target.value)}
                aria-label={t("workspace.warnPct1")}
              />
            </label>
            <input
              type="number"
              min={1}
              max={99}
              className={styles.compactInput}
              value={w2}
              onChange={(e) => setW2(e.target.value)}
              aria-label={t("workspace.warnPct2")}
            />
            <input
              type="number"
              min={1}
              max={99}
              className={styles.compactInput}
              value={w3}
              onChange={(e) => setW3(e.target.value)}
              aria-label={t("workspace.warnPct3")}
            />
            <label className={styles.hint}>
              {t("workspace.minFreeGb")}{" "}
              <input
                type="text"
                inputMode="decimal"
                className={styles.compactInput}
                value={minFreeGb}
                onChange={(e) => setMinFreeGb(e.target.value)}
                aria-label={t("workspace.minFreeGb")}
              />
            </label>
          </div>
        </SettingRow>
        <SettingRow
          title={t("workspace.retentionTitle")}
          description={t("workspace.retentionDesc")}
          stacked
        >
          <div className={styles.thresholdInputs}>
            <label className={styles.hint}>
              {t("workspace.downloadsDays")}{" "}
              <input
                type="number"
                min={1}
                max={3650}
                className={styles.compactInput}
                value={dlDays}
                placeholder={t("workspace.limitOff")}
                onChange={(e) => setDlDays(e.target.value)}
              />
            </label>
            <label className={styles.hint}>
              {t("workspace.logsDays")}{" "}
              <input
                type="number"
                min={1}
                max={3650}
                className={styles.compactInput}
                value={logDays}
                placeholder={t("workspace.limitOff")}
                onChange={(e) => setLogDays(e.target.value)}
              />
            </label>
            <Button
              size="sm"
              variant="primary"
              disabled={busyKey !== null}
              onClick={() => void saveSettings()}
              data-testid="workspace-save-settings"
            >
              {t("common.save")}
            </Button>
          </div>
        </SettingRow>
      </SettingGroup>
    </div>
  );
}
