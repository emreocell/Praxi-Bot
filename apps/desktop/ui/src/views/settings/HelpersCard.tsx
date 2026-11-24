import { useCallback, useEffect, useState } from "react";
import { ShieldAlert, Sparkles, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button, Card } from "ui";
import type { DecisionHelper, EmbeddingProviderInput, EmbeddingProviderRow, EmbeddingsSetup } from "../../api/types";
import form from "../../components/Form.module.css";
import { humanLine } from "../../lib/humanError";
import { DOCS_BASE_URL } from "../../lib/helpFaq";
import { freshId, knownState, meterFraction, meterTone, providerId, shortCount } from "../../lib/embeddings";
import { useDaemon } from "../../state/DaemonContext";
import styles from "./HelpersCard.module.css";

type Kind = "gemini" | "openai" | "compatible";
const PURPOSES = ["router", "memory"] as const;

/** A provider row as the PUT body wants it: everything it has, no key (the stored key stays). */
function asInput(p: EmbeddingProviderRow): EmbeddingProviderInput {
  return {
    id: p.id,
    kind: p.kind,
    base_url: p.base_url ?? null,
    model: p.model,
    dimensions: p.dimensions,
    request_dimensions: p.request_dimensions,
    allow_remote: p.allow_remote,
    allow_local_network: p.allow_local_network,
    limits: p.limits ?? null
  };
}

function Bar({ label, used, limit }: { label: string; used: number; limit?: number | null }) {
  const { t } = useTranslation();
  const fraction = meterFraction(used, limit);
  return (
    <div className={styles.meter}>
      <div className={styles.meterLine}>
        <span>{label}</span>
        <span>{limit ? t("helpers.usage.of", { used: shortCount(used), limit: shortCount(limit) }) : t("helpers.usage.noLimit", { used: shortCount(used) })}</span>
      </div>
      <div
        className={styles.bar}
        data-tone={meterTone(fraction)}
        role="progressbar"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(fraction * 100)}
      >
        <div className={styles.fill} style={{ width: `${fraction * 100}%` }} />
      </div>
    </div>
  );
}

