import {
  ArrowDown,
  ArrowUp,
  CircleAlert,
  CircleCheck,
  Info,
  KeyRound,
  PlugZap,
  Trash2
} from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type {
  WebSearchConfig,
  WebSearchConfigInput,
  WebSearchTestReport
} from "../../api/types";
import formStyles from "../../components/Form.module.css";
import { Switch } from "../../components/SettingsUi";
import { useToast } from "../../components/Toast";
import { useDaemon } from "../../state/DaemonContext";
import styles from "./WebSearchCard.module.css";

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
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

  const [searxngUrl, setSearxngUrl] = useState("");
  const [braveKey, setBraveKey] = useState("");
  const [tavilyKey, setTavilyKey] = useState("");

  const [testQuery, setTestQuery] = useState("");
  const [testing, setTesting] = useState(false);
  const [report, setReport] = useState<WebSearchTestReport | null>(null);

  const applyConfig = useCallback((next: WebSearchConfig) => {
    setConfig(next);
    setSearxngUrl(next.searxng_url ?? "");
  }, []);

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

  const patch = async (input: WebSearchConfigInput, toastMessage?: string) => {
    if (!client) {
      return;
    }
    setSaving(true);
    try {
      const updated = await client.updateWebSearch(input);
      applyConfig(updated);
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

  const saveCredentials = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!client || !config) {
      return;
    }
    const input: WebSearchConfigInput = {};
    const trimmedUrl = searxngUrl.trim();
    if (trimmedUrl !== (config.searxng_url ?? "")) {
      input.searxng_url = trimmedUrl;
    }
    if (braveKey.trim().length > 0) {
      input.brave_key = braveKey.trim();
    }
    if (tavilyKey.trim().length > 0) {
      input.tavily_key = tavilyKey.trim();
    }
    if (
      input.searxng_url === undefined &&
      input.brave_key === undefined &&
      input.tavily_key === undefined
    ) {
      return;
    }
    setSaving(true);
    try {
      const updated = await client.updateWebSearch(input);
      applyConfig(updated);
      if (input.brave_key !== undefined) {
        setBraveKey("");
      }
      if (input.tavily_key !== undefined) {
        setTavilyKey("");
      }
      setReport(null);
      toast.show(t("settingsModal.webSearch.toast.saved"), "success");
    } catch (err) {
      toast.show(errorText(err), "danger");
    } finally {
      setSaving(false);
    }
  };

  const runTest = async () => {
    if (!client) {
      return;
    }
    setTesting(true);
    try {
      const q = testQuery.trim();
      const res = await client.testWebSearch(q ? { query: q } : {});
      setReport(res);
    } catch (e) {
      toast.show(errorText(e), "danger");
    } finally {
      setTesting(false);
    }
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
    const enabled = isKeylessBackend(id) ? cfg[id] : false;
    return enabled
      ? { text: t("settingsModal.webSearch.status.active"), active: true }
      : { text: t("settingsModal.webSearch.status.disabled"), active: false };
  };

  const hasCredentialEdits =
    config !== null &&
    (searxngUrl.trim() !== (config.searxng_url ?? "") ||
      braveKey.trim().length > 0 ||
      tavilyKey.trim().length > 0);

  return (
    <div className={styles.root} data-testid="web-search-card">
      <p className={styles.note}>
        <Info aria-hidden="true" />
        <span>{t("settingsModal.webSearch.note")}</span>
      </p>

      {loadError ? (
        <p className={formStyles.error}>
          {t("settingsModal.webSearch.loadError", { message: loadError })}
        </p>
      ) : null}

      {config ? (
        <>
          <div className={styles.section}>
            <h4 className={styles.subheading}>{t("settingsModal.webSearch.orderTitle")}</h4>
            <p className={styles.subhint}>{t("settingsModal.webSearch.orderHint")}</p>
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
                        onClick={() => moveBackend(index, -1)}
                      >
                        <ArrowUp />
                      </Button>
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={saving || index === config.order.length - 1}
                        aria-label={t("settingsModal.webSearch.moveDown", { name: label })}
                        onClick={() => moveBackend(index, 1)}
                      >
                        <ArrowDown />
                      </Button>
                    </div>
                  </li>
                );
              })}
            </ul>
          </div>

          <form className={styles.card} onSubmit={(e) => void saveCredentials(e)}>
            <h4 className={styles.subheading}>{t("settingsModal.webSearch.credentialsTitle")}</h4>

            <div className={formStyles.field}>
              <label className={formStyles.label} htmlFor="web-searxng-url">
                {t("settingsModal.webSearch.searxngLabel")}
              </label>
              <input
                id="web-searxng-url"
                type="url"
                className={formStyles.input}
                value={searxngUrl}
                placeholder="http://127.0.0.1:8080"
                autoComplete="off"
                onChange={(e) => setSearxngUrl(e.target.value)}
              />
              <p className={formStyles.hint}>{t("settingsModal.webSearch.searxngHint")}</p>
            </div>

            <div className={formStyles.field}>
              <label className={formStyles.label} htmlFor="web-brave-key">
                {t("settingsModal.webSearch.braveKeyLabel")}
              </label>
              <div className={styles.keyRow}>
                <input
                  id="web-brave-key"
                  type="password"
                  className={[formStyles.input, styles.keyInput].join(" ")}
                  value={braveKey}
                  autoComplete="off"
                  placeholder={
                    config.brave_key_set
                      ? t("settingsModal.webSearch.keyStoredPlaceholder")
                      : t("settingsModal.webSearch.keyEmptyPlaceholder")
                  }
                  onChange={(e) => setBraveKey(e.target.value)}
                />
                {config.brave_key_set ? (
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={saving}
                    onClick={() =>
                      void patch(
                        { clear_brave_key: true },
                        t("settingsModal.webSearch.toast.keyCleared")
                      )
                    }
                  >
                    <Trash2 />
                    {t("settingsModal.webSearch.clearKey")}
                  </Button>
                ) : null}
              </div>
              <p className={formStyles.hint}>
                <KeyRound aria-hidden="true" /> {t("settingsModal.webSearch.keyWriteOnlyHint")}
              </p>
            </div>

            <div className={formStyles.field}>
              <label className={formStyles.label} htmlFor="web-tavily-key">
                {t("settingsModal.webSearch.tavilyKeyLabel")}
              </label>
              <div className={styles.keyRow}>
                <input
                  id="web-tavily-key"
                  type="password"
                  className={[formStyles.input, styles.keyInput].join(" ")}
                  value={tavilyKey}
                  autoComplete="off"
                  placeholder={
                    config.tavily_key_set
                      ? t("settingsModal.webSearch.keyStoredPlaceholder")
                      : t("settingsModal.webSearch.keyEmptyPlaceholder")
                  }
                  onChange={(e) => setTavilyKey(e.target.value)}
                />
                {config.tavily_key_set ? (
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={saving}
                    onClick={() =>
                      void patch(
                        { clear_tavily_key: true },
                        t("settingsModal.webSearch.toast.keyCleared")
                      )
                    }
                  >
                    <Trash2 />
                    {t("settingsModal.webSearch.clearKey")}
                  </Button>
                ) : null}
              </div>
              <p className={formStyles.hint}>{t("settingsModal.webSearch.tavilyHint")}</p>
            </div>

            <div className={styles.actions}>
              <Button
                type="submit"
                size="sm"
                variant="primary"
                disabled={saving || !hasCredentialEdits}
              >
                {t("settingsModal.webSearch.save")}
              </Button>
            </div>
          </form>

          <div className={styles.card}>
            <h4 className={styles.subheading}>{t("settingsModal.webSearch.testTitle")}</h4>
            <div className={styles.keyRow}>
              <input
                type="text"
                className={[formStyles.input, styles.keyInput].join(" ")}
                value={testQuery}
                placeholder="rust programming"
                aria-label={t("settingsModal.webSearch.testQueryLabel")}
                onChange={(e) => setTestQuery(e.target.value)}
              />
              <Button
                size="sm"
                variant="secondary"
                disabled={testing || saving}
                data-testid="web-search-test-btn"
                onClick={() => void runTest()}
              >
                <PlugZap />
                {testing
                  ? t("settingsModal.webSearch.testing")
                  : t("settingsModal.webSearch.test")}
              </Button>
            </div>

            {report ? (
              <div className={styles.result} data-testid="web-search-test-result">
                <div
                  className={[
                    styles.pill,
                    report.ok ? styles.pillOk : styles.pillBad
                  ].join(" ")}
                >
                  {report.ok ? (
                    <CircleCheck aria-hidden="true" />
                  ) : (
                    <CircleAlert aria-hidden="true" />
                  )}
                  <span>
                    {report.ok && report.backend
                      ? t("settingsModal.webSearch.testOk", {
                          backend: t(backendLabelKey(report.backend)),
                          count: report.result_count,
                          ms: report.elapsed_ms
                        })
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
            ) : null}
          </div>
        </>
      ) : null}
    </div>
  );
}
