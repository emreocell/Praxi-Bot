/** Files attached in the composer: limits (mirroring the daemon) and drag/paste helpers. */

/** Most files on one message (the daemon refuses more). */
export const MAX_ATTACHMENTS = 6;
/** Largest file the daemon accepts, in bytes. */
export const MAX_ATTACHMENT_BYTES = 15 * 1024 * 1024;
export const MAX_IMAGE_BYTES = 5 * 1024 * 1024;
export const MAX_TEXT_BYTES = 1024 * 1024;
export const MAX_IMAGE_EDGE = 2048;
export const ATTACHMENT_ACCEPT = ".png,.jpg,.jpeg,.gif,.webp,.pdf,.docx,.xlsx,.pptx,.txt,.md,.csv,.json,.xml,.yaml,.yml,.log,image/png,image/jpeg,image/gif,image/webp,application/pdf,text/*";

/** A file in the composer: uploading, ready (with the daemon's id) or failed. */
export interface PendingFile {
  key: string;
  name: string;
  size: number;
  state: "preparing" | "uploading" | "ready" | "failed";
  /** Prepared bytes stay in browser memory until Send. */
  blob?: Blob;
  resized?: boolean;
  truncated?: boolean;
  /** Daemon attachment id once `ready`. */
  id?: string;
  kind?: string;
  /** Local preview of an image (object URL), revoked when the file is removed. */
  preview?: string;
  /** i18n key of why it failed. */
  errorKey?: string;
}

/** "1.4 MB", "12 KB", "300 B". */
export function formatBytes(n: number): string {
  if (n < 1024) {
    return `${n} B`;
  }
  if (n < 1024 * 1024) {
    return `${Math.round(n / 1024)} KB`;
  }
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

/** The files of a drag-drop or paste (folders and text drags carry none). */
export function filesOf(data: { files?: ArrayLike<File> | null } | null | undefined): File[] {
  return data?.files ? Array.from(data.files) : [];
}

/** How many of `incoming` still fit next to `current` files; the rest is dropped. */
export function room(current: number, incoming: number): number {
  return Math.max(0, Math.min(incoming, MAX_ATTACHMENTS - current));
}

/** i18n key for an upload refusal, by the daemon's error code (HTTP status as a fallback). */
export function uploadErrorKey(code: string | undefined, status: number | undefined): string {
  if (code === "attachment_too_large" || status === 413) {
    return "chat.attach.tooLarge";
  }
  if (code === "unsupported_attachment" || status === 415) {
    return "chat.attach.unsupported";
  }
  return "chat.attach.failed";
}
