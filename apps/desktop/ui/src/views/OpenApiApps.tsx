import { ChevronDown, ChevronRight, ShieldAlert } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { ApiError } from "../api/client";
import type { OpenApiApp, OpenApiAuthKind, OpenApiOperation } from "../api/types";
import formStyles from "../components/Form.module.css";
import { Segmented } from "../components/SettingsUi";
import { useToast } from "../components/Toast";
import { humanError } from "../lib/humanError";
import {
  type AccessTarget,
  authFromForm,
  filterOperations,
  isOn,
  toggleOperation,
  validSpecUrl,
} from "../lib/openapiApps";
import { useDaemon } from "../state/DaemonContext";
import styles from "./OpenApiApps.module.css";

const KINDS: OpenApiAuthKind[] = ["none", "bearer", "api_key_header", "api_key_query", "basic"];
/** Most rows drawn at once; the search narrows a long document down to what the owner wants. */
const ROW_LIMIT = 200;

function ErrorLine({ error }: { error: unknown }) {
  const h = humanError(error);
  // The daemon's sentence says what exactly is wrong with this document or address.
  const detail = error instanceof ApiError && error.status >= 400 && error.status < 500 ? error.message : "";
  return (
    <div className={styles.error} role="alert">
      <strong>{h.title}</strong> {h.hint}
      {detail ? <span className={styles.errorDetail}>{detail}</span> : null}
    </div>
  );
}

function AddForm({ onAdded }: { onAdded: (app: OpenApiApp) => void }) {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const [source, setSource] = useState<"url" | "file">("url");
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [fileText, setFileText] = useState<{ name: string; text: string } | null>(null);
  const [baseUrl, setBaseUrl] = useState("");
  const [kind, setKind] = useState<OpenApiAuthKind>("none");
  const [authName, setAuthName] = useState("");
  const [username, setUsername] = useState("");
  const [secret, setSecret] = useState("");
  const [problem, setProblem] = useState<string | null>(null);
  const [failure, setFailure] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    setFailure(null);
    if (!name.trim()) return setProblem("name");
    if (source === "url" ? !validSpecUrl(url) : !fileText) return setProblem(source);
    const auth = authFromForm({ kind, name: authName, username, secret });
    if (!auth.ok) return setProblem(auth.error === "name" ? "name_auth" : auth.error);
    setProblem(null);
    if (!client) return;
    setBusy(true);
    try {
      const app = await client.openapiAddApp({
        name: name.trim(),
        ...(source === "url" ? { spec_url: url.trim() } : { spec_text: fileText?.text }),
        ...(baseUrl.trim() ? { base_url: baseUrl.trim() } : {}),
        auth: auth.auth,
      });
      setSecret("");
      setName("");
      setUrl("");
      setFileText(null);
      setBaseUrl("");
      setKind("none");
      onAdded(app);
    } catch (e) {
      setFailure(e);
    } finally {
      setBusy(false);
    }
  };

  const err = (key: string) =>
    problem === key ? (
      <p className={formStyles.error} role="alert">
        {t(`apps.restApis.err.${key}`)}
      </p>
    ) : null;

  return (
    <form
      className={styles.form}
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <div className={formStyles.field}>
        <label className={formStyles.label} htmlFor="oa-name">{t("apps.restApis.name")}</label>
        <input id="oa-name" className={formStyles.input} value={name} placeholder="Shop" onChange={(e) => setName(e.target.value)} />
        {err("name")}
      </div>
      <Segmented
        label={t("apps.restApis.source")}
        value={source}
        onChange={setSource}
        options={[
          { value: "url", label: t("apps.restApis.sourceUrl") },
          { value: "file", label: t("apps.restApis.sourceFile") },
        ]}
      />
      {source === "url" ? (
        <div className={formStyles.field}>
          <label className={formStyles.label} htmlFor="oa-url">{t("apps.restApis.specUrl")}</label>
          <input id="oa-url" className={formStyles.input} value={url} placeholder="https://api.example.com/openapi.json" onChange={(e) => setUrl(e.target.value)} />
          {err("url")}
        </div>
      ) : (
        <div className={formStyles.field}>
          <label className={formStyles.label} htmlFor="oa-file">{t("apps.restApis.specFile")}</label>
          <input
            id="oa-file"
            className={formStyles.input}
            type="file"
            accept=".json,.yaml,.yml,application/json,text/yaml"
            onChange={(e) => {
              const f = e.target.files?.[0];
              if (!f) return setFileText(null);
              void f.text().then((text) => setFileText({ name: f.name, text }));
            }}
          />
          {err("file")}
        </div>
      )}
      <div className={formStyles.field}>
        <label className={formStyles.label} htmlFor="oa-base">{t("apps.restApis.baseUrl")}</label>
        <input id="oa-base" className={formStyles.input} value={baseUrl} placeholder="https://api.example.com/v1" onChange={(e) => setBaseUrl(e.target.value)} />
        <p className={formStyles.hint}>{t("apps.restApis.baseUrlHint")}</p>
      </div>
      <div className={formStyles.field}>
        <label className={formStyles.label} htmlFor="oa-auth">{t("apps.restApis.auth")}</label>
        <select id="oa-auth" className={formStyles.select} value={kind} onChange={(e) => setKind(e.target.value as OpenApiAuthKind)}>
          {KINDS.map((k) => (
            <option key={k} value={k}>{t(`apps.restApis.kind.${k}`)}</option>
          ))}
        </select>
      </div>
      {kind === "api_key_header" || kind === "api_key_query" ? (
        <div className={formStyles.field}>
          <label className={formStyles.label} htmlFor="oa-auth-name">
            {t(kind === "api_key_header" ? "apps.restApis.headerName" : "apps.restApis.paramName")}
          </label>
          <input id="oa-auth-name" className={formStyles.input} value={authName} placeholder={kind === "api_key_header" ? "X-Api-Key" : "api_key"} onChange={(e) => setAuthName(e.target.value)} />
          {err("name_auth")}
        </div>
      ) : null}
      {kind === "basic" ? (
        <div className={formStyles.field}>
          <label className={formStyles.label} htmlFor="oa-user">{t("apps.restApis.username")}</label>
          <input id="oa-user" className={formStyles.input} value={username} autoComplete="off" onChange={(e) => setUsername(e.target.value)} />
          {err("username")}
        </div>
      ) : null}
      {kind !== "none" ? (
        <div className={formStyles.field}>
          <label className={formStyles.label} htmlFor="oa-secret">
            {t(kind === "basic" ? "apps.restApis.password" : kind === "bearer" ? "apps.restApis.token" : "apps.restApis.key")}
          </label>
          <input id="oa-secret" className={formStyles.input} type="password" autoComplete="off" value={secret} onChange={(e) => setSecret(e.target.value)} />
          <p className={formStyles.hint}>{t("apps.restApis.secretHint")}</p>
          {err("secret")}
        </div>
      ) : null}
      {failure ? <ErrorLine error={failure} /> : null}
      <div className={styles.actions}>
        <Button type="submit" variant="primary" size="sm" disabled={busy}>
          {busy ? t("apps.restApis.adding") : t("apps.restApis.add")}
        </Button>
      </div>
    </form>
  );
}

