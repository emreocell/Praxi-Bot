import { getVersion } from "@tauri-apps/api/app";
import type { TFunction } from "i18next";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { SettingGroup, SettingRow, SettingsCard } from "../../components/SettingsUi";
import { PRODUCT_CONFIG } from "../../config/product";
import { isTauriRuntime } from "../../overlay/events";
import { useDaemon } from "../../state/DaemonContext";
import { UsageView } from "../UsageView";
import { DataCard } from "./DataCard";
import { DiagnosticsCard } from "./DiagnosticsCard";
import { RecentLogs } from "../../components/RecentLogs";
import { SafeModeCard } from "./SafeModeCard";
import { UpdatePanel } from "./UpdatePanel";
import type { CardDef } from "./types";

function VersionBlock() {
  const { t } = useTranslation();
  const { health } = useDaemon();
  const [app, setApp] = useState<string>("");

  useEffect(() => {
    if (!isTauriRuntime()) {
      return;
    }
    void getVersion()
      .then(setApp)
      .catch(() => undefined);
  }, []);

  return (
    <SettingGroup>
      <SettingRow
        title={PRODUCT_CONFIG.appName}
        description={t("settingsModal.updates.appVersion")}
      >
        <span data-testid="app-version">
          {app || t("settingsModal.updates.devBuild")}
        </span>
      </SettingRow>
      <SettingRow
        title={t("settingsModal.updates.daemon")}
        description={t("settingsModal.updates.daemonDescription")}
      >
        <span data-testid="daemon-version">
          {health?.version ?? t("settingsModal.updates.unknown")}
        </span>
      </SettingRow>
      <UpdatePanel />
    </SettingGroup>
  );
}

/** Cards of the "Usage", "Diagnostics" and "Updates" sections. */
export function otherCards(t: TFunction): CardDef[] {
  return [
    {
      id: "logs",
      section: "diagnostics",
      kind: "row",
      title: t("logs.title"),
      description: t("logs.privacy"),
      keywords: "logs support help journal debug gunluk kayit destek yardim hilfe protokoll support",
      node: (
        <SettingRow title={t("logs.title")} description={t("logs.privacy")}>
          <RecentLogs />
        </SettingRow>
      )
    },
    {
      id: "usage",
      section: "usage",
      kind: "block",
      title: t("usage.title"),
      description: t("usage.subtitle"),
      keywords: "usage tokens cost spend limit bill kullanim maliyet limit butce verbrauch kosten token",
      node: (
        <SettingsCard title={t("usage.title")} description={t("usage.subtitle")}>
          <UsageView embedded />
        </SettingsCard>
      )
    },
    {
      id: "diagnostics",
      section: "diagnostics",
      kind: "block",
      title: t("settingsModal.diagnostics.title"),
      description: t("settingsModal.diagnostics.description"),
      keywords: "doctor diagnostics health repair fix support report tanilama saglik onar duzelt destek rapor diagnose gesundheit reparatur bericht",
      node: (
        <SettingsCard title={t("settingsModal.diagnostics.title")} description={t("settingsModal.diagnostics.description")}>
          <DiagnosticsCard />
        </SettingsCard>
      )
    },
    {
      id: "data",
      section: "diagnostics",
      kind: "block",
      title: t("settingsModal.data.title"),
      description: t("settingsModal.data.description"),
      keywords: "data disk space storage cache clear delete backup daten speicher platz cache zwischenspeicher löschen sichern veri depolama alan onbellek temizle sil yedek",
      node: <DataCard />
    },
    {
      id: "safe-mode",
      section: "diagnostics",
      kind: "block",
      title: t("safeMode.settings.title"),
      description: t("safeMode.settings.description"),
      keywords: "safe mode recovery tools schedules channels off reset guvenli mod kurtarma araclar zamanlama kanallar sicherer modus wiederherstellung",
      node: <SafeModeCard />
    },
    {
      id: "version",
      section: "updates",
      kind: "block",
      title: t("settingsModal.updates.title"),
      description: t("settingsModal.updates.description"),
      keywords: "version surum update upgrade rollback guncelleme guncelleme yukselt aktualisierung version",
      node: <VersionBlock />
    }
  ];
}
