import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { BotMemorySettings as Settings } from "../api/types";
import { humanLine } from "../lib/humanError";
import { useDaemon } from "../state/DaemonContext";
import { ChoiceList, MoreInfo } from "./ChoiceList";
import choice from "./ChoiceList.module.css";
import form from "./Form.module.css";

export const MEMORY_MODES = ["on", "ask", "off"] as const;
type Mode = (typeof MEMORY_MODES)[number];

/** One bot's memory settings: load, save (owner-only on the daemon; a refusal shows as an error). */
export function useBotMemorySettings(botId: string) {
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

  return { settings, setSettings, save, error, busy };
}

type MemoryState = ReturnType<typeof useBotMemorySettings>;

/** "On (recommended) / Ask before saving / Off": one line each. */
export function BotMemoryModeChoice({ botId, state }: { botId: string; state: MemoryState }) {
  const { t } = useTranslation();
  const { settings, save, busy } = state;
  if (!settings) return null;
  return (
    <ChoiceList<Mode>
      name={`memory-mode-${botId}`}
      label={t("memorySettings.title")}
      value={settings.mode as Mode}
      disabled={busy}
      testIdPrefix="memory-mode"
      options={MEMORY_MODES.map((mode) => ({
        value: mode,
        label: t(`memorySettings.mode.${mode}`),
        hint: t(`memorySettings.modeHint.${mode}`),
        recommended: mode === "on"
      }))}
      onChange={(mode) => void save({ ...settings, mode })}
    />
  );
}

/** How much memory goes into one answer, and whether other bots may use what this one learns. */
export function BotMemoryAdvancedFields({ state }: { state: MemoryState }) {
  const { t } = useTranslation();
  const { settings, setSettings, save, busy } = state;
  if (!settings) return null;
  return (
    <>
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
    </>
  );
}

function MemoryError({ state }: { state: MemoryState }) {
  return state.error ? (
    <p role="alert" className={form.error}>
      {state.error}
    </p>
  ) : null;
}

/**
 * The bot editor's memory block: the three modes, and (folded) the answer size and sharing.
 * Settings > AI and memory uses the same parts through `useBotMemorySettings`.
 */
export function BotMemorySettings({ botId }: { botId: string }) {
  const { t } = useTranslation();
  const state = useBotMemorySettings(botId);
  if (!state.settings) return <MemoryError state={state} />;
  return (
    <div data-testid="bot-memory-settings" className={choice.stack}>
      <BotMemoryModeChoice botId={botId} state={state} />
      <MoreInfo label={t("memorySettings.advanced")}>
        <BotMemoryAdvancedFields state={state} />
      </MoreInfo>
      <MemoryError state={state} />
    </div>
  );
}

export { MemoryError as BotMemoryError };