function OperationRow({
  op,
  on,
  disabled,
  onToggle,
}: {
  op: OpenApiOperation;
  on: boolean;
  disabled: boolean;
  onToggle: (on: boolean) => void;
}) {
  const { t } = useTranslation();
  return (
    <li className={styles.op}>
      <label className={styles.opLabel}>
        <input type="checkbox" checked={on} disabled={disabled} onChange={(e) => onToggle(e.target.checked)} />
        <span className={`${styles.method} ${styles[`effect_${op.effect}`]}`}>{op.method}</span>
        <span className={styles.opText}>
          <span className={styles.opPath}>{op.path}</span>
          {op.summary ? <span className={styles.opSummary}>{op.summary}</span> : null}
        </span>
      </label>
      {op.always_ask ? (
        <span className={styles.ask} title={t("apps.restApis.alwaysAskHint")}>
          <ShieldAlert aria-hidden="true" />
          {t("apps.restApis.alwaysAsk")}
        </span>
      ) : null}
    </li>
  );
}

function AppCard({
  app,
  onChange,
  onRemoved,
}: {
  app: OpenApiApp;
  onChange: (app: OpenApiApp) => void;
  onRemoved: (id: string) => void;
}) {
  const { t } = useTranslation();
  const { client, bots } = useDaemon();
  const toast = useToast();
  const [open, setOpen] = useState(false);
  const [target, setTarget] = useState<string>("all");
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<unknown>(null);
  const [confirm, setConfirm] = useState(false);
  const accessTarget: AccessTarget = target === "all" ? { kind: "all" } : { kind: "bot", id: target };
  const allBotIds = useMemo(() => bots.map((b) => b.id), [bots]);
  const shown = useMemo(() => filterOperations(app.operations, query), [app.operations, query]);
  const atLimit = app.enabled_count >= app.max_enabled;

  const run = async (job: () => Promise<OpenApiApp | void>, done?: string) => {
    if (!client) return;
    setBusy(true);
    setFailure(null);
    try {
      const next = await job();
      if (next) onChange(next);
      if (done) toast.show(done, "success");
    } catch (e) {
      setFailure(e);
    } finally {
      setBusy(false);
    }
  };

  const toggle = (op: OpenApiOperation, on: boolean) =>
    run(() =>
      client!.openapiUpdateApp(app.id, { operations: toggleOperation(app.operations, op.name, accessTarget, on, allBotIds) }),
    );

  return (
    <li className={styles.card} data-testid={`openapi-app-${app.id}`}>
      <div className={styles.cardHead}>
        <button
          type="button"
          className={styles.expand}
          aria-expanded={open}
          onClick={() => setOpen((o) => !o)}
        >
          {open ? <ChevronDown aria-hidden="true" /> : <ChevronRight aria-hidden="true" />}
          <span className={styles.cardName}>{app.name}</span>
        </button>
        <span className={styles.cardSub}>
          {app.base_url} · {t("apps.restApis.enabledCount", { enabled: app.enabled_count, total: app.operations.length })}
        </span>
      </div>
      {open ? (
        <div className={styles.cardBody}>
          {app.warnings.map((w) => (
            <p key={w} className={formStyles.error} role="alert">{w}</p>
          ))}
          <p className={formStyles.hint}>
            {app.auth.kind === "none"
              ? t("apps.restApis.noAuth")
              : t("apps.restApis.authSet", { kind: t(`apps.restApis.kind.${app.auth.kind}`) })}
          </p>
          <p className={formStyles.hint}>{t("apps.restApis.rules")}</p>
          <div className={styles.toolbar}>
            <label className={styles.picker}>
              <span className={formStyles.label}>{t("apps.restApis.forBot")}</span>
              <select className={formStyles.select} value={target} onChange={(e) => setTarget(e.target.value)}>
                <option value="all">{t("apps.restApis.allBots")}</option>
                {bots.map((b) => (
                  <option key={b.id} value={b.id}>{b.name}</option>
                ))}
              </select>
            </label>
            <input
              type="search"
              className={formStyles.input}
              value={query}
              placeholder={t("apps.restApis.filter")}
              aria-label={t("apps.restApis.filter")}
              onChange={(e) => setQuery(e.target.value)}
            />
          </div>
          {atLimit ? <p className={formStyles.hint}>{t("apps.restApis.limit", { max: app.max_enabled })}</p> : null}
          {shown.length === 0 ? (
            <p className={formStyles.hint}>{t("apps.restApis.noOperations")}</p>
          ) : (
            <ul className={styles.ops}>
              {shown.slice(0, ROW_LIMIT).map((op) => {
                const on = isOn(op, accessTarget);
                return (
                  <OperationRow
                    key={op.name}
                    op={op}
                    on={on}
                    disabled={busy || (!on && atLimit && !op.enabled)}
                    onToggle={(v) => void toggle(op, v)}
                  />
                );
              })}
            </ul>
          )}
          {shown.length > ROW_LIMIT ? <p className={formStyles.hint}>{t("apps.restApis.more", { count: shown.length - ROW_LIMIT })}</p> : null}
          {app.skipped.length > 0 ? (
            <p className={formStyles.hint}>{t("apps.restApis.skipped", { count: app.skipped.length })}</p>
          ) : null}
          {failure ? <ErrorLine error={failure} /> : null}
          <div className={styles.actions}>
            {app.spec_url ? (
              <Button size="sm" variant="ghost" disabled={busy} onClick={() => void run(() => client!.openapiRefreshApp(app.id), t("apps.restApis.refreshed"))}>
                {t("apps.restApis.refresh")}
              </Button>
            ) : null}
            {confirm ? (
              <>
                <Button
                  size="sm"
                  variant="danger"
                  disabled={busy}
                  onClick={() =>
                    void run(async () => {
                      await client!.openapiRemoveApp(app.id);
                      onRemoved(app.id);
                    })
                  }
                >
                  {t("apps.restApis.removeConfirm")}
                </Button>
                <Button size="sm" variant="ghost" onClick={() => setConfirm(false)}>{t("common.cancel")}</Button>
              </>
            ) : (
              <Button size="sm" variant="ghost" disabled={busy} onClick={() => setConfirm(true)}>
                {t("apps.restApis.remove")}
              </Button>
            )}
          </div>
        </div>
      ) : null}
    </li>
  );
}

