import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { BotSpendLimits } from "../api/types";
import {
  draftFromLimits,
  LIMIT_FIELDS,
  LIMIT_PERIODS,
  limitsFromDraft,
  usedFraction,
  type LimitField,
  type LimitPeriod,
  type LimitsDraft
} from "../lib/botLimits";
import { humanLine } from "../lib/humanError";
import { useDaemon } from "../state/DaemonContext";
import formStyles from "./Form.module.css";
import styles from "./BotLimitsField.module.css";

/** Daily / weekly / monthly spending limits of one bot (owner-only to change). */
export function BotLimitsField({ botId }: { botId: string }) {
  const { t, i18n } = useTranslation();
  const { client, activeComputerId } = useDaemon();
  // The owner key only goes to this computer's daemon: a remote host's limits cannot be changed from here.
  const remote = activeComputerId !== "local";
  const [state, setState] = useState<BotSpendLimits | null>(null);
  const [draft, setDraft] = useState<LimitsDraft | null>(null);
  const [bad, setBad] = useState<{ period: LimitPeriod; field: LimitField | "order" } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);
  const usd = new Intl.NumberFormat(i18n.language, { style: "currency", currency: "USD", maximumFractionDigits: 2 });
  const num = new Intl.NumberFormat(i18n.language);
  const when = (ms: number) => new Date(ms).toLocaleString(i18n.language, { dateStyle: "medium", timeStyle: "short" });

  const load = useCallback(async () => {
    if (!client) {
      return;
    }
    try {
      const s = await client.getBotSpendLimits(botId);
      setState(s);
      setDraft(draftFromLimits(s.limits));
    } catch (e) {
      setError(humanLine(e));
    }
  }, [client, botId]);
  useEffect(() => {
    void load();
  }, [load]);

  const save = async (limits: BotSpendLimits["limits"]) => {
    if (!client) {
      return;
    }
    setBusy(true);
    setError(null);
    setSaved(false);
    try {
      const s = await client.setBotSpendLimits(botId, limits);
      setState(s);
      setDraft(draftFromLimits(s.limits));
      setSaved(true);
    } catch (e) {
      setError(humanLine(e));
    } finally {
      setBusy(false);
    }
  };

  const submit = () => {
    if (!draft) {
      return;
    }
    const r = limitsFromDraft(draft);
    if (!r.ok) {
      setBad({ period: r.period, field: r.field });
      return;
    }
    setBad(null);
    void save(r.limits);
  };

  const patch = (period: LimitPeriod, field: LimitField, value: string) => {
    setSaved(false);
    setDraft((d) => (d ? { ...d, [period]: { ...d[period], [field]: value } } : d));
  };

  if (!state || !draft) {
    return error ? (
      <p className={formStyles.error} role="alert">
        {error}
      </p>
    ) : null;
  }

  return (
    <div className={styles.root} data-testid="bot-limits">
      {state.is_default ? (
        <p className={styles.defaultNote} data-testid="bot-limits-default">
          {t("botLimits.defaultNote", {
            usd: usd.format(state.default_daily_hard_usd),
            amount: num.format(state.default_daily_hard_tokens)
          })}
        </p>
      ) : null}
      {LIMIT_PERIODS.map((period) => {
        const u = state.usage[period];
        const l = state.limits[period] ?? {};
        const frac = Math.max(usedFraction(u.usd, l.hard_usd ?? l.soft_usd), usedFraction(u.tokens, l.hard_tokens ?? l.soft_tokens));
        return (
          <section key={period} className={styles.period} aria-labelledby={`bl-${period}`}>
            <h3 id={`bl-${period}`} className={styles.periodTitle}>
              {t(`botLimits.period.${period}`)}
            </h3>
            <p className={formStyles.hint}>
              {t("botLimits.used", { usd: usd.format(u.usd), amount: num.format(u.tokens), when: when(u.resets_ms) })}
            </p>
            <div
              className={styles.bar}
              data-level={frac >= 1 ? "full" : frac >= 0.8 ? "warn" : "ok"}
              role="progressbar"
              aria-label={t(`botLimits.period.${period}`)}
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={Math.round(frac * 100)}
            >
              <div className={styles.fill} style={{ width: `${frac * 100}%` }} />
            </div>
            {u.unreported_calls > 0 ? <p className={formStyles.hint}>{t("botLimits.unreported", { count: u.unreported_calls })}</p> : null}
            <div className={styles.fields}>
              {LIMIT_FIELDS.map((field) => (
                <label key={field} className={formStyles.field}>
                  <span className={formStyles.label}>{t(`botLimits.field.${field}`)}</span>
                  <input
                    className={formStyles.input}
                    inputMode="decimal"
                    disabled={remote}
                    value={draft[period][field]}
                    placeholder={t("botLimits.unlimited")}
                    aria-invalid={bad?.period === period && bad.field === field}
                    onChange={(e) => patch(period, field, e.target.value)}
                  />
                </label>
              ))}
            </div>
            {bad?.period === period ? (
              <p className={formStyles.error} role="alert">
                {t(bad.field === "order" ? "botLimits.order" : "botLimits.invalid")}
              </p>
            ) : null}
          </section>
        );
      })}
      <p className={formStyles.hint}>{t("botLimits.hint")}</p>
      {remote ? (
        <p className={formStyles.hint} data-testid="bot-limits-remote">
          {t("botLimits.remote")}
        </p>
      ) : (
        <div className={styles.actions}>
          <Button disabled={busy} onClick={submit} data-testid="bot-limits-save">
            {busy ? t("botLimits.saving") : t("botLimits.save")}
          </Button>
          {saved ? <span className={formStyles.success}>{t("botLimits.saved")}</span> : null}
        </div>
      )}
      {error ? (
        <p className={formStyles.error} role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}
