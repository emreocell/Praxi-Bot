import { useState } from "react";
import { useTranslation } from "react-i18next";
import form from "../../components/Form.module.css";
import { MoreInfo } from "../../components/ChoiceList";
import { SwitchField } from "../../components/SettingsUi";
import { humanLine } from "../../lib/humanError";
import { historySummaryOn, historySummaryPatch } from "../../lib/historySummary";
import { useDaemon } from "../../state/DaemonContext";

/**
 * Settings > AI: whether a bot condenses the older part of a long chat into a short note that
 * stands in for it (the messages themselves stay stored and searchable). On by default; the
 * switch is the bot's `history.summary` profile field, changed with a field-level patch.
 */
export function HistorySummaryCard() {
  const { t } = useTranslation();
  const { client, bots, refreshBots } = useDaemon();
  const [picked, setPicked] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const bot = bots.find((b) => b.id === picked) ?? bots[0];

  if (!bot) {
    return <p className={form.hint}>{t("historySummary.noBot")}</p>;
  }
  const choose = async (on: boolean) => {
    if (!client) return;
    setBusy(true);
    setError(null);
    try {
      await client.patchBot(bot.id, historySummaryPatch(bot, on));
      await refreshBots();
    } catch (e) {
      setError(humanLine(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div data-testid="history-summary">
      {bots.length > 1 ? (
        <label className={form.field}>
          {t("historySummary.bot")}
          <select className={form.select} value={bot.id} disabled={busy} onChange={(e) => setPicked(e.target.value)}>
            {bots.map((item) => (
              <option key={item.id} value={item.id}>
                {item.name}
              </option>
            ))}
          </select>
        </label>
      ) : null}
      <SwitchField
        label={t("historySummary.label")}
        hint={t("historySummary.hint")}
        checked={historySummaryOn(bot.config)}
        disabled={busy || !client}
        testId="history-summary-switch"
        onChange={(on) => void choose(on)}
      />
      <MoreInfo testId="history-summary-more">
        <p>{t("historySummary.note")}</p>
      </MoreInfo>
      {error ? (
        <p role="alert" className={form.error}>
          {error}
        </p>
      ) : null}
    </div>
  );
}
