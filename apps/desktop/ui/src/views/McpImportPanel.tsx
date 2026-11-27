import { useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { ApiClient, ApiError } from "../api/client";
import type { McpBundleInspection, McpRegistryResult, McpRegistrySource, McpServer } from "../api/types";
import form from "../components/Form.module.css";
import apps from "./AppsModal.module.css";
import styles from "./McpImportPanel.module.css";

type Mode = "registry" | "bundle";
type Approval =
  | { kind: "bundle"; file: File; preview: McpBundleInspection }
  | { kind: "registry"; entry: McpRegistryResult; source: McpRegistrySource };

const MIB = 1024 * 1024;

function importError(error: unknown, fallback: "searchFailed" | "inspectFailed" | "installFailed") {
  if (error instanceof ApiError) {
    if (error.status === 403) return "apps.imports.ownerRequired";
    if (error.status === 413) return "apps.imports.fileTooLarge";
    if (error.code === "version_conflict") return "apps.imports.registryChanged";
    if (error.code === "busy") return "apps.imports.bundleBusy";
    if (error.code === "conflict") return "apps.imports.alreadyInstalled";
  }
  return `apps.imports.${fallback}` as const;
}

function Verification({ entry }: { entry: McpRegistryResult }) {
  const { t } = useTranslation();
  return (
    <p className={form.hint}>
      {t("apps.imports.publisher", { name: entry.publisher })}{" · "}
      {t(entry.namespace_verified ? "apps.imports.namespaceVerified" : "apps.imports.namespaceUnknown")}
    </p>
  );
}

function ApprovalCard({ approval, busy, onCancel, onInstall }: {
  approval: Approval;
  busy: boolean;
  onCancel: () => void;
  onInstall: (values: Record<string, string>) => void;
}) {
  const { t } = useTranslation();
  const heading = useRef<HTMLHeadingElement>(null);
  const headingId = useId();
  useEffect(() => { heading.current?.focus(); }, []);

  const bundle = approval.kind === "bundle" ? approval.preview : null;
  const launch = approval.kind === "bundle" ? approval.preview : approval.source;
  const remoteUrl = approval.kind === "registry" ? approval.source.url : null;
  const fields = approval.kind === "bundle" ? approval.preview.fields : approval.source.fields.map((field) => ({
    key: field.name,
    title: field.name,
    description: field.description,
    kind: "string",
    default: null,
    multiple: false,
    required: field.required,
    sensitive: field.secret
  }));
  const accessNames = bundle?.env_names ?? fields.map((field) => field.key);

  return (
    <form
      className={styles.card}
      aria-labelledby={headingId}
      onSubmit={(event) => {
        event.preventDefault();
        if (!busy) onInstall(Object.fromEntries(new FormData(event.currentTarget)) as Record<string, string>);
      }}
    >
      <h4 id={headingId} ref={heading} tabIndex={-1} className={styles.title}>{t("apps.imports.review")}</h4>
      <strong>{approval.kind === "bundle" ? approval.preview.display_name : approval.entry.name}</strong>
      <p className={form.hint}>{approval.kind === "bundle" ? approval.preview.description : approval.entry.description}</p>
      {approval.kind === "registry" ? <Verification entry={approval.entry} /> : null}
      <dl className={styles.details}>
        <dt>{t("apps.imports.version")}</dt>
        <dd>{approval.kind === "bundle" ? approval.preview.version : approval.entry.version}</dd>
        {bundle?.author ? <><dt>{t("apps.imports.author")}</dt><dd>{bundle.author}</dd></> : null}
        {bundle ? <><dt>{t("apps.imports.runtime")}</dt><dd>{t(`apps.imports.runtimes.${bundle.runtime}`)}</dd></> : null}
        <dt>{t(remoteUrl ? "apps.imports.endpoint" : "apps.imports.command")}</dt>
        <dd><code className={styles.literal} dir="ltr">{remoteUrl ?? launch.command}</code></dd>
        {launch.args.length > 0 ? (
          <>
            <dt>{t("apps.imports.arguments")}</dt>
            <dd>
              <ol className={styles.arguments}>
                {launch.args.map((arg, index) => <li key={index}><code className={styles.literal} dir="ltr">{JSON.stringify(arg)}</code></li>)}
              </ol>
            </dd>
          </>
        ) : null}
        {accessNames.length > 0 ? (
          <>
            <dt>{t(remoteUrl ? "apps.imports.headers" : "apps.imports.environment")}</dt>
            <dd><code>{accessNames.join(", ")}</code></dd>
          </>
        ) : null}
        {bundle ? (
          <>
            <dt>{t("apps.imports.sha256")}</dt><dd><code>{bundle.sha256}</code></dd>
            <dt>{t("apps.imports.signature")}</dt>
            <dd>{t(bundle.signature_verified ? "apps.imports.signatureChecked" : "apps.imports.unsigned")}</dd>
            {bundle.checksum_verified ? <><dt>{t("apps.imports.checksum")}</dt><dd>{t("apps.imports.checksumMatched")}</dd></> : null}
            {bundle.tools.length > 0 ? <><dt>{t("apps.imports.declaredTools")}</dt><dd>{bundle.tools.join(", ")}</dd></> : null}
          </>
        ) : null}
      </dl>
      {bundle ? <p className={form.hint}>{t("apps.imports.bundleFolder")}</p> : null}
      <p className={form.hint}>{t(remoteUrl ? "apps.imports.remoteAccess" : "apps.imports.localAccess")}</p>
      {bundle?.warnings.map((warning) => <p key={warning} className={form.hint}>{t(`apps.imports.warnings.${warning}`)}</p>)}
      {fields.map((field) => (
        <label key={field.key} className={form.field}>
          <span className={form.label}>{field.title}</span>
          {field.multiple ? (
            <textarea className={form.textarea} name={field.key} required={field.required}
              defaultValue={field.default ?? ""} maxLength={4096} spellCheck={false} />
          ) : field.kind === "boolean" && !field.sensitive ? (
            <select className={form.select} name={field.key} required={field.required} defaultValue={field.default ?? ""}>
              <option value="">{t("apps.imports.unspecified")}</option>
              <option value="true">{t("apps.imports.yes")}</option>
              <option value="false">{t("apps.imports.no")}</option>
            </select>
          ) : (
            <input
              className={form.input}
              name={field.key}
              type={field.sensitive ? "password" : field.kind === "number" ? "number" : "text"}
              step={field.kind === "number" ? "any" : undefined}
              autoComplete="off"
              spellCheck={false}
              maxLength={4096}
              required={field.required}
              defaultValue={field.default ?? ""}
            />
          )}
          {field.description ? <span className={form.hint}>{field.description}</span> : null}
          {field.kind === "directory" || field.kind === "file" ? (
            <span className={form.hint}>{t(field.multiple ? "apps.imports.multiplePaths" : "apps.imports.absolutePath")}</span>
          ) : null}
        </label>
      ))}
      <div className={styles.actions}>
        <Button type="submit" size="sm" variant="primary" disabled={busy}>{t("apps.imports.installDisabled")}</Button>
        <Button type="button" size="sm" variant="ghost" disabled={busy} onClick={onCancel}>{t("common.cancel")}</Button>
      </div>
    </form>
  );
}

export function McpImportPanel({ client, onInstalled }: { client: ApiClient; onInstalled: (server: McpServer) => void }) {
  const { t } = useTranslation();
  const [mode, setMode] = useState<Mode | null>(null);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<McpRegistryResult[] | null>(null);
  const [approval, setApproval] = useState<Approval | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ReturnType<typeof importError> | "apps.imports.badChecksum" | null>(null);
  const life = useRef<AbortController>(null);
  useEffect(() => {
    const control = new AbortController();
    life.current = control;
    setApproval(null);
    setResults(null);
    setBusy(false);
    setError(null);
    return () => control.abort();
  }, [client]);

  async function run<T>(op: (signal: AbortSignal) => Promise<T>, onSuccess: (value: T) => void, fallback: "searchFailed" | "inspectFailed" | "installFailed") {
    const signal = life.current?.signal;
    if (!signal || signal.aborted || busy) return;
    setBusy(true);
    setError(null);
    try {
      const value = await op(signal);
      if (!signal.aborted) onSuccess(value);
    } catch (e) {
      if (!signal.aborted) setError(importError(e, fallback));
    } finally {
      if (!signal.aborted) setBusy(false);
    }
  }

  function choose(next: Mode) {
    setApproval(null);
    setError(null);
    setMode((current) => current === next ? null : next);
  }

  return (
    <section className={apps.section} aria-label={t("apps.imports.title")}>
      <h3 className={apps.sectionLabel}>{t("apps.imports.title")}</h3>
      <div className={styles.actions}>
        <Button type="button" size="sm" variant="secondary" aria-expanded={mode === "registry"} disabled={busy} onClick={() => choose("registry")}>
          {t("apps.imports.registry")}
        </Button>
        <Button type="button" size="sm" variant="secondary" aria-expanded={mode === "bundle"} disabled={busy} onClick={() => choose("bundle")}>
          {t("apps.imports.bundle")}
        </Button>
      </div>
      {approval ? (
        <ApprovalCard
          approval={approval}
          busy={busy}
          onCancel={() => { setApproval(null); setError(null); }}
          onInstall={(values) => void run(
            (signal) => approval.kind === "bundle"
              ? client.installMcpBundle(approval.file, approval.preview.sha256, values, signal)
              : client.installMcpRegistry({
                name: approval.entry.name, version: approval.entry.version,
                approval_sha256: approval.source.approval_sha256, values
              }, signal),
            (server) => { setApproval(null); setMode(null); onInstalled(server); },
            "installFailed"
          )}
        />
      ) : mode === "registry" ? (
        <div className={styles.card}>
          <form className={styles.actions} onSubmit={(event) => {
            event.preventDefault();
            if (!query.trim()) return;
            setResults(null);
            void run((signal) => client.searchMcpRegistry(query.trim(), signal), setResults, "searchFailed");
          }}>
            <input className={`${form.input} ${styles.search}`} type="search" value={query} maxLength={200}
              aria-label={t("apps.imports.registry")} placeholder={t("apps.imports.searchPlaceholder")}
              onChange={(e) => { setQuery(e.target.value); setResults(null); }} disabled={busy} />
            <Button type="submit" size="sm" variant="primary" disabled={busy || !query.trim()}>{t("apps.imports.search")}</Button>
          </form>
          {busy ? <p className={form.hint} role="status">{t("apps.imports.searching")}</p> : null}
          {results?.length === 0 ? <p className={form.hint} role="status">{t("apps.imports.noResults")}</p> : null}
          {results?.length ? (
            <ul className={styles.results}>
              {results.map((entry) => (
                <li key={`${entry.name}@${entry.version}`} className={styles.result}>
                  <strong>{entry.name}</strong>
                  <Verification entry={entry} />
                  <p className={form.hint}>{entry.description}</p>
                  {entry.sources.length === 0 ? (
                    <p className={form.hint}>{t(entry.active ? "apps.imports.unsupported" : "apps.imports.inactive")}</p>
                  ) : entry.sources.map((source) => (
                    <div className={styles.source} key={source.approval_sha256}>
                      <code className={styles.literal} dir="ltr">{source.url ?? [source.command, ...source.args].join(" ")}</code>
                      <Button type="button" size="sm" variant="secondary" onClick={() => {
                        setError(null);
                        setApproval({ kind: "registry", entry, source });
                      }}>{t("apps.imports.review")}</Button>
                    </div>
                  ))}
                </li>
              ))}
            </ul>
          ) : null}
        </div>
      ) : mode === "bundle" ? (
        <form className={styles.card} onSubmit={(event) => {
          event.preventDefault();
          const data = new FormData(event.currentTarget);
          const file = data.get("bundle");
          const checksum = String(data.get("checksum") ?? "").trim();
          if (!(file instanceof File) || !file.size) { setError("apps.imports.inspectFailed"); return; }
          const max = client.hostName === null ? 100 * MIB : 16 * MIB - 68 * 1024;
          if (file.size > max) { setError("apps.imports.fileTooLarge"); return; }
          if (checksum && !/^[0-9a-f]{64}$/i.test(checksum)) { setError("apps.imports.badChecksum"); return; }
          void run((signal) => client.inspectMcpBundle(file, checksum, signal),
            (preview) => setApproval({ kind: "bundle", file, preview }), "inspectFailed");
        }}>
          <label className={form.field}>
            <span className={form.label}>{t("apps.imports.bundleFile")}</span>
            <input className={form.input} type="file" name="bundle" accept=".mcpb" disabled={busy} required />
          </label>
          <label className={form.field}>
            <span className={form.label}>{t("apps.imports.optionalChecksum")}</span>
            <input className={form.input} name="checksum" autoComplete="off" spellCheck={false} maxLength={64} disabled={busy} />
          </label>
          <div className={styles.actions}>
            <Button type="submit" size="sm" variant="primary" disabled={busy}>{t("apps.imports.inspect")}</Button>
          </div>
          {busy ? <p className={form.hint} role="status">{t("apps.imports.inspecting")}</p> : null}
        </form>
      ) : null}
      {error ? <p className={form.error} role="alert">{t(error)}</p> : null}
    </section>
  );
}
