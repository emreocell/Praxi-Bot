import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { SettingsCard } from "../../components/SettingsUi";
import type { KnowledgeFileFailure, KnowledgeFolderListing, KnowledgeFolderPreview, KnowledgeSkipped, KnowledgeSource, KnowledgeWatchStatus } from "../../api/types";
import { useDaemon } from "../../state/DaemonContext";
import { estimateParts, finishIndexing, folderName, folderSources, overlappingFolder, type IndexProgress, typeCounts } from "../../lib/documents";
import { formatBytes } from "../../lib/workspaceFormat";
import { humanLine } from "../../lib/humanError";
import { formatDateTime } from "../../lib/messageTime";
import form from "../../components/Form.module.css";
import styles from "./DocumentsCard.module.css";

type Step = { name: "idle" } | { name: "pick"; listing: KnowledgeFolderListing; typed: string } | { name: "review"; path: string; preview: KnowledgeFolderPreview; label: string };
type Reading = { label: string; total: number; indexed: number; failures: KnowledgeFileFailure[] };

/**
 * "My documents": choose folders a bot may read, see what will be read before anything is, and
 * forget a folder completely. Everything is per folder and per bot; the heavy lifting (indexing,
 * citations, search) is the knowledge system, this is the plain-language front door to it.
 */
