import { Copy, Download } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import {
  clipPreview,
  DEFAULT_EXPORT_OPTIONS,
  saveChatExport,
  type ExportFormat,
  type ExportOptions
} from "../lib/chatHistory";
import { humanLine } from "../lib/humanError";
import { useDaemon } from "../state/DaemonContext";
import { DialogBody, DialogFooter, DialogHeader } from "./DialogParts";
import { Modal } from "./Modal";
import { useToast } from "./Toast";
import dialog from "./Dialog.module.css";
import styles from "./ChatExportModal.module.css";

const FORMATS: ExportFormat[] = ["markdown", "html", "json"];

/**
 * Export one chat as a file. The preview is the daemon's own redacted Markdown, so what you see
 * is what gets written: secrets, private messages and profile folders are already removed.
 */
export function ChatExportModal({ chatId, title, onClose }: { chatId: string | null; title: string; onClose: () => void }) {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const toast = useToast();
  const [format, setFormat] = useState<ExportFormat>("markdown");
  const [options, setOptions] = useState<ExportOptions>(DEFAULT_EXPORT_OPTIONS);
  const [markdown, setMarkdown] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!chatId || !client) return undefined;
    let cancelled = false;
    setMarkdown(null);
    setError(null);
    client.exportChat(chatId, "markdown", options).then(
      (blob) => blob.text().then((text) => { if (!cancelled) setMarkdown(text); }),
      (e: unknown) => { if (!cancelled) setError(humanLine(e)); }
    );
    return () => { cancelled = true; };
  }, [chatId, client, options]);

  useEffect(() => {
    if (chatId) {
      setFormat("markdown");
      setOptions(DEFAULT_EXPORT_OPTIONS);
    }
  }, [chatId]);

  const save = async () => {
    if (!client || !chatId || busy) return;
    setBusy(true);
    try {
      await saveChatExport(client, chatId, format, options);
      toast.show(t("chat.export.done"), "success");
      onClose();
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setBusy(false);
    }
  };

  const copy = async () => {
    if (markdown === null) return;
    // The text is the redacted export itself: copy it as is.
    const ok = await navigator.clipboard.writeText(markdown).then(() => true, () => false);
    toast.show(ok ? t("chatExport.copied") : t("chatExport.copyFailed"), ok ? "success" : "danger");
  };

  const preview = markdown === null ? null : clipPreview(markdown);
  return (
    <Modal open={chatId !== null} onClose={onClose} size="md" label={t("chatExport.title")}>
      {chatId ? (
        <div className={dialog.root} data-testid="chat-export-modal">
          <DialogHeader title={t("chatExport.title")} subtitle={title} onClose={onClose} />
          <DialogBody>
            <fieldset className={styles.group}>
              <legend className={styles.legend}>{t("chatExport.format")}</legend>
              {FORMATS.map((f) => (
                <label key={f} className={styles.choice}>
                  <input type="radio" name="chat-export-format" checked={format === f} onChange={() => setFormat(f)} />
                  <span>{t(`chatExport.formats.${f}`)}</span>
                </label>
              ))}
            </fieldset>
            <fieldset className={styles.group}>
              <legend className={styles.legend}>{t("chatExport.include")}</legend>
              <label className={styles.choice}>
                <input type="checkbox" checked disabled />
                <span>{t("chatExport.messages")}</span>
              </label>
              <label className={styles.choice}>
                <input type="checkbox" data-testid="chat-export-steps" checked={options.steps} onChange={(e) => setOptions((o) => ({ ...o, steps: e.target.checked }))} />
                <span>{t("chatExport.steps")}</span>
              </label>
              <label className={styles.choice}>
                <input type="checkbox" data-testid="chat-export-approvals" checked={options.approvals} onChange={(e) => setOptions((o) => ({ ...o, approvals: e.target.checked }))} />
                <span>{t("chatExport.approvals")}</span>
              </label>
            </fieldset>
            <p className={styles.note}>{t("chatExport.privacy")}</p>
            {error ? <p className={styles.error} role="alert">{error}</p> : null}
            {preview ? (
              <>
                <pre className={styles.preview} data-testid="chat-export-preview" tabIndex={0} aria-label={t("chatExport.previewLabel")}>{preview.text}</pre>
                {preview.clipped ? <p className={styles.note}>{t("chatExport.previewClipped")}</p> : null}
              </>
            ) : error ? null : (
              <p className={styles.note}>{t("chatExport.loading")}</p>
            )}
          </DialogBody>
          <DialogFooter>
            <Button variant="ghost" disabled={markdown === null} onClick={() => void copy()}>
              <Copy aria-hidden="true" />{t("chatExport.copy")}
            </Button>
            <Button disabled={busy || markdown === null} onClick={() => void save()} data-testid="chat-export-save">
              <Download aria-hidden="true" />{t("chatExport.save")}
            </Button>
          </DialogFooter>
        </div>
      ) : null}
    </Modal>
  );
}
