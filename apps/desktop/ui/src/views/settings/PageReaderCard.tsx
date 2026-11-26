import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { PageReader } from "../../api/types";
import { ChoiceList, MoreInfo } from "../../components/ChoiceList";
import choice from "../../components/ChoiceList.module.css";
import form from "../../components/Form.module.css";
import { humanLine } from "../../lib/humanError";
import { useDaemon } from "../../state/DaemonContext";

const MODES = ["off", "auto", "always"] as const;
type Mode = (typeof MODES)[number];

/**
 * Settings > Safety: how outside text (web pages, mail) reaches one bot. "Automatic" sends
 * pages through a separate helper model while the bot holds private data; "Always" does it
 * for every page; "Off" lets the bot read pages itself. Owner-only on the daemon.
 */
export function PageReaderCard() {
  const { t } = useTranslation();
  const { client, bots } = useDaemon();
  const [bot, setBot] = useState("");
  const activeBot = bot || bots[0]?.id || "";
  const [setting, setSetting] = useState<PageReader | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!client || !activeBot) return undefined;
    let cancelled = false;
    setSetting(null);
    setError(null);
    client
      .getPageReader(activeBot)
      .then((s) => {
        if (!cancelled) setSetting(s);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(humanLine(e));
      });
    return () => {
      cancelled = true;
    };
  }, [client, activeBot]);

  const choose = useCallback(
    async (mode: Mode) => {
      if (!client || !activeBot) return;
      setBusy(true);
      setError(null);
      try {
        setSetting(await client.putPageReader(activeBot, mode));
      } catch (e) {
        setError(humanLine(e));
      } finally {
        setBusy(false);
      }
    },
    [client, activeBot]
  );

  if (!activeBot) {
    return <p className={form.hint}>{t("pageReader.noBot")}</p>;
  }
  return (
    <div data-testid="page-reader" className={choice.stack}>
      {bots.length > 1 ? (
        <label className={form.field}>
          {t("pageReader.bot")}
          <select
            className={form.select}
            value={activeBot}
            disabled={busy}
            onChange={(e) => setBot(e.target.value)}
          >
            {bots.map((item) => (
              <option key={item.id} value={item.id}>
                {item.name}
              </option>
            ))}
          </select>
        </label>
      ) : null}
      {setting ? (
        <ChoiceList<Mode>
          name={`page-reader-${activeBot}`}
          label={t("pageReader.legend")}
          value={setting.mode as Mode}
          disabled={busy}
          testIdPrefix="page-reader"
          options={MODES.map((mode) => ({
            value: mode,
            label: t(`pageReader.mode.${mode}`),
            hint: t(`pageReader.modeHint.${mode}`),
            recommended: mode === "auto"
          }))}
          onChange={(mode) => void choose(mode)}
        />
      ) : null}
      <MoreInfo testId="page-reader-more">
        <p>{t("pageReader.note")}</p>
      </MoreInfo>
      {error ? (
        <p role="alert" className={form.error}>
          {error}
        </p>
      ) : null}
    </div>
  );
}
