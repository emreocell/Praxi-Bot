import {
  ArrowDown,
  ArrowUp,
  CircleAlert,
  CircleCheck,
  PlugZap,
  Trash2,
  TriangleAlert
} from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Card } from "ui";
import type {
  WebSearchConfig,
  WebSearchConfigInput,
  WebSearchTestReport
} from "../../api/types";
import { MoreInfo } from "../../components/ChoiceList";
import { FormField, formStyles } from "../../components/SettingsForm";
import { SettingGroup, Switch, SwitchField } from "../../components/SettingsUi";
import { useToast } from "../../components/Toast";
import { applyWebSearchUpdate } from "../../lib/webSearchDraft";
import {
  PROVIDERS,
  type ProviderId,
  clearInput,
  draftChanged,
  initialProvider,
  providerConfigured,
  saveInput
} from "../../lib/webSearchProvider";
import { useDaemon } from "../../state/DaemonContext";
import styles from "./WebSearchCard.module.css";
import { humanLine } from "../../lib/humanError";

function errorText(e: unknown): string {
  return humanLine(e);
}

type KeylessBackendId = "duckduckgo" | "duckduckgo_lite" | "bing";

function isKeylessBackend(id: string): id is KeylessBackendId {
  return id === "duckduckgo" || id === "duckduckgo_lite" || id === "bing";
}

function backendLabelKey(id: string): string {
  switch (id) {
    case "searxng":
      return "settingsModal.webSearch.backends.searxng";
    case "brave":
      return "settingsModal.webSearch.backends.brave";
    case "tavily":
      return "settingsModal.webSearch.backends.tavily";
    case "firecrawl":
      return "settingsModal.webSearch.backends.firecrawl";
    case "duckduckgo":
      return "settingsModal.webSearch.backends.duckduckgo";
    case "duckduckgo_lite":
      return "settingsModal.webSearch.backends.duckduckgo_lite";
    case "bing":
      return "settingsModal.webSearch.backends.bing";
    default:
      return id;
  }
}