/** Settings > Models > Helpers (optional): embedding providers and the decision helper. Nothing here is needed to start. */
export function HelpersCard() {
  const { t } = useTranslation();
  const { client, activeComputerId } = useDaemon();
  // The owner key only goes to this computer's daemon.
  const remote = activeComputerId !== "local";
  const [setup, setSetup] = useState<EmbeddingsSetup | null>(null);
  const [helper, setHelper] = useState<DecisionHelper | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [tests, setTests] = useState<Record<string, string>>({});

  const [adding, setAdding] = useState(false);
  const [kind, setKind] = useState<Kind>("gemini");
  const [model, setModel] = useState("gemini-embedding-001");
  const [dimensions, setDimensions] = useState("768");
  const [baseUrl, setBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [reuse, setReuse] = useState(false);
  const [purposes, setPurposes] = useState<string[]>(["router", "memory"]);
  const [consent, setConsent] = useState(false);
  // Consent is for one destination: changing provider, endpoint or model clears the tick.
  useEffect(() => {
    setConsent(false);
  }, [kind, baseUrl, model]);

  const load = useCallback(async () => {
    if (!client) {
      return;
    }
    try {
      setSetup(await client.getEmbeddings());
      setHelper(await client.getDecisionHelper());
    } catch (e) {
      setError(humanLine(e));
    }
  }, [client]);
  useEffect(() => {
    void load();
  }, [load]);
  // The usage bars move while the card is open.
  useEffect(() => {
    const timer = window.setInterval(() => void load(), 15000);
    return () => window.clearInterval(timer);
  }, [load]);

  const catalogModels = (setup?.catalog.models ?? []).filter((m) => m.provider === kind);
  const providerName = kind === "gemini" ? "Google (Gemini)" : kind === "openai" ? "OpenAI" : baseUrl.trim() || t("helpers.add.otherName");
  const reusable = kind === "gemini" && (setup?.reusable_keys ?? []).includes("gemini");

  const pickKind = (next: Kind) => {
    setKind(next);
    const first = (setup?.catalog.models ?? []).find((m) => m.provider === next);
    setModel(first?.model ?? "");
    setDimensions(String(first?.dimensions ?? 768));
    setReuse(false);
  };
  const pickModel = (id: string) => {
    setModel(id);
    const m = catalogModels.find((c) => c.model === id);
    if (m) {
      setDimensions(String(m.dimensions));
    }
  };

  const save = async (providers: EmbeddingProviderInput[], chains: Record<string, string[]>) => {
    if (!client) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      setSetup(await client.putEmbeddings({ providers, chains, remote_denied_bots: setup?.remote_denied_bots ?? [] }));
    } catch (e) {
      setError(humanLine(e));
    } finally {
      setBusy(false);
    }
  };

  const current = () => (setup?.providers ?? []).map(asInput);
  const chains = () => ({ ...(setup?.chains ?? {}) });

  const add = async () => {
    const id = freshId(providerId(kind === "compatible" ? baseUrl.replace(/^https?:\/\//, "") : kind), (setup?.providers ?? []).map((p) => p.id));
    const entry: EmbeddingProviderInput = {
      id,
      kind,
      base_url: kind === "compatible" ? baseUrl.trim() : null,
      model: model.trim(),
      dimensions: Number(dimensions),
      request_dimensions: kind !== "compatible",
      allow_remote: consent,
      confirm_remote: consent,
      api_key: reuse || !apiKey.trim() ? null : apiKey.trim(),
      reuse_key_from: reuse ? "gemini" : null
    };
    const next = chains();
    for (const purpose of purposes) {
      next[purpose] = [...(next[purpose] ?? []), id];
    }
    await save([...current(), entry], next);
    setApiKey("");
    setConsent(false);
    setAdding(false);
  };

  const remove = async (id: string) => {
    const next = Object.fromEntries(Object.entries(chains()).map(([k, v]) => [k, v.filter((x) => x !== id)]));
    await save(current().filter((p) => p.id !== id), next);
  };

  const toggleConsent = async (id: string, allow: boolean) => {
    await save(current().map((p) => (p.id === id ? { ...p, allow_remote: allow, confirm_remote: allow } : p)), chains());
  };

  const test = async (id: string) => {
    if (!client) {
      return;
    }
    setTests((s) => ({ ...s, [id]: "…" }));
    try {
      const r = await client.testEmbedding(id);
      setTests((s) => ({ ...s, [id]: r.outcome }));
      await load();
    } catch (e) {
      setTests((s) => ({ ...s, [id]: humanLine(e) }));
    }
  };

  const flipHelper = async (enabled: boolean) => {
    if (!client) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      setHelper(await client.setDecisionHelper(enabled));
    } catch (e) {
      setError(humanLine(e));
    } finally {
      setBusy(false);
    }
  };

  const valid =
    model.trim() !== "" &&
    Number(dimensions) >= 1 &&
    (kind !== "compatible" || baseUrl.trim() !== "") &&
    purposes.length > 0 &&
    consent &&
    (reuse || apiKey.trim() !== "" || kind === "compatible");

  return (
    <Card padding="md" data-testid="helpers-card">
      <p className={form.hint}>
        <Sparkles size={14} aria-hidden /> {t("helpers.keyOptional")}{" "}
        <a href={`${DOCS_BASE_URL}embeddings.md`} target="_blank" rel="noreferrer">
          {t("helpers.learnMore")}
        </a>
      </p>

      <h3 className={styles.heading}>{t("helpers.embeddings.title")}</h3>
      <p className={form.hint}>{t("helpers.embeddings.description")}</p>
      {(setup?.providers ?? []).length === 0 ? <p className={form.hint}>{t("helpers.embeddings.none")}</p> : null}
      <ul className={styles.list}>
        {(setup?.providers ?? []).map((p) => {
          const state = knownState(p.state);
          return (
            <li key={p.id} className={styles.provider} data-testid={`embedding-${p.id}`}>
              <div className={styles.row}>
                <strong>{p.id}</strong>
                <span className={styles.model}>{p.model}</span>
                <span className={styles.state} data-state={state}>
                  {t(`helpers.state.${state}`)}
                  {p.retry_in_secs > 0 ? ` · ${t("helpers.retryIn", { seconds: p.retry_in_secs })}` : ""}
                </span>
              </div>
              <Bar label={t("helpers.usage.perMinute")} used={p.usage.requests_per_minute.used} limit={p.usage.requests_per_minute.limit} />
              <Bar label={t("helpers.usage.tokensPerMinute")} used={p.usage.tokens_per_minute.used} limit={p.usage.tokens_per_minute.limit} />
              <Bar label={t("helpers.usage.perDay")} used={p.usage.requests_per_day.used} limit={p.usage.requests_per_day.limit} />
              {p.remote ? (
                <label className={styles.check}>
                  <input type="checkbox" checked={p.allow_remote} disabled={busy || remote} onChange={(e) => void toggleConsent(p.id, e.target.checked)} />
                  <span>{t("helpers.allowSending")}</span>
                </label>
              ) : null}
              <div className={styles.actions}>
                <Button variant="ghost" disabled={busy || remote} onClick={() => void test(p.id)}>
                  {t("helpers.test")}
                </Button>
                <Button variant="ghost" disabled={busy || remote} onClick={() => void remove(p.id)}>
                  <Trash2 size={14} aria-hidden /> {t("helpers.remove")}
                </Button>
                {tests[p.id] ? (
                  <span role="status" className={tests[p.id] === "ok" ? form.success : form.hint}>
                    {tests[p.id] === "ok" ? t("helpers.testOk") : tests[p.id] === "…" ? "…" : t(`helpers.state.${knownState(tests[p.id])}`, { defaultValue: tests[p.id] })}
                  </span>
                ) : null}
              </div>
            </li>
          );
        })}
      </ul>
      <p className={form.hint}>{t("helpers.fallbackNote")}</p>

      {remote ? (
        <p className={form.hint}>{t("helpers.remote")}</p>
      ) : adding ? (
        <form
          className={styles.form}
          onSubmit={(e) => {
            e.preventDefault();
            if (valid) {
              void add();
            }
          }}
        >
          <label className={form.field}>
            <span className={form.label}>{t("helpers.add.kind")}</span>
            <select className={form.select} value={kind} onChange={(e) => pickKind(e.target.value as Kind)}>
              <option value="gemini">Google Gemini</option>
              <option value="openai">OpenAI</option>
              <option value="compatible">{t("helpers.add.compatible")}</option>
            </select>
          </label>
          {kind === "compatible" ? (
            <label className={form.field}>
              <span className={form.label}>{t("helpers.add.address")}</span>
              <input className={form.input} value={baseUrl} placeholder="http://localhost:11434/v1" onChange={(e) => setBaseUrl(e.target.value)} />
              <span className={form.hint}>{t("helpers.add.addressHint")}</span>
            </label>
          ) : null}
          <label className={form.field}>
            <span className={form.label}>{t("helpers.add.model")}</span>
            {kind === "compatible" ? (
              <input className={form.input} value={model} onChange={(e) => setModel(e.target.value)} />
            ) : (
              <select className={form.select} value={model} onChange={(e) => pickModel(e.target.value)}>
                {catalogModels.map((m) => (
                  <option key={m.model} value={m.model}>
                    {m.label}
                  </option>
                ))}
              </select>
            )}
          </label>
          <label className={form.field}>
            <span className={form.label}>{t("helpers.add.dimensions")}</span>
            <input className={form.input} inputMode="numeric" value={dimensions} onChange={(e) => setDimensions(e.target.value)} />
            <span className={form.hint}>{t("helpers.add.dimensionsHint")}</span>
          </label>
          {reusable ? (
            <label className={styles.check}>
              <input type="checkbox" checked={reuse} onChange={(e) => setReuse(e.target.checked)} />
              <span>{t("helpers.add.reuseGemini")}</span>
            </label>
          ) : null}
          {!reuse ? (
            <label className={form.field}>
              <span className={form.label}>{t("helpers.add.key")}</span>
              <input className={form.input} type="password" autoComplete="off" value={apiKey} onChange={(e) => setApiKey(e.target.value)} />
              <span className={form.hint}>{kind === "compatible" ? t("helpers.add.keyOptional") : t("helpers.add.keyStored")}</span>
            </label>
          ) : null}
          <fieldset className={styles.fieldset}>
            <legend className={form.label}>{t("helpers.add.use")}</legend>
            {PURPOSES.map((p) => (
              <label key={p} className={styles.check}>
                <input
                  type="checkbox"
                  checked={purposes.includes(p)}
                  onChange={(e) => setPurposes((s) => (e.target.checked ? [...s, p] : s.filter((x) => x !== p)))}
                />
                <span>{t(`helpers.purpose.${p}`)}</span>
              </label>
            ))}
          </fieldset>
          <div className={styles.notice} role="note">
            <ShieldAlert size={16} aria-hidden />
            <div>
              <p>{t("helpers.privacy.notice", { provider: providerName })}</p>
              <p className={form.hint}>{t("helpers.privacy.safeguards")}</p>
              <label className={styles.check}>
                <input type="checkbox" checked={consent} onChange={(e) => setConsent(e.target.checked)} />
                <span>{t("helpers.privacy.agree", { provider: providerName })}</span>
              </label>
            </div>
          </div>
          <div className={styles.actions}>
            <Button type="submit" disabled={busy || !valid}>
              {t("helpers.add.save")}
            </Button>
            <Button variant="ghost" onClick={() => setAdding(false)}>
              {t("helpers.add.cancel")}
            </Button>
          </div>
        </form>
      ) : (
        <Button variant="ghost" onClick={() => setAdding(true)}>
          {t("helpers.add.open")}
        </Button>
      )}

      <h3 className={styles.heading}>{t("helpers.decision.title")}</h3>
      <p className={form.hint}>{t("helpers.decision.description")}</p>
      {helper ? (
        <div className={styles.actions}>
          <span>{helper.enabled ? t("helpers.decision.on") : t("helpers.decision.off")}</span>
          {helper.custom_file ? (
            <span className={form.hint}>{t("helpers.decision.custom")}</span>
          ) : helper.enabled ? (
            <Button variant="ghost" disabled={busy || remote} onClick={() => void flipHelper(false)}>
              {t("helpers.decision.turnOff")}
            </Button>
          ) : helper.available ? (
            <Button variant="ghost" disabled={busy || remote} onClick={() => void flipHelper(true)}>
              {t("helpers.decision.turnOn")}
            </Button>
          ) : (
            <span className={form.hint}>{t("helpers.decision.needKey")}</span>
          )}
        </div>
      ) : null}
      {helper?.free_tier ? (
        <p className={form.hint}>
          {t("helpers.decision.limits", {
            rpm: helper.free_tier.rpm ?? 0,
            tpm: shortCount(helper.free_tier.tpm ?? 0),
            rpd: shortCount(helper.free_tier.rpd ?? 0),
            date: helper.last_reviewed
          })}
        </p>
      ) : null}
      {error ? (
        <p className={form.error} role="alert">
          {error}
        </p>
      ) : null}
    </Card>
  );
}