/** "REST APIs" in the Apps dialog: connect an API from its OpenAPI document and choose which
 *  operations each bot may use. Owner-only on the daemon side. */
export function OpenApiApps() {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const toast = useToast();
  const [apps, setApps] = useState<OpenApiApp[] | null>(null);
  const [failure, setFailure] = useState<unknown>(null);

  const reload = useCallback(async () => {
    if (!client) return;
    try {
      setApps(await client.openapiApps());
      setFailure(null);
    } catch (e) {
      setFailure(e);
      setApps((a) => a ?? []);
    }
  }, [client]);

  useEffect(() => {
    void reload();
  }, [reload]);

  const replace = (next: OpenApiApp) => setApps((list) => (list ?? []).map((a) => (a.id === next.id ? next : a)));

  return (
    <section className={styles.root} aria-labelledby="apps-rest-h" data-testid="openapi-apps">
      <h3 className={styles.label} id="apps-rest-h">{t("apps.restApis.title")}</h3>
      <p className={formStyles.hint}>{t("apps.restApis.intro")}</p>
      <AddForm
        onAdded={(app) => {
          setApps((list) => [...(list ?? []), app]);
          toast.show(t("apps.restApis.addedToast", { name: app.name, count: app.operations.length }), "success");
        }}
      />
      {failure ? <ErrorLine error={failure} /> : null}
      {apps && apps.length > 0 ? (
        <>
          <h3 className={styles.label}>{t("apps.restApis.yours")}</h3>
          <ul className={styles.list}>
            {apps.map((a) => (
              <AppCard
                key={a.id}
                app={a}
                onChange={replace}
                onRemoved={(id) => setApps((list) => (list ?? []).filter((x) => x.id !== id))}
              />
            ))}
          </ul>
        </>
      ) : null}
    </section>
  );
}

