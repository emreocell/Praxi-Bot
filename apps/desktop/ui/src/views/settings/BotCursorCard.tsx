import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import formStyles from "../../components/Form.module.css";
import { SettingRow, SettingGroup, Switch } from "../../components/SettingsUi";
import { humanLine } from "../../lib/humanError";
import { useDaemon } from "../../state/DaemonContext";
import type { AppSettings } from "../../store/settingsStore";

/**
 * Settings > Computer: whether the bot draws a pointer of its own (a coloured arrow with a click
 * ripple) on the desktop, in its browser page and in the live view, so you can see where it is
 * about to click. The daemon holds the switch (owner only); the app mirrors it so the desktop
 * overlay windows follow at once.
 */
export function BotCursorCard({
  settings,
  update
}: {
  settings: AppSettings;
  update: (patch: Partial<AppSettings>) => void;
}) {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const [enabled, setEnabled] = useState(settings.botCursor);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // The daemon is the truth: line the app's mirror up with it when they differ.
  useEffect(() => {
    if (!client) return undefined;
    let cancelled = false;
    client
      .getBotCursor()
      .then((s) => {
        if (cancelled) return;
        setEnabled(s.enabled);
        if (s.enabled !== settings.botCursor) update({ botCursor: s.enabled });
      })
      .catch(() => {
        // An older daemon has no such switch: keep the app's own value.
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- read once per connection
  }, [client]);

  const choose = useCallback(
    async (next: boolean) => {
      setBusy(true);
      setError(null);
      setEnabled(next);
      try {
        if (client) await client.putBotCursor({ enabled: next });
        update({ botCursor: next });
      } catch (e) {
        setEnabled(!next);
        setError(humanLine(e));
      } finally {
        setBusy(false);
      }
    },
    [client, update]
  );

  return (
    <SettingGroup>
      <SettingRow title={t("botCursor.title")} description={t("botCursor.description")}>
        <Switch
          label={t("botCursor.title")}
          checked={enabled}
          disabled={busy}
          onChange={() => void choose(!enabled)}
        />
      </SettingRow>
      {error ? (
        <p role="alert" className={formStyles.error}>
          {error}
        </p>
      ) : null}
    </SettingGroup>
  );
}
