import { Cable, Check, Plus, Search, Server, X } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button } from "ui";
import { BrandIcon } from "../components/BrandIcon";
import { brandIcon } from "../lib/brandIcons";
import { botView } from "../lib/botProfile";
import type { McpCatalogEntry, McpServer } from "../api/types";
import formStyles from "../components/Form.module.css";
import { Modal } from "../components/Modal";
import { Segmented } from "../components/SettingsUi";
import { useToast } from "../components/Toast";
import { openExternal } from "../lib/externalLink";
import {
  ALL_CATEGORIES,
  botAccess,
  catalogCategories,
  connectCatalogEntry,
  customServerConfig,
  filterCatalog,
  filterServers,
  isConnected,
  MissingSecretError,
  statusTone
} from "../lib/mcpApps";
import { useDaemon } from "../state/DaemonContext";
import { useMcp } from "../state/useMcp";
import styles from "./AppsModal.module.css";

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function ConnectedRow({
  server,
  busy,
  onReconnect,
  onDisconnect
}: {
  server: McpServer;
  busy: boolean;
  onReconnect: () => void;
  onDisconnect: () => void;
}) {
  const { t } = useTranslation();
  const { bots } = useDaemon();
  const [confirm, setConfirm] = useState(false);
  const access = botAccess(server, bots);
  const tone = statusTone(server.status);
  const needsReconnect =
    server.status === "error" ||
    server.status === "disconnected" ||
    server.status === "quarantined";
  return (
    <li className={styles.serverRow} data-testid={`mcp-server-${server.name}`}>
      <BrandIcon name={server.name} />
      <div className={styles.serverMain}>
        <div className={styles.serverTitle}>
          <span className={styles.serverName}>{brandIcon(server.name).known ? brandIcon(server.name).name : server.name}</span>
          <span className={`${styles.status} ${styles[`tone_${tone}`]}`}>
            <span className={styles.statusDot} aria-hidden="true" />
            {t(`apps.status.${server.status}`)}
          </span>
        </div>
        <div className={styles.serverMeta}>
          <span>{t("apps.toolCount", { count: server.tool_count })}</span>
          <span className={styles.usedBy} title={access.all ? t("apps.botsAll") : t("apps.botsSome", { names: access.names.join(", ") })}>
            {bots.filter(b => access.all || server.bots?.includes(b.id)).map(b => <BotAvatar key={b.id} bot={botView(b)} size="xs" />)}
          </span>
        </div>
        {server.error ? (
          <p className={`${formStyles.error} ${styles.connectionError}`} title={server.error}>{server.error}</p>
        ) : null}
      </div>
      <div className={styles.serverActions}>
        {needsReconnect ? (
          <Button size="sm" variant="secondary" disabled={busy} onClick={onReconnect}>
            {t("apps.reconnect")}
          </Button>
        ) : null}
        {confirm ? (
          <>
            <Button
              size="sm"
              variant="danger"
              disabled={busy}
              onClick={() => {
                setConfirm(false);
                onDisconnect();
              }}
            >
              {t("apps.disconnectConfirm")}
            </Button>
            <Button size="sm" variant="ghost" onClick={() => setConfirm(false)}>
              {t("common.cancel")}
            </Button>
          </>
        ) : (
          <Button
            size="sm"
            variant="ghost"
            disabled={busy}
            onClick={() => setConfirm(true)}
          >
            <Cable />{t("apps.disconnect")}
          </Button>
        )}
      </div>
    </li>
  );
}

function CatalogCard({
  entry,
  connected,
  onConnect
}: {
  entry: McpCatalogEntry;
  connected: boolean;
  onConnect: (values: Record<string, string>) => Promise<void>;
}) {
  const { t } = useTranslation();
  const secrets = entry.install.required_secrets;
  const [asking, setAsking] = useState(false);
  const [values, setValues] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);

  const submit = async (v: Record<string, string>) => {
    setBusy(true);
    try {
      await onConnect(v);
      setAsking(false);
      setValues({});
    } finally {
      setBusy(false);
    }
  };

  const complete = secrets.every((s) => (values[s.name] ?? "").trim() !== "");

  return (
    <li className={styles.card} data-testid={`mcp-catalog-${entry.id}`}>
      <div className={styles.cardHead}>
        <BrandIcon name={entry.name} />
        <div className={styles.cardTitle}>
          <span className={styles.serverName}>{entry.name}</span>
          <span className={styles.category}>
            {t(`apps.categories.${entry.category}`, {
              defaultValue: entry.category
            })}
          </span>
        </div>
        {connected ? (
          <span className={styles.connectedTag}><Check />{t("apps.connected")}</span>
        ) : (
          <Button
            size="sm"
            variant="primary"
            disabled={busy || asking}
            onClick={() => {
              if (secrets.length === 0) {
                void submit({});
              } else {
                setAsking(true);
              }
            }}
          >
            <Plus />{t("apps.connect")}
          </Button>
        )}
      </div>
      <p className={styles.cardDesc}>{entry.description}</p>
      {asking && !connected ? (
        <form
          className={styles.secretForm}
          onSubmit={(e) => {
            e.preventDefault();
            if (complete) {
              void submit(values);
            }
          }}
        >
          {secrets.map((s, i) => (
            <div key={s.name} className={formStyles.field}>
              <label className={formStyles.label} htmlFor={`${entry.id}-${s.name}`}>
                {s.label}
              </label>
              <input
                id={`${entry.id}-${s.name}`}
                className={formStyles.input}
                type="password"
                autoComplete="off"
                autoFocus={i === 0}
                value={values[s.name] ?? ""}
                onChange={(e) =>
                  setValues((v) => ({ ...v, [s.name]: e.target.value }))
                }
              />
              {s.help_url ? (
                <button
                  type="button"
                  className={styles.link}
                  onClick={() => void openExternal(s.help_url ?? "")}
                >
                  {t("apps.getKey")}
                </button>
              ) : null}
            </div>
          ))}
          <p className={formStyles.hint}>{t("apps.secretHint")}</p>
          <div className={styles.formActions}>
            <Button type="submit" size="sm" variant="primary" disabled={!complete || busy}>
              {t("apps.connectNow")}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              onClick={() => {
                setAsking(false);
                setValues({});
              }}
            >
              {t("common.cancel")}
            </Button>
          </div>
        </form>
      ) : null}
    </li>
  );
}

