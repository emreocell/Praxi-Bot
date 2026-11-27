import type { TFunction } from "i18next";
import { SettingRow, SettingsCard } from "../../components/SettingsUi";
import { UsageView, UsageLimitsCard } from "../UsageView";
import { DataCard } from "./DataCard";
import { DiagnosticsCard } from "./DiagnosticsCard";
import { RecentLogs } from "../../components/RecentLogs";
import { SafeModeCard } from "./SafeModeCard";
import { UpdatesPage } from "./UpdatesPage";
import type { CardDef } from "./types";

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
      keywords: "usage tokens cost spend limit bill quota kullanim maliyet limit butce kota verbrauch kosten token kontingent",
      node: (
        <SettingsCard title={t("usage.title")} description={t("usage.subtitle")}>
          <UsageView embedded />
        </SettingsCard>
      )
    },
    {
      id: "spend-limits",
      section: "usage",
      kind: "block",
      title: t("settingsUi.limits.title"),
      description: t("settingsUi.limits.description"),
      keywords: "spending limit budget monthly forecast quota remaining today daily digest harcama limit butce aylik tahmin kota kalan bugun ausgabenlimit budget monatlich prognose kontingent heute",
      node: (
        <SettingsCard title={t("settingsUi.limits.title")} description={t("settingsUi.limits.description")}>
          <UsageLimitsCard />
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
      title: t("settingsUi.updates.title"),
      description: t("settingsUi.updates.description"),
      keywords: "about version build stable preview surum hakkinda kararlı önizleme update upgrade rollback guncelleme yukselt aktualisierung über stabil vorschau",
      node: <UpdatesPage />
    }
  ];
}