export function WebSearchCard() {
  const { t } = useTranslation();
  const toast = useToast();
  const { client } = useDaemon();

  const [config, setConfig] = useState<WebSearchConfig | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const [provider, setProvider] = useState<ProviderId>("brave");
  const [providerChosen, setProviderChosen] = useState(false);
  const [searxngUrl, setSearxngUrl] = useState("");
  const [braveKey, setBraveKey] = useState("");
  const [tavilyKey, setTavilyKey] = useState("");
  const [firecrawlKey, setFirecrawlKey] = useState("");
  const [firecrawlUrl, setFirecrawlUrl] = useState("");
  const [firecrawlSelfHosted, setFirecrawlSelfHosted] = useState(false);
  const [firecrawlScrape, setFirecrawlScrape] = useState(false);
  const [firecrawlAllow, setFirecrawlAllow] = useState(false);

  const [testQuery, setTestQuery] = useState("");
  const [testing, setTesting] = useState(false);
  const [report, setReport] = useState<WebSearchTestReport | null>(null);
  // Where the report is shown: under the provider that was tested, or under "Try a search".
  const [reportFor, setReportFor] = useState<"provider" | "chain">("chain");

  const applyConfig = useCallback((next: WebSearchConfig) => {
    setConfig(next);
    setSearxngUrl(next.searxng_url ?? "");
    setFirecrawlUrl(next.firecrawl_url ?? "");
    setFirecrawlSelfHosted(Boolean(next.firecrawl_url));
    setFirecrawlScrape(next.firecrawl_scrape);
    setFirecrawlAllow(next.firecrawl_allow_self_hosted_scrape);
  }, []);

  const applyDraft = useCallback(
    (sent: WebSearchConfigInput, updated: WebSearchConfig) => {
      setConfig(updated);
      const next = applyWebSearchUpdate(
        { searxngUrl, braveKey, tavilyKey },
        sent,
        updated
      );
      setSearxngUrl(next.searxngUrl);
      setBraveKey(next.braveKey);
      setTavilyKey(next.tavilyKey);
      if (sent.firecrawl_key !== undefined) setFirecrawlKey("");
      if (sent.firecrawl_url !== undefined) {
        setFirecrawlUrl(updated.firecrawl_url ?? "");
        setFirecrawlSelfHosted(Boolean(updated.firecrawl_url));
      }
      if (sent.firecrawl_scrape !== undefined) setFirecrawlScrape(updated.firecrawl_scrape);
      if (sent.firecrawl_allow_self_hosted_scrape !== undefined || sent.firecrawl_url !== undefined) setFirecrawlAllow(updated.firecrawl_allow_self_hosted_scrape);
    },
    [braveKey, searxngUrl, tavilyKey]
  );

  const refresh = useCallback(async () => {
    if (!client) {
      return;
    }
    try {
      const dto = await client.getWebSearch();
      applyConfig(dto);
      setLoadError(null);
    } catch (e) {
      setLoadError(errorText(e));
    }
  }, [applyConfig, client]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Open on the provider that is already set up (the first one), until the owner picks one.
  useEffect(() => {
    if (config && !providerChosen) {
      setProvider(initialProvider(config));
    }
  }, [config, providerChosen]);

  const patch = async (input: WebSearchConfigInput, toastMessage?: string) => {
    if (!client) {
      return;
    }
    setSaving(true);
    try {
      const updated = await client.updateWebSearch(input);
      applyDraft(input, updated);
      setReport(null);
      if (toastMessage) {
        toast.show(toastMessage, "success");
      }
    } catch (e) {
      toast.show(errorText(e), "danger");
    } finally {
      setSaving(false);
    }
  };

  const moveBackend = (index: number, delta: -1 | 1) => {
    if (!config) {
      return;
    }
    const target = index + delta;
    if (target < 0 || target >= config.order.length) {
      return;
    }
    const nextOrder = [...config.order];
    const [item] = nextOrder.splice(index, 1);
    nextOrder.splice(target, 0, item);
    void patch({ order: nextOrder }, t("settingsModal.webSearch.toast.saved"));
  };

  const toggleKeyless = (id: KeylessBackendId, enabled: boolean) => {
    const body: WebSearchConfigInput =
      id === "duckduckgo"
        ? { duckduckgo: enabled }
        : id === "duckduckgo_lite"
          ? { duckduckgo_lite: enabled }
          : { bing: enabled };
    void patch(body, t("settingsModal.webSearch.toast.saved"));
  };

  const probe = async (backend?: ProviderId) => {
    if (!client) {
      return;
    }
    setTesting(true);
    try {
      const q = testQuery.trim();
      const res = await client.testWebSearch({
        ...(q ? { query: q } : {}),
        ...(backend ? { backend } : {})
      });
      setReport(res);
      setReportFor(backend ? "provider" : "chain");
    } catch (e) {
      toast.show(errorText(e), "danger");
    } finally {
      setTesting(false);
    }
  };

  const draftValue = (id: ProviderId): string =>
    (id === "brave" ? braveKey : id === "tavily" ? tavilyKey : searxngUrl).trim();

  /** Save what was typed for the chosen provider, then try a search through it. */
  const saveProvider = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!client || !config) {
      return;
    }
    if (selectedHasDraft) {
      const input: WebSearchConfigInput = provider === "firecrawl" ? {
        firecrawl_url: firecrawlSelfHosted ? firecrawlUrl.trim() : "",
        ...(firecrawlKey.trim() ? { firecrawl_key: firecrawlKey.trim() } : {}),
        firecrawl_scrape: firecrawlScrape,
        firecrawl_allow_self_hosted_scrape: firecrawlSelfHosted && firecrawlAllow
      } : saveInput(provider, draftValue(provider));
      setSaving(true);
      try {
        const updated = await client.updateWebSearch(input);
        applyDraft(input, updated);
        setReport(null);
        toast.show(t("settingsModal.webSearch.toast.saved"), "success");
      } catch (err) {
        toast.show(errorText(err), "danger");
        return;
      } finally {
        setSaving(false);
      }
    }
    await probe(provider);
  };

  const clearProvider = (id: ProviderId) => {
    void patch(clearInput(id), t("settingsModal.webSearch.toast.providerRemoved"));
  };

  const statusText = (id: string, cfg: WebSearchConfig): { text: string; active: boolean } => {
    if (id === "searxng") {
      return cfg.searxng_url
        ? { text: t("settingsModal.webSearch.status.active"), active: true }
        : { text: t("settingsModal.webSearch.status.needsUrl"), active: false };
    }
    if (id === "brave") {
      return cfg.brave_key_set
        ? { text: t("settingsModal.webSearch.status.active"), active: true }
        : { text: t("settingsModal.webSearch.status.needsKey"), active: false };
    }
    if (id === "tavily") {
      return cfg.tavily_key_set
        ? { text: t("settingsModal.webSearch.status.active"), active: true }
        : { text: t("settingsModal.webSearch.status.needsKey"), active: false };
    }
    const enabled = id === "firecrawl"
      ? cfg.active_backends.includes(id)
      : isKeylessBackend(id) ? cfg[id] : false;
    return enabled
      ? { text: t("settingsModal.webSearch.status.active"), active: true }
      : { text: t("settingsModal.webSearch.status.disabled"), active: false };
  };

  const selectedConfigured = config !== null && providerConfigured(provider, config);
  const selectedHasDraft = config !== null && (provider === "firecrawl"
    ? Boolean(firecrawlKey.trim()) || (firecrawlSelfHosted ? firecrawlUrl.trim() : "") !== (config.firecrawl_url ?? "") || firecrawlScrape !== config.firecrawl_scrape || (firecrawlSelfHosted && firecrawlAllow) !== config.firecrawl_allow_self_hosted_scrape
    : draftChanged(provider, draftValue(provider), config));
  const busy = saving || testing;

  const keyField = (
    id: "brave" | "tavily",
    value: string,
    setValue: (v: string) => void,
    cfg: WebSearchConfig
  ) => (
    <FormField
      id={`web-${id}-key`}
      label={t(`settingsModal.webSearch.${id}KeyLabel`)}
      helpTitle={t(`settingsModal.webSearch.${id}KeyLabel`)}
      helpBody={t("settingsModal.webSearch.keyWriteOnlyHint")}
      width="md"
    >
      <input
        id={`web-${id}-key`}
        type="password"
        className={formStyles.input}
        value={value}
        autoComplete="off"
        placeholder={
          providerConfigured(id, cfg)
            ? t("settingsModal.webSearch.keyStoredPlaceholder")
            : t("settingsModal.webSearch.keyEmptyPlaceholder")
        }
        onChange={(e) => setValue(e.target.value)}
      />
    </FormField>
  );

  return (
    <div className={styles.root} data-testid="web-search-card">
      {loadError ? (
        <p className={formStyles.error} role="alert">
          {t("settingsModal.webSearch.loadError", { message: loadError })}
        </p>
      ) : null}

      {config?.config_error ? (
        <p className={styles.configWarning} data-testid="web-search-config-warning">
          <TriangleAlert aria-hidden="true" />
          <span>
            {t("settingsModal.webSearch.configError", { message: config.config_error })}
          </span>
        </p>
      ) : null}

      {config?.needs_provider ? (
        <p className={styles.configWarning} data-testid="web-search-needs-provider">
          <TriangleAlert aria-hidden="true" />
          <span>{t("settingsModal.webSearch.needsProvider")}</span>
        </p>
      ) : null}

      {config ? (
        <>
          <Card asChild padding="md" elevated>
            <form className={styles.card} onSubmit={(e) => void saveProvider(e)}>
              <h4 className={styles.subheading}>{t("settingsModal.webSearch.providerTitle")}</h4>

              <div
                className={styles.actions}
                role="group"
                aria-label={t("settingsModal.webSearch.providerChoice")}
              >
                {PROVIDERS.map((id) => (
                  <Button
                    key={id}
                    size="sm"
                    variant={provider === id ? "primary" : "secondary"}
                    aria-pressed={provider === id}
                    data-testid={`web-provider-${id}`}
                    onClick={() => {
                      setProvider(id);
                      setProviderChosen(true);
                    }}
                  >
                    {t(backendLabelKey(id))}
                    {providerConfigured(id, config)
                      ? ` · ${t("settingsModal.webSearch.status.active")}`
                      : ""}
                  </Button>
                ))}
              </div>

              <p className={styles.subhint}>
                {t(`settingsModal.webSearch.providerHelp.${provider}`)}
              </p>

              {provider === "brave"
                ? keyField("brave", braveKey, setBraveKey, config)
                : null}
              {provider === "tavily"
                ? keyField("tavily", tavilyKey, setTavilyKey, config)
                : null}
              {provider === "searxng" ? (
                <FormField
                  id="web-searxng-url"
                  label={t("settingsModal.webSearch.searxngLabel")}
                  width="md"
                >
                  <input
                    id="web-searxng-url"
                    type="url"
                    className={formStyles.input}
                    value={searxngUrl}
                    placeholder="http://127.0.0.1:8080"
                    autoComplete="off"
                    onChange={(e) => setSearxngUrl(e.target.value)}
                  />
                </FormField>
              ) : null}

              {provider === "firecrawl" ? <>
                <FormField id="firecrawl-mode" label={t("settingsModal.webSearch.firecrawl.mode")} width="md">
                  <select id="firecrawl-mode" className={formStyles.input} value={firecrawlSelfHosted ? "self" : "cloud"} onChange={(e) => { setFirecrawlSelfHosted(e.target.value === "self"); setFirecrawlAllow(false); }}>
                    <option value="cloud">{t("settingsModal.webSearch.firecrawl.cloud")}</option>
                    <option value="self">{t("settingsModal.webSearch.firecrawl.selfHosted")}</option>
                  </select>
                </FormField>
                {firecrawlSelfHosted ? <FormField id="firecrawl-url" label={t("settingsModal.webSearch.firecrawl.url")} width="md"><input id="firecrawl-url" type="url" className={formStyles.input} value={firecrawlUrl} onChange={(e) => { setFirecrawlUrl(e.target.value); setFirecrawlAllow(false); }} /></FormField> : null}
                <FormField id="firecrawl-key" label={t("settingsModal.webSearch.firecrawl.key")} helpTitle={t("settingsModal.webSearch.firecrawl.key")} helpBody={t("settingsModal.webSearch.keyWriteOnlyHint")} width="md"><input id="firecrawl-key" type="password" autoComplete="off" className={formStyles.input} value={firecrawlKey} onChange={(e) => setFirecrawlKey(e.target.value)} placeholder={t(config.firecrawl_key_set ? "settingsModal.webSearch.keyStoredPlaceholder" : "settingsModal.webSearch.keyEmptyPlaceholder")} /></FormField>
                <SwitchField checked={firecrawlScrape} onChange={setFirecrawlScrape} label={t("settingsModal.webSearch.firecrawl.useScrape")} />
                {firecrawlSelfHosted ? <SwitchField checked={firecrawlAllow} onChange={setFirecrawlAllow} label={t("settingsModal.webSearch.firecrawl.allow")} hint={t("settingsModal.webSearch.firecrawl.risk")} /> : null}
              </> : null}
              <div className={styles.actions}>
                <Button
                  type="submit"
                  size="sm"
                  variant="primary"
                  disabled={busy || !(selectedHasDraft || selectedConfigured) || (provider === "firecrawl" && firecrawlSelfHosted && !firecrawlUrl.trim())}
                  data-testid="web-provider-save-test"
                >
                  <PlugZap />
                  {testing
                    ? t("settingsModal.webSearch.testing")
                    : selectedHasDraft
                      ? t("settingsModal.webSearch.saveAndTest")
                      : t("settingsModal.webSearch.testProvider")}
                </Button>
                {selectedConfigured ? (
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={busy}
                    onClick={() => clearProvider(provider)}
                  >
                    <Trash2 />
                    {provider === "firecrawl"
                      ? t("settingsModal.webSearch.firecrawl.remove")
                      : provider === "searxng"
                      ? t("settingsModal.webSearch.clearUrl")
                      : t("settingsModal.webSearch.clearKey")}
                  </Button>
                ) : null}
              </div>

              {report && reportFor === "provider" ? <TestResult report={report} /> : null}
            </form>
          </Card>

          <Card padding="md" elevated className={styles.card}>
            <h4 className={styles.subheading}>{t("settingsModal.webSearch.testTitle")}</h4>
            <div className={styles.keyRow}>
              <FormField id="web-search-test-query" label={t("settingsModal.webSearch.testQueryLabel")} width="md">
                <input
                  id="web-search-test-query"
                  type="text"
                  className={formStyles.input}
                  value={testQuery}
                  placeholder="rust programming"
                  onChange={(e) => setTestQuery(e.target.value)}
                />
              </FormField>
              <Button
                size="sm"
                variant="secondary"
                disabled={busy}
                data-testid="web-search-test-btn"
                onClick={() => void probe()}
              >
                <PlugZap />
                {testing
                  ? t("settingsModal.webSearch.testing")
                  : t("settingsModal.webSearch.test")}
              </Button>
            </div>
            {report && reportFor === "chain" ? <TestResult report={report} /> : null}
          </Card>

          <MoreInfo label={t("settingsModal.webSearch.advanced")} testId="web-search-advanced">
            <p>{t("settingsModal.webSearch.note")}</p>
            <div className={styles.section}>
              <h4 className={styles.subheading}>{t("settingsModal.webSearch.orderTitle")}</h4>
              <p className={styles.subhint}>{t("settingsModal.webSearch.orderHint")}</p>
              <SettingGroup>
                <ul className={styles.list}>
                  {config.order.map((id, index) => {
                    const status = statusText(id, config);
                    const label = t(backendLabelKey(id));
                    return (
                      <li key={id} className={styles.row} data-testid={`web-backend-${id}`}>
                        <div className={styles.rowLeft}>
                          <span className={styles.rank}>{index + 1}.</span>
                          <span className={styles.name}>{label}</span>
                          <span
                            className={[
                              styles.badge,
                              status.active ? styles.badgeActive : ""
                            ]
                              .filter(Boolean)
                              .join(" ")}
                          >
                            {status.text}
                          </span>
                        </div>
                        <div className={styles.rowControls}>
                          {isKeylessBackend(id) ? (
                            <Switch
                              checked={config[id]}
                              onChange={(checked) => toggleKeyless(id, checked)}
                              disabled={saving}
                              label={t("settingsModal.webSearch.toggleBackend", { name: label })}
                            />
                          ) : null}
                          <Button
                            size="sm"
                            variant="ghost"
                            disabled={saving || index === 0}
                            aria-label={t("settingsModal.webSearch.moveUp", { name: label })}
                            title={t("settingsModal.webSearch.moveUp", { name: label })}
                            onClick={() => moveBackend(index, -1)}
                          >
                            <ArrowUp />
                          </Button>
                          <Button
                            size="sm"
                            variant="ghost"
                            disabled={saving || index === config.order.length - 1}
                            aria-label={t("settingsModal.webSearch.moveDown", { name: label })}
                            title={t("settingsModal.webSearch.moveDown", { name: label })}
                            onClick={() => moveBackend(index, 1)}
                          >
                            <ArrowDown />
                          </Button>
                        </div>
                      </li>
                    );
                  })}
                </ul>
              </SettingGroup>
            </div>
          </MoreInfo>
        </>
      ) : null}
    </div>
  );
}

/** What a search probe found, in plain words (a robot check gets its own sentence). */
function TestResult({ report }: { report: WebSearchTestReport }) {
  const { t } = useTranslation();
  return (
    <div className={styles.result} data-testid="web-search-test-result">
      <div
        className={[styles.pill, report.ok ? styles.pillOk : styles.pillBad].join(" ")}
      >
        {report.ok ? <CircleCheck aria-hidden="true" /> : <CircleAlert aria-hidden="true" />}
        <span>
          {report.ok && report.backend
            ? t("settingsModal.webSearch.testOk", {
                backend: t(backendLabelKey(report.backend)),
                count: report.result_count,
                ms: report.elapsed_ms
              })
            : report.blocked
              ? t("settingsModal.webSearch.testBlocked", { ms: report.elapsed_ms })
              : t("settingsModal.webSearch.testFailed", {
                  ms: report.elapsed_ms,
                  error: report.error ?? ""
                })}
        </span>
      </div>
      {(report.fallback_errors ?? []).length > 0 ? (
        <p className={formStyles.hint}>
          {t("settingsModal.webSearch.fallbackNotes", {
            details: (report.fallback_errors ?? []).join("; ")
          })}
        </p>
      ) : null}
    </div>
  );
}
