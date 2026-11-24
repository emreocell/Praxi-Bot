import { FileText, Loader2, RotateCcw, TriangleAlert, X } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ApiClient } from "../api/client";
import type { Attachment } from "../api/types";
import { type PendingFile, formatBytes } from "../lib/attachments";
import styles from "./Attachments.module.css";

/** The files waiting in the composer: a chip each, with a spinner, an error or a remove button. */
export function ComposerFiles({ files, onRemove, onRetry, disabled }: {
  files: PendingFile[]; onRemove: (key: string) => void; onRetry: (key: string) => void; disabled: boolean;
}) {
  const { t } = useTranslation();
  if (files.length === 0) {
    return null;
  }
  return (
    <div data-no-composer-focus>
    <p className={styles.notice}>{t("chat.attach.localNotice")}</p>
    <ul className={styles.chips} tabIndex={0} aria-label={t("chat.attach.list")} aria-live="polite" aria-relevant="additions text">
      {files.map((f) => (
        <li key={f.key} className={styles.chip} data-state={f.state} title={f.errorKey ? t(f.errorKey) : f.name}>
          {f.preview ? (
            <img className={styles.thumb} src={f.preview} alt="" />
          ) : f.state === "failed" ? (
            <TriangleAlert aria-hidden="true" />
          ) : (
            <FileText aria-hidden="true" />
          )}
          <span className={styles.details}>
            <span className={styles.name}>{f.name}</span>
            <span className={styles.meta}>
              {f.state === "uploading" || f.state === "preparing" ? <Loader2 className={styles.spin} aria-hidden="true" /> : null}
              {f.state === "failed" && f.errorKey ? t(f.errorKey) : f.state === "ready" ? `${formatBytes(f.size)} · ${t(f.id ? "chat.attach.stored" : "chat.attach.local")}` : t(`chat.attach.${f.state}`)}
            </span>
            {f.resized ? <span className={styles.meta}>{t("chat.attach.resized")}</span> : null}
            {f.truncated ? <span className={styles.meta}>{t("chat.attach.truncated")}</span> : null}
            {f.kind ? <span className={styles.meta}>{t(f.kind === "image" ? "chat.attach.providerImage" : "chat.attach.providerText")}</span> : null}
          </span>
          {f.state === "failed" && f.blob ? <button type="button" className={styles.remove} disabled={disabled}
            aria-label={t("chat.attach.retry", { name: f.name })} title={t("chat.attach.retry", { name: f.name })}
            onClick={() => onRetry(f.key)}><RotateCcw aria-hidden="true" /></button> : null}
          <button
            type="button"
            className={styles.remove}
            disabled={disabled}
            aria-label={t("chat.attach.remove", { name: f.name })} title={t("chat.attach.remove", { name: f.name })}
            onClick={() => onRemove(f.key)}
          >
            <X aria-hidden="true" />
          </button>
        </li>
      ))}
    </ul>
    <details className={styles.notice}>
      <summary>{t("chat.attach.details")}</summary>
      <p>{t("chat.attach.delivery")}</p>
      <p>{t("chat.attach.documentHelp")}</p>
      <p>{t("chat.attach.limits")}</p>
      <p>{t("chat.attach.footer")}</p>
    </details>
    </div>
  );
}

/** One image attachment: fetched with the bearer token, shown from an object URL. */
function MessageImage({ client, chatId, att }: { client: ApiClient | null; chatId: string; att: Attachment }) {
  const [src, setSrc] = useState<string | null>(null);
  useEffect(() => {
    if (!client) {
      return;
    }
    let url: string | null = null;
    let live = true;
    client
      .attachmentBlob(chatId, att.id)
      .then((blob) => {
        if (live) {
          url = URL.createObjectURL(blob);
          setSrc(url);
        }
      })
      .catch(() => undefined);
    return () => {
      live = false;
      if (url) {
        URL.revokeObjectURL(url);
      }
    };
  }, [client, chatId, att.id]);
  return src ? (
    <img className={styles.image} src={src} alt={att.name} data-attachment-id={att.id} />
  ) : (
    <span className={styles.imageHolder} aria-label={att.name} data-attachment-id={att.id} />
  );
}

/** Attachments of a sent message: images as pictures, other files as a name and size. */
export function MessageFiles({
  client,
  chatId,
  attachments
}: {
  client: ApiClient | null;
  chatId: string;
  attachments: Attachment[];
}) {
  const { t } = useTranslation();
  if (attachments.length === 0) {
    return null;
  }
  return (
    <div className={styles.sent}>
      {attachments.map((a) =>
        a.kind === "image" ? (
          <MessageImage key={a.id} client={client} chatId={chatId} att={a} />
        ) : (
          <span key={a.id} className={styles.file} title={a.truncated ? t("chat.attach.truncated") : a.name} data-attachment-id={a.id}>
            <FileText aria-hidden="true" />
            <span className={styles.name}>{a.name}</span>
            <span className={styles.meta}>{formatBytes(a.size)}</span>
            <span className={styles.meta}>{t("chat.attach.untrusted")}</span>
            {a.truncated ? <span className={styles.meta}>{t("chat.attach.truncated")}</span> : null}
          </span>
        )
      )}
    </div>
  );
}
