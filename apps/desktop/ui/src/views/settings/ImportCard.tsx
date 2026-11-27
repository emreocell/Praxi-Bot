import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { SettingRow } from "../../components/SettingsUi";
import { ImportAssistantsModal } from "../ImportAssistantsModal";

/**
 * Settings > General > Import from other assistants: opens the import screen. Nothing is read
 * from the other apps until the owner presses "Look for setups" inside it.
 */
export function ImportCard() {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  return (
    <SettingRow title={t("importAssistants.card.title")} description={t("importAssistants.card.description")}>
      <Button size="sm" variant="secondary" data-testid="settings-open-import" onClick={() => setOpen(true)}>
        {t("importAssistants.card.button")}
      </Button>
      <ImportAssistantsModal open={open} onClose={() => setOpen(false)} />
    </SettingRow>
  );
}