export function DocumentsCard() {
  const { t, i18n } = useTranslation();
  const { client, bots } = useDaemon();
  const [bot, setBot] = useState("");
  const activeBot = bot || bots[0]?.id || "";
  const [sources, setSources] = useState<KnowledgeSource[]>([]);
  const [watch, setWatch] = useState<Record<string, KnowledgeWatchStatus>>({});
  const [step, setStep] = useState<Step>({ name: "idle" });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState("");
  const [reading, setReading] = useState<Reading | null>(null);
  const [details, setDetails] = useState<Record<string, { preview?: KnowledgeFolderPreview; failed?: boolean }>>({});
  const [removing, setRemoving] = useState<KnowledgeSource | null>(null);
  const epoch = useRef(0);
  const alive = useRef(true);
  // Set again on every mount: React's strict mode runs the cleanup once before the real mount.
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);

  const reload = useCallback(async () => {
    if (!client || !activeBot) return;
    const current = ++epoch.current;
    const [next, watching] = await Promise.all([client.listKnowledgeSources(activeBot), client.listKnowledgeWatch(activeBot)]);
    if (current !== epoch.current) return;
    setSources(folderSources(next));
    setWatch(Object.fromEntries(watching.map((item) => [item.source_id, item])));
  }, [client, activeBot]);

  useEffect(() => {
    setSources([]); setWatch({}); setStep({ name: "idle" }); setError(null); setNotice(""); setReading(null); setDetails({}); setRemoving(null);
    void reload().catch((e: unknown) => setError(humanLine(e)));
    return () => { epoch.current += 1; };
  }, [reload]);

  // Keep the counts and update state current while this page is open.
  useEffect(() => {
    if (busy) return;
    const timer = window.setInterval(() => { void reload().catch(() => undefined); }, 10_000);
    return () => window.clearInterval(timer);
  }, [busy, reload]);

  const act = async (operation: () => Promise<void>) => {
    setBusy(true); setError(null); setNotice("");
    try { await operation(); await reload(); }
    catch (e) { setError(humanLine(e)); await reload().catch(() => undefined); }
    finally { setBusy(false); }
  };

  const browse = (path?: string) => act(async () => {
    if (!client) return;
    const listing = await client.browseKnowledgeFolders(path);
    setStep({ name: "pick", listing, typed: listing.path });
  });

  const review = (path: string) => act(async () => {
    if (!client) return;
    const overlap = overlappingFolder(sources, path);
    if (overlap) { setError(t("documents.alreadyAdded", { label: overlap.label })); return; }
    const preview = await client.previewKnowledgeFolder(activeBot, path);
    setStep({ name: "review", path, preview, label: folderName(path) });
  });

  const allow = (path: string, label: string, preview: KnowledgeFolderPreview) => act(async () => {
    if (!client) return;
    const botId = activeBot;
    setReading({ label, total: preview.file_count, indexed: 0, failures: [] });
    try {
      const first = await client.createKnowledgeSource(botId, { kind: "folder", label: label.trim() || folderName(path), path });
      setStep({ name: "idle" });
      const done = await finishIndexing(
        client, botId, first,
        (progress: IndexProgress) => { if (alive.current) setReading({ label, total: preview.file_count, indexed: progress.indexed, failures: progress.failures }); },
        () => !alive.current || botId !== activeBot
      );
      if (alive.current) setNotice(t("documents.readingDone", { count: done.indexed }));
    } finally {
      if (alive.current) setReading((current) => (current && current.failures.length ? current : null));
    }
  });

  const update = (source: KnowledgeSource) => act(async () => {
    if (!client) return;
    const first = await client.reindexKnowledgeSource(activeBot, source.id, source.revision);
    const done = await finishIndexing(client, activeBot, first, () => undefined, () => !alive.current);
    setNotice(t("documents.readingDone", { count: done.indexed }));
  });

  const loadDetails = (source: KnowledgeSource) => act(async () => {
    if (!client) return;
    try {
      setDetails((previous) => ({ ...previous, [source.id]: { preview: undefined } }));
      const preview = await client.previewKnowledgeFolder(activeBot, source.locator);
      setDetails((previous) => ({ ...previous, [source.id]: { preview } }));
    } catch {
      setDetails((previous) => ({ ...previous, [source.id]: { failed: true } }));
    }
  });

  const forget = (source: KnowledgeSource) => act(async () => {
    await client!.deleteKnowledgeSource(activeBot, source.id, source.revision);
    setRemoving(null);
    setDetails((previous) => { const next = { ...previous }; delete next[source.id]; return next; });
    setNotice(t("documents.forgotten", { folder: source.label }));
  });

  const botName = bots.find((item) => item.id === activeBot)?.name ?? "";

  return <SettingsCard title={t("documents.title")} description={t("documents.description")} testId="documents-card">
    <div className={styles.root}>
      <p className={form.hint}>{t("documents.privacy")}</p>
      {bots.length > 1 ? <label className={form.field}>{t("documents.bot")}
        <select className={form.select} value={activeBot} disabled={busy} onChange={(e) => setBot(e.target.value)}>
          {bots.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}
        </select>
      </label> : null}
      {!activeBot ? <p className={styles.text}>{t("documents.noBot")}</p> : <>
        {error ? <p role="alert" className={form.error}>{error}</p> : null}
        {notice ? <p role="status" className={form.success}>{notice}</p> : null}
        {reading ? <ReadingStatus reading={reading} /> : null}
        <ul className={styles.list}>
          {sources.map((source) => <li key={source.id} className={styles.folder}>
            <strong>{source.label}</strong>
            <code className={styles.path}>{source.locator}</code>
            <span className={form.hint}>{t("documents.filesReady", { count: source.documents })}
              {watch[source.id] ? <> · {t(`knowledge.watch.state.${watch[source.id].state}`)}{watch[source.id].last_sync_ms ? <> · {t("knowledge.watch.lastSync", { time: formatDateTime(watch[source.id].last_sync_ms!, i18n.language) })}</> : null}</> : null}
            </span>
            {watch[source.id]?.error ? <p role="alert" className={form.error}>{t(`knowledge.watch.errors.${watch[source.id].error!.code}`, { defaultValue: t("knowledge.watch.errors.sync_failed") })}</p> : null}
            {details[source.id] ? <FolderDetails preview={details[source.id].preview} failed={details[source.id].failed} /> : null}
            <div className={styles.actions}>
              <Button size="sm" variant="secondary" disabled={busy || !source.enabled} onClick={() => void update(source)}>{t("documents.reindex")}</Button>
              {watch[source.id] ? <Button size="sm" variant="secondary" disabled={busy} onClick={() => void act(async () => { await client?.setKnowledgeWatchPaused(activeBot, source.id, watch[source.id].state !== "paused"); })}>{t(watch[source.id].state === "paused" ? "knowledge.watch.resume" : "knowledge.watch.pause")}</Button> : null}
              <Button size="sm" variant="secondary" disabled={busy} onClick={() => void loadDetails(source)}>{t("documents.details")}</Button>
              <Button size="sm" variant="secondary" disabled={busy} onClick={() => setRemoving(source)}>{t("documents.forget")}</Button>
            </div>
          </li>)}
        </ul>
        {sources.length === 0 && !reading && step.name === "idle" ? <p className={form.hint}>{t("documents.empty")}</p> : null}
        {sources.length > 0 || reading ? <p className={form.hint}>{t("documents.howToAsk")}</p> : null}
        {removing ? <section className={styles.panel} aria-label={t("documents.forget")}>
          <strong>{t("documents.forgetTitle", { folder: removing.label })}</strong>
          <p className={styles.text}>{t("documents.forgetHint")}</p>
          <div className={styles.actions}>
            <Button disabled={busy} onClick={() => void forget(removing)}>{t("documents.forgetConfirm")}</Button>
            <Button variant="secondary" disabled={busy} onClick={() => setRemoving(null)}>{t("common.cancel")}</Button>
          </div>
        </section> : null}
        {step.name === "idle" ? <div className={styles.actions}>
          <Button disabled={busy || Boolean(reading)} onClick={() => void browse()}>{t("documents.add")}</Button>
        </div> : null}
        {step.name === "pick" ? <FolderPicker listing={step.listing} typed={step.typed} busy={busy}
          onTyped={(typed) => setStep({ ...step, typed })} onBrowse={(path) => void browse(path)}
          onChoose={(path) => void review(path)} onCancel={() => setStep({ name: "idle" })} /> : null}
        {step.name === "review" ? <section className={styles.panel} aria-label={t("documents.previewTitle", { folder: folderName(step.path) })}>
          <FolderPreviewBody preview={step.preview} title={t("documents.previewTitle", { folder: folderName(step.path) })} />
          {step.preview.file_count > 0 ? <>
            <label className={form.field}>{t("documents.name")}
              <input className={form.input} value={step.label} maxLength={200} onChange={(e) => setStep({ ...step, label: e.target.value })} />
            </label>
            <p className={styles.text}>{t("documents.consent", { bot: botName })}</p>
            <p className={form.hint}>{t("documents.consentHint")}</p>
          </> : null}
          <div className={styles.actions}>
            {step.preview.file_count > 0 ? <Button disabled={busy || !step.label.trim()} onClick={() => void allow(step.path, step.label, step.preview)}>{t("documents.consentButton", { bot: botName })}</Button> : null}
            <Button variant="secondary" disabled={busy} onClick={() => void browse(step.path)}>{t("common.back")}</Button>
            <Button variant="secondary" disabled={busy} onClick={() => setStep({ name: "idle" })}>{t("common.cancel")}</Button>
          </div>
        </section> : null}
      </>}
    </div>
  </SettingsCard>;
}

