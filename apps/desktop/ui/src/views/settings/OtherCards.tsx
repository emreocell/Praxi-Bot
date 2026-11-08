import { getVersion } from "@tauri-apps/api/app";
import type { TFunction } from "i18next";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { SettingGroup, SettingRow } from "../../components/SettingsUi";
import { PRODUCT_CONFIG } from "../../config/product";
import { isTauriRuntime } from "../../overlay/events";
import { useDaemon } from "../../state/DaemonContext";
import { UsageView } from "../UsageView";
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
      <SettingRow
        title={t("settingsModal.updates.how.title")}
        description={t("settingsModal.updates.how.description")}
      />
    </SettingGroup>
  );
}

/** Cards of the "Usage" and "Updates" sections. */
export function otherCards(t: TFunction): CardDef[] {
  return [
    {
      id: "usage",
      section: "usage",
      kind: "block",
      title: t("usage.title"),
      description: t("usage.subtitle"),
      keywords: "tokens cost maliyet kullanim",
      node: <UsageView embedded />
    },
    {
      id: "version",
      section: "updates",
      kind: "block",
      title: t("settingsModal.updates.title"),
      description: t("settingsModal.updates.description"),
      keywords: "version surum update guncelleme",
      node: <VersionBlock />
    }
  ];
}
