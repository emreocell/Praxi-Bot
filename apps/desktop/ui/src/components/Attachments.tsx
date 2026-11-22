import { FileText, Loader2, TriangleAlert, X } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ApiClient } from "../api/client";
import type { Attachment } from "../api/types";
import { type PendingFile, formatBytes } from "../lib/attachments";
import styles from "./Attachments.module.css";

/** The files waiting in the composer: a chip each, with a spinner, an error or a remove button. */
export function ComposerFiles({ files, onRemove }: { files: PendingFile[]; onRemove: (key: string) => void }) {
  const { t } = useTranslation();
  if (files.length === 0) {
    return null;
  }
  return (
    <ul className={styles.chips} aria-label={t("chat.attach.list")}>
      {files.map((f) => (
        <li key={f.key} className={styles.chip} data-state={f.state} title={f.errorKey ? t(f.errorKey) : f.name}>
          {f.preview ? (
            <img className={styles.thumb} src={f.preview} alt="" />
          ) : f.state === "failed" ? (
            <TriangleAlert aria-hidden="true" />
          ) : (
            <FileText aria-hidden="true" />
          )}
          <span className={styles.name}>{f.name}</span>
          <span className={styles.meta}>
            {f.state === "uploading" ? <Loader2 className={styles.spin} aria-label={t("chat.attach.uploading")} /> : null}
            {f.state === "failed" && f.errorKey ? t(f.errorKey) : f.state === "ready" ? formatBytes(f.size) : null}
          </span>
          <button
            type="button"
            className={styles.remove}
            aria-label={t("chat.attach.remove", { name: f.name })} title={t("chat.attach.remove", { name: f.name })}
            onClick={() => onRemove(f.key)}
          >
            <X aria-hidden="true" />
          </button>
        </li>
      ))}
    </ul>
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
    <img className={styles.image} src={src} alt={att.name} />
  ) : (
    <span className={styles.imageHolder} aria-label={att.name} />
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
          <span key={a.id} className={styles.file} title={a.truncated ? t("chat.attach.truncated") : a.name}>
            <FileText aria-hidden="true" />
            <span className={styles.name}>{a.name}</span>
            <span className={styles.meta}>{formatBytes(a.size)}</span>
            {a.truncated ? <span className={styles.meta}>{t("chat.attach.truncated")}</span> : null}
          </span>
        )
      )}
    </div>
  );
}
