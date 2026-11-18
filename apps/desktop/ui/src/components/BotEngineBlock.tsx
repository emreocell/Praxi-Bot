import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { Bot, CliEngineDto } from "../api/types";
import {
  type ApprovalLevel,
  type EngineMode,
  type EngineState,
  consentMissing,
  engineOf,
  engineRequest,
  executorCli
} from "../lib/botEngine";
import { useDaemon } from "../state/DaemonContext";
import formStyles from "./Form.module.css";
import styles from "./BotEngineBlock.module.css";
import { humanLine } from "../lib/humanError";

/** Static keys keep `check:i18n` honest. */
function levelText(t: (key: string) => string, level: ApprovalLevel): { name: string; text: string } {
  switch (level) {
    case "accept_edits": return { name: t("botEngine.level.accept_edits.name"), text: t("botEngine.level.accept_edits.text") };
    case "auto": return { name: t("botEngine.level.auto.name"), text: t("botEngine.level.auto.text") };
    case "full": return { name: t("botEngine.level.full.name"), text: t("botEngine.level.full.text") };
    default: return { name: t("botEngine.level.ask.name"), text: t("botEngine.level.ask.text") };
  }
}

/**
 * "Engine" block of a bot: who runs its files and shell. Applies on its own
 * (`PUT /v1/bots/{id}/engine`, local owner only), independent of the profile Save.
 */
export function BotEngineBlock({ bot }: { bot: Bot }) {
  const { t } = useTranslation();
  const { client, refreshBots } = useDaemon();
  const current = engineOf(bot.config);
  const cliId = executorCli(bot.config);
  const [engines, setEngines] = useState<CliEngineDto[]>([]);
  const [draft, setDraft] = useState<EngineState>(current);
  const [consent, setConsent] = useState({ native: false, full: false });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!client) { return; }
    client.cliEngines().then(setEngines).catch(() => setEngines([]));
  }, [client]);

  const engine = engines.find((e) => e.detected.id === cliId);
  const native = engine?.native;
  const name = engine?.detected.display_name ?? cliId ?? "";
  const patch = (p: Partial<EngineState>) => { setDraft((d) => ({ ...d, ...p })); setError(null); };
  const missing = consentMissing(draft, current, consent);
  const changed = draft.mode !== current.mode || draft.approvalLevel !== current.approvalLevel || draft.workspace !== current.workspace;

  const run = async (action: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await action();
      setConsent({ native: false, full: false });
      await refreshBots();
    } catch (e) {
      setError(humanLine(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className={styles.block} data-testid="bot-engine-block">
      <p className={formStyles.hint}>{t("botEngine.hint")}</p>
      {current.mode === "native_tools" ? (
        <p className={styles.label} role="status">
          {t("botEngine.label", { cli: name })}
        </p>
      ) : null}
      {!cliId || !native ? (
        <p className={formStyles.hint}>{t("botEngine.needCli")}</p>
      ) : (
        <>
          <div role="radiogroup" aria-label={t("botEngine.mode")} className={styles.choices}>
            {(["praxi_governed", "native_tools"] as EngineMode[]).map((mode) => (
              <label key={mode} className={styles.choice}>
                <input
                  type="radio"
                  name="engine-mode"
                  checked={draft.mode === mode}
                  onChange={() => patch({ mode, approvalLevel: mode === "native_tools" ? draft.approvalLevel : "ask" })}
                />
                <span>
                  <strong>{mode === "native_tools" ? t("botEngine.native.name", { cli: name }) : t("botEngine.governed.name")}</strong>
                  <span className={formStyles.hint}>{mode === "native_tools" ? t("botEngine.native.text", { cli: name }) : t("botEngine.governed.text")}</span>
                </span>
              </label>
            ))}
          </div>

          {draft.mode === "native_tools" ? (
            <>
              {native.support === "experimental" ? (
                <p className={styles.experimental} role="note">{t("botEngine.experimental", { cli: name })}</p>
              ) : null}
              <div role="radiogroup" aria-label={t("botEngine.level.title")} className={styles.choices}>
                {native.levels.map((row) => {
                  const level = row.level as ApprovalLevel;
                  const text = levelText(t, level);
                  return (
                    <label key={level} className={styles.choice} data-offered={row.offered}>
                      <input
                        type="radio"
                        name="engine-level"
                        disabled={!row.offered}
                        checked={draft.approvalLevel === level}
                        onChange={() => patch({ approvalLevel: level })}
                      />
                      <span>
                        <strong>{text.name}</strong>
                        <span className={formStyles.hint}>{text.text}</span>
                        <code className={styles.setting}>
                          {row.offered
                            ? t("botEngine.level.maps", { cli: name, setting: row.native_setting })
                            : t("botEngine.level.notOffered", { cli: name, applied: row.applied, setting: row.native_setting })}
                        </code>
                      </span>
                    </label>
                  );
                })}
              </div>
              <div className={formStyles.field}>
                <label className={formStyles.label} htmlFor="engine-workspace">{t("botEngine.workspace")}</label>
                <input
                  id="engine-workspace"
                  className={formStyles.input}
                  value={draft.workspace}
                  placeholder={t("botEngine.workspacePlaceholder")}
                  onChange={(e) => patch({ workspace: e.target.value })}
                />
                <p className={formStyles.hint}>{t("botEngine.workspaceHint")}</p>
              </div>
              {current.mode !== "native_tools" ? (
                <label className={styles.consent}>
                  <input type="checkbox" checked={consent.native} onChange={(e) => setConsent((c) => ({ ...c, native: e.target.checked }))} />
                  <span>{t("botEngine.consentNative", { cli: name })}</span>
                </label>
              ) : null}
              {draft.approvalLevel === "full" && !(current.mode === "native_tools" && current.approvalLevel === "full") ? (
                <label className={styles.consent}>
                  <input type="checkbox" checked={consent.full} onChange={(e) => setConsent((c) => ({ ...c, full: e.target.checked }))} />
                  <span>{t("botEngine.consentFull", { cli: name })}</span>
                </label>
              ) : null}
            </>
          ) : null}

          {error ? <p className={formStyles.error} role="alert">{error}</p> : null}
          <div className={styles.actions}>
            <Button
              variant="primary"
              disabled={!client || busy || !changed || missing}
              onClick={() => client && void run(() => client.putBotEngine(bot.id, engineRequest(draft, current, consent)))}
            >
              {t("botEngine.apply")}
            </Button>
            {current.mode === "native_tools" ? (
              <Button
                variant="secondary"
                disabled={!client || busy}
                onClick={() => client && void run(() => client.killBotEngine(bot.id).then(() => setDraft(engineOf(undefined))))}
              >
                {t("botEngine.kill")}
              </Button>
            ) : null}
          </div>
        </>
      )}
    </section>
  );
}
