import { AlertTriangle, CircleCheck, Gauge, TrendingUp } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Card } from "ui";
import type { SpendLimit } from "../api/types";
import { humanLine } from "../lib/humanError";
import { paceOf, parseLimitInput, usedFraction, type Pace } from "../lib/spendLimit";
import { useDaemon } from "../state/DaemonContext";
import { MoreInfo } from "./ChoiceList";
import formStyles from "./Form.module.css";
import styles from "./SpendLimitCard.module.css";

const PACE_ICON: Record<Pace, typeof Gauge> = {
  off: Gauge,
  noForecast: TrendingUp,
  under: CircleCheck,
  over: AlertTriangle,
  warn: AlertTriangle,
  reached: AlertTriangle
};

/** Monthly spending limit: month-to-date, remaining, linear pace forecast, and the owner's set/clear field. */
export function SpendLimitCard({ refreshKey }: { refreshKey: number }) {
  const { t, i18n } = useTranslation();
  const { client, activeComputerId } = useDaemon();
  // The owner key only goes to this computer's daemon: a remote host's limit cannot be changed from here.
  const remote = activeComputerId !== "local";
  const [state, setState] = useState<SpendLimit | null>(null);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const usd = new Intl.NumberFormat(i18n.language, { style: "currency", currency: "USD", maximumFractionDigits: 2 });
  const money = (v?: number | null) => (v === null || v === undefined ? "—" : usd.format(v));

  const load = useCallback(async () => {
    if (!client) {
      return;
    }
    try {
      setState(await client.getSpendLimit());
    } catch (e) {
      setError(humanLine(e));
    }
  }, [client]);
  useEffect(() => {
    void load();
  }, [load, refreshKey]);

  const save = async (value: number | null) => {
    if (!client) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      setState(await client.setSpendLimit(value));
      setDraft("");
    } catch (e) {
      setError(humanLine(e));
    } finally {
      setBusy(false);
    }
  };

  const parsed = parseLimitInput(draft);
  const pace = state ? paceOf(state) : "off";
  const PaceIcon = PACE_ICON[pace];
  const hasLimit = state?.limit_usd !== null && state?.limit_usd !== undefined;

  return (
    <Card padding="md" data-testid="spend-limit">
      <div className={styles.head}>
        <Gauge size={16} className={styles.headIcon} aria-hidden />
        <h2 className={styles.title}>{t("usage.limit.title")}</h2>
      </div>
      {state ? (
        <>
          <div className={styles.stats}>
            <div className={styles.stat}>
              <span className={styles.statLabel}>{t("usage.limit.limitLabel")}</span>
              <span className={`${styles.statValue} ${hasLimit ? "" : styles.statOff}`}>{hasLimit ? money(state.limit_usd) : t("usage.limit.off")}</span>
            </div>
            <div className={styles.stat}>
              <span className={styles.statLabel}>{t("usage.limit.spent")}</span>
              <span className={styles.statValue}>{money(state.spent_usd)}</span>
            </div>
            {hasLimit ? (
              <div className={styles.stat}>
                <span className={styles.statLabel}>{t("usage.limit.remaining")}</span>
                <span className={styles.statValue}>{money(state.remaining_usd)}</span>
              </div>
            ) : null}
            <div className={styles.stat}>
              <span className={styles.statLabel}>{t("usage.limit.forecast")}</span>
              <span className={styles.statValue}>{money(state.forecast_usd)}</span>
            </div>
          </div>
          {hasLimit ? (
            <div
              className={styles.bar}
              data-pace={pace}
              role="progressbar"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={Math.round(usedFraction(state) * 100)}
            >
              <div className={styles.fill} style={{ width: `${usedFraction(state) * 100}%` }} />
            </div>
          ) : null}
          <p className={styles.pace} data-pace={pace}>
            <PaceIcon size={14} aria-hidden />
            {t(`usage.limit.pace.${pace}`)}
          </p>
        </>
      ) : null}
      {remote ? (
        <p className={formStyles.hint} data-testid="spend-limit-remote">
          {t("usage.limit.remote")}
        </p>
      ) : (
        <>
          <form
            className={styles.form}
            onSubmit={(e) => {
              e.preventDefault();
              if (parsed !== null) {
                void save(parsed);
              }
            }}
          >
            <label className={`${formStyles.field} ${styles.amount}`}>
              <span className={formStyles.label}>{t("usage.limit.label")}</span>
              <input
                className={formStyles.input}
                inputMode="decimal"
                value={draft}
                placeholder={t("usage.limit.placeholder")}
                aria-invalid={draft !== "" && parsed === null}
                onChange={(e) => setDraft(e.target.value)}
              />
            </label>
            <Button type="submit" disabled={busy || parsed === null}>
              {t("usage.limit.save")}
            </Button>
            {hasLimit ? (
              <Button variant="ghost" disabled={busy} onClick={() => void save(null)}>
                {t("usage.limit.clear")}
              </Button>
            ) : null}
          </form>
          {draft !== "" && parsed === null ? <p className={formStyles.error} role="alert">{t("usage.limit.invalid")}</p> : null}
        </>
      )}
      {error ? (
        <p className={formStyles.error} role="alert">
          {error}
        </p>
      ) : null}
      <MoreInfo testId="spend-limit-more">
        <p>{t("usage.limit.hint")}</p>
        <p>{t("usage.limit.notCounted")}</p>
        {state && state.unpriced_calls > 0 ? (
          <p>{t("usage.limit.unpriced", { count: state.unpriced_calls })}</p>
        ) : null}
      </MoreInfo>
    </Card>
  );
}
