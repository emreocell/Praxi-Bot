import { useEffect, useState, useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import formStyles from "../../components/Form.module.css";
import { SettingRow, Switch } from "../../components/SettingsUi";
import {
  getQuickAskProblem,
  hotkeyProblemKey,
  QUICK_ASK_PRESETS,
  subscribeQuickAskProblem
} from "../../lib/quickAsk";
import type { AppSettings } from "../../store/settingsStore";
import styles from "../SettingsModal.module.css";

/** Settings card for the global Quick ask hotkey: off by default, explained in plain words. */
export function QuickAskCard({
  settings,
  update
}: {
  settings: AppSettings;
  update: (patch: Partial<AppSettings>) => void;
}) {
  const { t } = useTranslation();
  const problem = useSyncExternalStore(subscribeQuickAskProblem, getQuickAskProblem);
  const [draft, setDraft] = useState(settings.quickAskHotkey);
  useEffect(() => setDraft(settings.quickAskHotkey), [settings.quickAskHotkey]);

  const commit = () => {
    const next = draft.trim();
    if (next && next !== settings.quickAskHotkey) update({ quickAskHotkey: next });
  };

  return (
    <SettingRow
      stacked
      title={t("settings.quickAsk.title")}
      description={t("settings.quickAsk.description")}
    >
      <Switch
        label={t("settings.quickAsk.title")}
        checked={settings.quickAskEnabled}
        onChange={() => update({ quickAskEnabled: !settings.quickAskEnabled })}
      />
      {settings.quickAskEnabled ? (
        <>
          <input
            className={`${formStyles.input} ${styles.shortInput}`}
            value={draft}
            maxLength={40}
            aria-label={t("settings.quickAsk.hotkey")}
            onChange={(e) => setDraft(e.target.value)}
            onBlur={commit}
            onKeyDown={(e) => {
              if (e.key === "Enter") commit();
            }}
          />
          {QUICK_ASK_PRESETS.map((preset) => (
            <Button
              key={preset}
              size="sm"
              variant="secondary"
              active={settings.quickAskHotkey.toLowerCase() === preset.toLowerCase()}
              onClick={() => update({ quickAskHotkey: preset })}
            >
              {preset}
            </Button>
          ))}
          <span className={formStyles.hint}>{t("settings.quickAsk.privacy")}</span>
          {problem ? (
            <span className={formStyles.error} role="alert">
              {t(hotkeyProblemKey(problem))}
            </span>
          ) : null}
        </>
      ) : null}
    </SettingRow>
  );
}