function ReadingStatus({ reading }: { reading: Reading }) {
  const { t } = useTranslation();
  const label = t("documents.reading", { done: reading.indexed, total: reading.total });
  return <div role="status" className={styles.root}>
    <span className={styles.text}>{reading.label}: {label}</span>
    <progress className={styles.progress} aria-label={label} max={Math.max(reading.total, 1)} value={Math.min(reading.indexed, Math.max(reading.total, 1))} />
    {reading.failures.length ? <span className={form.hint}>{t("documents.readingFailed", { count: reading.failures.length })}</span> : null}
    <span className={form.hint}>{t("documents.readingBackground")}</span>
  </div>;
}

function FolderPicker({ listing, typed, busy, onTyped, onBrowse, onChoose, onCancel }: {
  listing: KnowledgeFolderListing; typed: string; busy: boolean;
  onTyped: (value: string) => void; onBrowse: (path: string) => void; onChoose: (path: string) => void; onCancel: () => void;
}) {
  const { t } = useTranslation();
  return <section className={styles.panel} aria-label={t("documents.pickTitle")}>
    <strong>{t("documents.pickTitle")}</strong>
    <p className={form.hint}>{t("documents.pickHint")}</p>
    <code className={styles.path}>{listing.path}</code>
    {listing.roots.length ? <div className={styles.actions}>{listing.roots.map((root) => <Button key={root} size="sm" variant="secondary" disabled={busy} onClick={() => onBrowse(root)}>{root}</Button>)}</div> : null}
    <div className={styles.picker}>
      {listing.parent ? <button type="button" className={styles.entry} disabled={busy} onClick={() => onBrowse(listing.parent!)}>↑ {t("documents.pickUp")}</button> : null}
      {listing.entries.map((entry) => <button key={entry.path} type="button" className={styles.entry} disabled={busy || !entry.selectable} title={entry.selectable ? undefined : t("documents.pickBlocked")} onClick={() => onBrowse(entry.path)}>{entry.name}</button>)}
      {listing.entries.length === 0 ? <p className={form.hint}>{t("documents.pickEmpty")}</p> : null}
    </div>
    {listing.truncated ? <p className={form.hint}>{t("documents.pickTruncated")}</p> : null}
    <form className={styles.panel} onSubmit={(e) => { e.preventDefault(); if (typed.trim()) onBrowse(typed.trim()); }}>
      <label className={form.field}>{t("documents.pickPath")}
        <input className={form.input} value={typed} onChange={(e) => onTyped(e.target.value)} />
        <span className={form.hint}>{t("documents.pickPathHint")}</span>
      </label>
      <div className={styles.actions}>
        <Button type="submit" variant="secondary" disabled={busy || !typed.trim() || typed.trim() === listing.path}>{t("documents.pickGo")}</Button>
      </div>
    </form>
    <div className={styles.actions}>
      <Button disabled={busy || !listing.selectable} onClick={() => onChoose(listing.path)}>{t("documents.pickChoose")}</Button>
      <Button variant="secondary" disabled={busy} onClick={onCancel}>{t("common.cancel")}</Button>
    </div>
    {!listing.selectable ? <p className={form.hint}>{t("documents.pickBlocked")}</p> : null}
  </section>;
}