function CustomServerForm({
  existing,
  onSubmit,
  onCancel
}: {
  existing: string[];
  onSubmit: (cfg: ReturnType<typeof customServerConfig>) => Promise<void>;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  const [form, setForm] = useState({
    name: "",
    transport: "stdio" as "stdio" | "http",
    command: "",
    url: "",
    env: ""
  });
  const [error, setError] = useState<"name" | "command" | "url" | null>(null);
  const [busy, setBusy] = useState(false);
  const set = (patch: Partial<typeof form>) => setForm((f) => ({ ...f, ...patch }));

  return (
    <form
      className={styles.custom}
      onSubmit={(e) => {
        e.preventDefault();
        const result = customServerConfig(form, existing);
        setError(result.ok ? null : result.error);
        if (result.ok) {
          setBusy(true);
          void onSubmit(result).finally(() => setBusy(false));
        }
      }}
    >
      <p className={formStyles.hint}>{t("apps.custom.intro")}</p>
      <div className={formStyles.field}>
        <label className={formStyles.label} htmlFor="mcp-name">
          {t("apps.custom.name")}
        </label>
        <input
          id="mcp-name"
          className={formStyles.input}
          value={form.name}
          placeholder="my-server"
          onChange={(e) => set({ name: e.target.value })}
        />
        {error === "name" ? (
          <p className={formStyles.error}>{t("apps.custom.errName")}</p>
        ) : null}
      </div>
      <Segmented
        label={t("apps.custom.transport")}
        value={form.transport}
        onChange={(transport) => set({ transport })}
        options={[
          { value: "stdio", label: t("apps.custom.stdio") },
          { value: "http", label: t("apps.custom.http") }
        ]}
      />
      {form.transport === "stdio" ? (
        <>
          <div className={formStyles.field}>
            <label className={formStyles.label} htmlFor="mcp-cmd">
              {t("apps.custom.command")}
            </label>
            <input
              id="mcp-cmd"
              className={formStyles.input}
              value={form.command}
              placeholder="npx -y @scope/server"
              onChange={(e) => set({ command: e.target.value })}
            />
            {error === "command" ? (
              <p className={formStyles.error}>{t("apps.custom.errCommand")}</p>
            ) : null}
          </div>
          <div className={formStyles.field}>
            <label className={formStyles.label} htmlFor="mcp-env">
              {t("apps.custom.env")}
            </label>
            <textarea
              id="mcp-env"
              className={formStyles.textarea}
              value={form.env}
              placeholder="KEY=value"
              onChange={(e) => set({ env: e.target.value })}
            />
            <p className={formStyles.hint}>{t("apps.custom.envHint")}</p>
          </div>
        </>
      ) : (
        <div className={formStyles.field}>
          <label className={formStyles.label} htmlFor="mcp-url">
            {t("apps.custom.url")}
          </label>
          <input
            id="mcp-url"
            className={formStyles.input}
            value={form.url}
            placeholder="https://"
            onChange={(e) => set({ url: e.target.value })}
          />
          {error === "url" ? (
            <p className={formStyles.error}>{t("apps.custom.errUrl")}</p>
          ) : null}
        </div>
      )}
      <div className={styles.formActions}>
        <Button type="submit" variant="primary" size="sm" disabled={busy}>
          {t("apps.custom.add")}
        </Button>
        <Button variant="ghost" size="sm" onClick={onCancel}>
          {t("common.cancel")}
        </Button>
      </div>
    </form>
  );
}

/**
 * "Uygulamalar": connected MCP servers and a one-click catalog.  Advanced users
 * add a custom stdio / http server from the "MCP servers" button.
 */
export function AppsModal({
  open,
  onClose
}: {
  open: boolean;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const { client } = useDaemon();
  const { servers, catalog, loading, error, reload } = useMcp(open);
  const [query, setQuery] = useState("");
  const [category, setCategory] = useState(ALL_CATEGORIES);
  const [advanced, setAdvanced] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      setQuery("");
      setCategory(ALL_CATEGORIES);
      setAdvanced(false);
    }
  }, [open]);

  const categories = useMemo(() => catalogCategories(catalog), [catalog]);
  const shownServers = useMemo(() => filterServers(servers, query), [servers, query]);
  const shownCatalog = useMemo(
    () => filterCatalog(catalog, query, category).sort((a,b) => Number(isConnected(a, servers)) - Number(isConnected(b, servers))),
    [catalog, query, category, servers]
  );

  const run = async (key: string, fn: () => Promise<void>) => {
    setBusy(key);
    try {
      await fn();
    } catch (e) {
      toast.show(errorText(e), "danger");
    } finally {
      setBusy(null);
    }
  };

  const connect = async (entry: McpCatalogEntry, values: Record<string, string>) => {
    if (!client) {
      return;
    }
    try {
      await connectCatalogEntry(client, entry, values);
      toast.show(t("apps.connectedToast", { name: entry.name }), "success");
      await reload();
    } catch (e) {
      toast.show(
        e instanceof MissingSecretError ? t("apps.missingSecret") : errorText(e),
        "danger"
      );
      throw e;
    }
  };

  return (
    <Modal open={open} onClose={onClose} label={t("apps.title")}>
      <div className={styles.root}>
        <header className={styles.header}>
          <div className={styles.headTop}>
            <div>
              <h2 className={styles.title}>{t("apps.title")}</h2>
              <p className={styles.subtitle}>{t("apps.subtitle")}</p>
            </div>
            <div className={styles.headActions}>
              <Button
                size="sm"
                variant="secondary"
                active={advanced}
                data-testid="apps-advanced"
                onClick={() => setAdvanced((a) => !a)}
              >
                <Server />{t("apps.mcpServers")}
              </Button>
              <button
                type="button"
                className={styles.close}
                title={t("common.close")}
                aria-label={t("common.close")}
                onClick={onClose}
              >
                <X />
              </button>
            </div>
          </div>
          {advanced ? null : (
            <>
              <div className={styles.searchWrap}><Search /><input
                className={styles.search}
                type="search"
                data-autofocus
                value={query}
                placeholder={t("apps.search")}
                aria-label={t("apps.search")}
                onChange={(e) => setQuery(e.target.value)}
              />
              </div><div className={styles.chips} role="group" aria-label={t("apps.categoriesLabel")}>
                {[ALL_CATEGORIES, ...categories].map((c) => (
                  <button
                    key={c}
                    type="button"
                    aria-pressed={category === c}
                    className={[styles.chip, category === c ? styles.chipActive : ""]
                      .filter(Boolean)
                      .join(" ")}
                    onClick={() => setCategory(c)}
                  >
                    {t(`apps.categories.${c}`, { defaultValue: c })}
                  </button>
                ))}
              </div>
            </>
          )}
        </header>

        <div className={styles.body}>
          {error ? (
            <p className={formStyles.error} role="alert">
              {error}
            </p>
          ) : null}

          {advanced ? (
            <CustomServerForm
              existing={servers.map((s) => s.name)}
              onCancel={() => setAdvanced(false)}
              onSubmit={async (result) => {
                if (!client || !result.ok) {
                  return;
                }
                await run("custom", async () => {
                  await client.createMcpServer(result.config);
                  toast.show(
                    t("apps.connectedToast", { name: result.config.name }),
                    "success"
                  );
                  await reload();
                  setAdvanced(false);
                });
              }}
            />
          ) : (
            <>
              {shownServers.length > 0 ? (
                <section className={styles.section}>
                  <h3 className={styles.sectionLabel}>{t("apps.connectedSection")}</h3>
                  <ul className={styles.serverList}>
                    {shownServers.map((s) => (
                      <ConnectedRow
                        key={s.name}
                        server={s}
                        busy={busy === s.name}
                        onReconnect={() =>
                          void run(s.name, async () => {
                            await client?.reconnectMcpServer(s.name);
                            await reload();
                          })
                        }
                        onDisconnect={() =>
                          void run(s.name, async () => {
                            await client?.deleteMcpServer(s.name);
                            toast.show(t("apps.disconnectedToast", { name: s.name }), "info");
                            await reload();
                          })
                        }
                      />
                    ))}
                  </ul>
                </section>
              ) : null}

              <section className={styles.section}>
                <h3 className={styles.sectionLabel}>{t("apps.catalogSection")}</h3>
                {shownCatalog.length === 0 ? (
                  <p className={styles.empty}><Search />
                    {loading ? t("apps.loading") : t("apps.noResults")}
                  </p>
                ) : (
                  <ul className={styles.grid}>
                    {shownCatalog.map((entry) => (
                      <CatalogCard
                        key={entry.id}
                        entry={entry}
                        connected={isConnected(entry, servers)}
                        onConnect={(values) => connect(entry, values)}
                      />
                    ))}
                  </ul>
                )}
              </section>
            </>
          )}
        </div>
      </div>
    </Modal>
  );
}
