import { Check, ShieldCheck } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { PolicyDto } from "../../api/types";
import formStyles from "../../components/Form.module.css";
import { humanLine } from "../../lib/humanError";
import { levelOfPreset, SAFETY_LEVELS } from "../../lib/settingsPages";
import { useDaemon } from "../../state/DaemonContext";
import styles from "./SafetyLevelsCard.module.css";

/** Fired after the settings page puts the safety level back to the recommended one. */
export const POLICY_CHANGED_EVENT = "praxi:policy-changed";

/** Three understandable levels for what bots may do on their own; the daemon presets behind them stay. */
export function SafetyLevelsCard() {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const [policy, setPolicy] = useState<PolicyDto | null>(null);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);

  useEffect(() => {
    if (!client) return undefined;
    const load = () =>
      client
        .getPolicy()
        .then(setPolicy)
        .catch((e: unknown) => setMsg({ ok: false, text: humanLine(e) }));
    void load();
    window.addEventListener(POLICY_CHANGED_EVENT, load);
    return () => window.removeEventListener(POLICY_CHANGED_EVENT, load);
  }, [client]);

  const choose = async (preset: string) => {
    if (!client || busy) return;
    setBusy(true);
    setMsg(null);
    try {
      setPolicy(await client.putPolicy({ preset }));
      setMsg({ ok: true, text: t("settingsModal.safety.saved") });
    } catch (e) {
      setMsg({ ok: false, text: humanLine(e) });
    } finally {
      setBusy(false);
    }
  };

  const current = levelOfPreset(policy?.preset);
  return (
    <div className={styles.root} data-testid="safety-levels">
      <p className={styles.lead}>{t("settingsModal.safety.lead")}</p>
      <div className={styles.grid} role="radiogroup" aria-label={t("settingsModal.safety.title")}>
        {SAFETY_LEVELS.map((l) => {
          const active = current === l.id;
          return (
            <button
              key={l.id}
              type="button"
              role="radio"
              aria-checked={active}
              disabled={busy || !policy}
              data-testid={`safety-level-${l.id}`}
              className={[styles.level, active ? styles.active : ""].filter(Boolean).join(" ")}
              onClick={() => void choose(l.preset)}
            >
              <span className={styles.head}>
                <span className={styles.name}>{t(`settingsModal.safety.levels.${l.id}.name`)}</span>
                {l.recommended ? (
                  <span className={styles.badge}>
                    <ShieldCheck aria-hidden="true" />
                    {t("settingsModal.safety.recommended")}
                  </span>
                ) : null}
                {active ? <Check className={styles.check} aria-hidden="true" /> : null}
              </span>
              <span className={styles.summary}>{t(`settingsModal.safety.levels.${l.id}.summary`)}</span>
              <span className={styles.group}>{t("settingsModal.safety.onItsOwn")}</span>
              <span className={styles.example}>{t(`settingsModal.safety.levels.${l.id}.own`)}</span>
              <span className={styles.group}>{t("settingsModal.safety.willAsk")}</span>
              <span className={styles.example}>{t(`settingsModal.safety.levels.${l.id}.ask`)}</span>
            </button>
          );
        })}
      </div>
      {policy && current === null ? (
        <p className={formStyles.hint}>{t("settingsModal.safety.custom")}</p>
      ) : null}
      {!client ? <p className={formStyles.hint}>{t("settingsModal.safety.offline")}</p> : null}
      {msg ? (
        <p className={msg.ok ? styles.ok : formStyles.error} role={msg.ok ? "status" : "alert"}>
          {msg.text}
        </p>
      ) : null}
    </div>
  );
}
