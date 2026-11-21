import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { SettingGroup, SettingRow } from "../../components/SettingsUi";
import { safeModeView } from "../../lib/safeMode";
import { useDaemon } from "../../state/DaemonContext";

/**
 * Settings > Diagnostics: restart the local service in safe mode (tools, schedules, channels,
 * MCP servers and apps off) to fix a bad configuration, or restart it normally to leave it.
 * Only the owner at this app can do it; no tool, bot or paired device can.
 */
export function SafeModeCard() {
  const { t } = useTranslation();
  const { health, conn, restartDaemon, phase } = useDaemon();
  const view = safeModeView(health, conn);
  const busy = phase === "connecting";
  return (
    <SettingGroup>
      <SettingRow
        title={view.active ? t("safeMode.settings.activeTitle") : t("safeMode.settings.inactiveTitle")}
        description={
          view.canEnter ? t("safeMode.settings.explain") : t("safeMode.settings.manual")
        }
      >
        {view.canEnter ? (
          <Button
            size="sm"
            variant="secondary"
            disabled={busy}
            data-testid="safe-mode-toggle"
            onClick={() => void restartDaemon(!view.active)}
          >
            {view.active ? t("safeMode.banner.leave") : t("safeMode.settings.enter")}
          </Button>
        ) : null}
      </SettingRow>
    </SettingGroup>
  );
}
