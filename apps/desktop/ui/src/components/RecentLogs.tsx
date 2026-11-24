import { getVersion } from "@tauri-apps/api/app";
import { ClipboardCopy, Download, FileSearch, RefreshCw } from "lucide-react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { humanLine } from "../lib/humanError";
import { filterLogs, supportFiles, supportZip, type SupportFile } from "../lib/supportBundle";
import { isTauriRuntime } from "../overlay/events";
import { useDaemon } from "../state/DaemonContext";
import { Modal } from "./Modal";
import { useToast } from "./Toast";
import styles from "./RecentLogs.module.css";

/** Fetch on an owner click; Copy and Save operate only on visible snapshots. */
export function RecentLogs() {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const toast = useToast();
  const [open, setOpen] = useState(false);
  const [lines, setLines] = useState<string[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState(false);
  const lock = useRef(false);
  const generation = useRef(0);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [level, setLevel] = useState("");
  const [files, setFiles] = useState<SupportFile[] | null>(null);
  const visible = filterLogs(lines, search, level);
  const close = () => { generation.current++; setOpen(false); setFiles(null); setLines([]); setLoaded(false); };

  const load = async (bundle = false) => {
    if (!client || lock.current) return;
    lock.current = true;
    const current = generation.current;
    setBusy(true);
    setError(null);
    setFiles(null);
    try {
      const logs = await client.recentLogs();
      const preview = bundle ? supportFiles(
        await client.health(),
        isTauriRuntime() ? await getVersion() : "unknown",
        await client.doctorReport(), logs.lines
      ) : null;
      if (current !== generation.current) return;
      setLines(logs.lines);
      setLoaded(true);
      setFiles(preview);
    } catch (e) {
      if (current === generation.current) setError(`${t("logs.failed")} (${humanLine(e)})`);
    } finally { lock.current = false; setBusy(false); }
  };

  const copy = async () => {
    try { await navigator.clipboard.writeText(visible.join("\n")); toast.show(t("logs.copied"), "success"); }
    catch { toast.show(t("logs.copyFailed"), "danger"); }
  };
  const save = () => {
    if (!files) return;
    try {
      const url = URL.createObjectURL(new Blob([supportZip(files)], { type: "application/zip" }));
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = "praxi-support.zip";
      anchor.click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch { toast.show(t("logs.saveFailed"), "danger"); }
  };
  return <>
    <Button variant="secondary" size="sm" disabled={!client} onClick={() => { setOpen(true); void load(); }}>
      <FileSearch aria-hidden="true" />{t("logs.title")}
    </Button>
    {!client ? <p>{t("help.center.diag.offline")}</p> : null}
    <Modal open={open} onClose={close} label={t("logs.title")} size="lg">
      <div className={styles.root}>
        <h2>{t("logs.title")}</h2>
        <p>{t("logs.privacy")}</p>
        <div className={styles.controls}>
          <label>{t("logs.search")}<input type="search" value={search} onChange={(e) => setSearch(e.target.value)} /></label>
          <label>{t("logs.level")}<select value={level} onChange={(e) => setLevel(e.target.value)}>
            {["", "error", "warn", "info", "debug", "trace"].map((l) => <option key={l} value={l}>{t(`logs.levels.${l || "all"}`)}</option>)}
          </select></label>
        </div>
        {busy ? <p role="status">{t("logs.loading")}</p> : null}
        {error ? <p role="alert">{error}</p> : null}
        {loaded ? <pre className={styles.preview} tabIndex={0} aria-label={t("logs.title")}>{visible.join("\n") || t("logs.empty")}</pre> : null}
        <div className={styles.actions}>
          <Button variant="secondary" size="sm" disabled={busy || !client} onClick={() => void load()}><RefreshCw aria-hidden="true" />{t("logs.refresh")}</Button>
          <Button variant="secondary" size="sm" disabled={busy || visible.length === 0} onClick={() => void copy()}><ClipboardCopy aria-hidden="true" />{t("logs.copy")}</Button>
          <Button variant="secondary" size="sm" disabled={busy || !client} onClick={() => void load(true)}>{t("logs.prepare")}</Button>
        </div>
        {files ? <section aria-label={t("logs.bundlePreview")}>
          <h3>{t("logs.bundlePreview")}</h3>
          <p>{t("logs.bundleNote")}</p>
          {files.map((file) => <details key={file.name}><summary>{file.name} ({new TextEncoder().encode(file.text).length} B)</summary><pre className={styles.preview} tabIndex={0}>{file.text}</pre></details>)}
        </section> : null}
        <div className={styles.actions}>
          <Button variant="primary" size="sm" disabled={!files || busy} onClick={save}><Download aria-hidden="true" />{t("logs.save")}</Button>
          <Button variant="ghost" size="sm" onClick={close}>{t("common.close")}</Button>
        </div>
      </div>
    </Modal>
  </>;
}