/** What will be read, how big, how long, and what is left out (secrets first, by name). */
function FolderPreviewBody({ preview, title }: { preview: KnowledgeFolderPreview; title?: string }) {
  const { t } = useTranslation();
  const [showFiles, setShowFiles] = useState(false);
  const estimate = estimateParts(preview.estimated_seconds);
  return <div className={styles.preview}>
    {title ? <strong>{title}</strong> : null}
    {preview.file_count === 0 ? <p className={styles.text}>{t("documents.previewNone")}</p> : <>
      <p className={styles.text}>{t("documents.previewSummary", { count: preview.file_count, size: formatBytes(preview.total_bytes) })}</p>
      <p className={form.hint}>{typeCounts(preview.by_type).map(({ type, count }) => t("documents.typeCount", { type: t(`documents.types.${type}`), count })).join(" · ")}</p>
      <p className={form.hint}>{estimate.unit === "lessThanMinute" ? t("documents.estimate.lessThanMinute") : t(`documents.estimate.${estimate.unit}`, { count: estimate.value })} {t("documents.estimateNote")}</p>
      <div className={styles.actions}><Button size="sm" variant="secondary" aria-expanded={showFiles} onClick={() => setShowFiles(!showFiles)}>{t("documents.showFiles")}</Button></div>
      {showFiles ? <ul className={styles.fileList}>
        {preview.files.map((file) => <li key={file.locator}><span>{file.locator}</span><span>{formatBytes(file.bytes)}</span></li>)}
        {preview.file_count > preview.files.length ? <li>{t("documents.moreFiles", { count: preview.file_count - preview.files.length })}</li> : null}
      </ul> : null}
    </>}
    <SkippedNote skipped={preview.skipped} />
  </div>;
}

/** The visible note about files left out on purpose. */
function SkippedNote({ skipped }: { skipped: KnowledgeSkipped }) {
  const { t } = useTranslation();
  const lines: [string, number][] = [["secret", skipped.secret], ["tooLarge", skipped.too_large], ["hidden", skipped.hidden], ["unsupported", skipped.unsupported]];
  if (lines.every(([, count]) => count === 0)) return null;
  const names = skipped.listed.filter((file) => file.reason === "secret").map((file) => file.locator);
  return <div>
    <strong className={styles.text}>{t("documents.skipped.title")}</strong>
    <ul className={styles.skipped}>
      {lines.filter(([, count]) => count > 0).map(([key, count]) => <li key={key}>{t(`documents.skipped.${key}`, { count })}
        {key === "secret" ? <><br />{t("documents.skipped.secretNote")}<br /><code className={styles.path}>{names.join(", ")}{skipped.secret > names.length ? " …" : ""}</code></> : null}
      </li>)}
    </ul>
  </div>;
}

function FolderDetails({ preview, failed }: { preview?: KnowledgeFolderPreview; failed?: boolean }) {
  const { t } = useTranslation();
  if (failed) return <p role="alert" className={form.error}>{t("documents.detailsFailed")}</p>;
  if (!preview) return <p role="status" className={form.hint}>{t("documents.detailsLoading")}</p>;
  return <FolderPreviewBody preview={preview} />;
}
