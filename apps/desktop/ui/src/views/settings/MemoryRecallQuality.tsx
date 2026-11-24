import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { MemoryEmbedding } from "../../api/types";
import form from "../../components/Form.module.css";
import { humanLine } from "../../lib/humanError";
import { useDaemon } from "../../state/DaemonContext";

/**
 * "Basic (word match)" or "Smart (meaning)" recall. Smart reuses an embedding model the owner
 * already set up for a bot's knowledge; a model on this computer sends nothing out, a remote one
 * needs an explicit OK because memory texts are sent to it. The re-index runs in the background.
 */
export function MemoryRecallQuality() {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const [status, setStatus] = useState<MemoryEmbedding | null>(null);
  const [source, setSource] = useState("");
  const [consent, setConsent] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    if (!client) return;
    setStatus(await client.getMemoryEmbedding());
  }, [client]);

  useEffect(() => {
    void load().catch((e: unknown) => setError(humanLine(e)));
  }, [load]);

  // Poll while the background re-index runs.
  useEffect(() => {
    if (status?.state !== "rebuilding") return undefined;
    const timer = setInterval(() => void load().catch(() => undefined), 1500);
    return () => clearInterval(timer);
  }, [status?.state, load]);

  if (!status) {
    return error ? (
      <p role="alert" className={form.error}>
        {error}
      </p>
    ) : null;
  }

  const chosen = status.sources.find((s) => s.bot_id === (source || status.sources[0]?.bot_id));
  const change = async (mode: "basic" | "smart") => {
    if (!client) return;
    setBusy(true);
    setError(null);
    try {
      setStatus(
        await client.putMemoryEmbedding({
          mode,
          from_bot: mode === "smart" ? (chosen?.bot_id ?? null) : null,
          consent_send_text: consent
        })
      );
    } catch (e) {
      setError(humanLine(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section data-testid="memory-recall-quality">
      <h4>{t("memoryRecall.title")}</h4>
      <p className={form.hint} role="status">
        {t(`memoryRecall.state.${status.mode}`)}
        {status.state === "rebuilding"
          ? ` · ${t("memoryRecall.progress", { done: status.done, total: status.total })}`
          : ""}
        {status.state === "waiting" ? ` · ${t("memoryRecall.waiting")}` : ""}
      </p>
      <p className={form.hint}>{t(`memoryRecall.explain.${status.mode}`)}</p>
      {status.state === "failed" ? (
        <p role="alert" className={form.error}>
          {t("memoryRecall.failed")}
        </p>
      ) : null}
      {status.mode === "basic" ? (
        status.sources.length === 0 ? (
          <p className={form.hint}>{t("memoryRecall.noSource")}</p>
        ) : (
          <div className={form.row}>
            <label className={`${form.field} ${form.grow}`}>
              {t("memoryRecall.source")}
              <select
                className={form.select}
                value={chosen?.bot_id ?? ""}
                disabled={busy}
                onChange={(e) => setSource(e.target.value)}
              >
                {status.sources.map((s) => (
                  <option key={s.bot_id} value={s.bot_id}>
                    {`${s.bot_name} — ${s.model}`}
                  </option>
                ))}
              </select>
            </label>
            {chosen && !chosen.local ? (
              <label className={form.hint}>
                <input type="checkbox" checked={consent} onChange={(e) => setConsent(e.target.checked)} />{" "}
                {t("memoryRecall.consent", { provider: chosen.provider_id })}
              </label>
            ) : null}
            <Button
              variant="secondary"
              disabled={busy || !chosen || (!chosen.local && !consent)}
              onClick={() => void change("smart")}
            >
              {t("memoryRecall.enable")}
            </Button>
          </div>
        )
      ) : (
        <Button variant="secondary" disabled={busy || status.state === "rebuilding"} onClick={() => void change("basic")}>
          {t("memoryRecall.disable")}
        </Button>
      )}
      {error ? (
        <p role="alert" className={form.error}>
          {error}
        </p>
      ) : null}
    </section>
  );
}
