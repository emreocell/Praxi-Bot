import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { PolicyDto } from "../../api/types";
import { AccessLevelPicker } from "../../components/AccessLevelPicker";
import formStyles from "../../components/Form.module.css";
import { accessLevelOf } from "../../lib/accessLevels";
import { humanLine } from "../../lib/humanError";
import { RECOMMENDED_SAFETY_PRESET } from "../../lib/settingsPages";
import { useDaemon } from "../../state/DaemonContext";
import styles from "./SafetyLevelsCard.module.css";

/** Fired after the settings page puts the safety level back to the recommended one. */
export const POLICY_CHANGED_EVENT = "praxi:policy-changed";

/** The level every bot follows unless it has its own: the same list the bot editor and the new-bot dialog use. */
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

  const current = accessLevelOf(policy?.preset);
  return (
    <div className={styles.root} data-testid="safety-levels">
      <AccessLevelPicker
        value={current}
        label={t("settingsModal.safety.title")}
        recommended={RECOMMENDED_SAFETY_PRESET}
        disabled={busy || !policy}
        testIdPrefix="safety-level"
        onChange={(preset) => void choose(preset)}
      />
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
