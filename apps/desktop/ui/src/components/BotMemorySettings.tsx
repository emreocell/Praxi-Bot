import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { BotMemorySettings as Settings } from "../api/types";
import { humanLine } from "../lib/humanError";
import { useDaemon } from "../state/DaemonContext";
import form from "./Form.module.css";

const MODES = ["on", "ask", "off"] as const;
type Mode = (typeof MODES)[number];

/**
 * "Memory: On / Ask before saving / Off" for one bot, plus (under Advanced) how much
 * memory may be read into one answer and whether what this bot learns is shared with the
 * owner's other bots. Changing it is owner-only on the daemon; a refusal shows as an error.
 */
export function BotMemorySettings({ botId }: { botId: string }) {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const [settings, setSettings] = useState<Settings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!client || !botId) return undefined;
    let cancelled = false;
    setSettings(null);
    client
      .getBotMemorySettings(botId)
      .then((s) => {
        if (!cancelled) setSettings(s);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(humanLine(e));
      });
    return () => {
      cancelled = true;
    };
  }, [client, botId]);

  const save = useCallback(
    async (next: Settings) => {
      if (!client) return;
      setBusy(true);
      setError(null);
      try {
        setSettings(
          await client.putBotMemorySettings(botId, {
            mode: next.mode,
            budget_tokens: next.budget_tokens,
            share_with_other_bots: next.share_with_other_bots
          })
        );
      } catch (e) {
        setError(humanLine(e));
      } finally {
        setBusy(false);
      }
    },
    [client, botId]
  );

  if (!settings) {
    return error ? (
      <p role="alert" className={form.error}>
        {error}
      </p>
    ) : null;
  }

  return (
    <div data-testid="bot-memory-settings">
      <fieldset className={form.field} disabled={busy}>
        <legend>{t("memorySettings.title")}</legend>
        {MODES.map((mode: Mode) => (
          <label key={mode} className={form.hint}>
            <input
              type="radio"
              name={`memory-mode-${botId}`}
              checked={settings.mode === mode}
              onChange={() => void save({ ...settings, mode })}
            />{" "}
            <strong>{t(`memorySettings.mode.${mode}`)}</strong> — {t(`memorySettings.modeHint.${mode}`)}
          </label>
        ))}
      </fieldset>
      <details>
        <summary>{t("memorySettings.advanced")}</summary>
        <label className={form.field}>
          {t("memorySettings.budget", { words: Math.round((settings.budget_tokens * 0.75) / 50) * 50 })}
          <input
            type="range"
            min={settings.budget_min}
            max={settings.budget_max}
            step={100}
            value={settings.budget_tokens}
            disabled={busy}
            aria-label={t("memorySettings.budgetLabel")}
            onChange={(e) => setSettings({ ...settings, budget_tokens: Number(e.target.value) })}
            onPointerUp={() => void save(settings)}
            onKeyUp={() => void save(settings)}
          />
        </label>
        <p className={form.hint}>{t("memorySettings.budgetHint")}</p>
        <label className={form.hint}>
          <input
            type="checkbox"
            checked={settings.share_with_other_bots}
            disabled={busy}
            onChange={(e) => void save({ ...settings, share_with_other_bots: e.target.checked })}
          />{" "}
          {t("memorySettings.share")}
        </label>
        <p className={form.hint}>{t("memorySettings.shareHint")}</p>
      </details>
      {error ? (
        <p role="alert" className={form.error}>
          {error}
        </p>
      ) : null}
    </div>
  );
}
