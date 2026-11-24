/** Save / copy helpers for message attachments, using browser APIs only (no extra Tauri permission). */

/** A file name that cannot climb out of the download folder or hide a type: no separators, no control characters. */
export function safeFileName(name: string, fallback = "file"): string {
  // eslint-disable-next-line no-control-regex
  const clean = name.replace(/[\u0000-\u001f\u007f\\/:*?"<>|]+/g, "_").replace(/^[.\s]+/, "").trim();
  return (clean || fallback).slice(0, 120);
}

/** Starts a download of `blob` named `name` (an object URL and a link, like the chat export). */
export function saveBlob(blob: Blob, name: string): void {
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = safeFileName(name);
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** The browser can put an image on the clipboard. */
export function canCopyImage(): boolean {
  return typeof ClipboardItem !== "undefined" && typeof navigator !== "undefined" && !!navigator.clipboard?.write;
}

async function asPng(blob: Blob): Promise<Blob> {
  if (blob.type === "image/png") return blob;
  const bitmap = await createImageBitmap(blob);
  const canvas = document.createElement("canvas");
  canvas.width = bitmap.width;
  canvas.height = bitmap.height;
  canvas.getContext("2d")?.drawImage(bitmap, 0, 0);
  bitmap.close();
  return await new Promise<Blob>((resolve, reject) =>
    canvas.toBlob((b) => (b ? resolve(b) : reject(new Error("png"))), "image/png")
  );
}

/** Copies the picture itself (as PNG, the one type every clipboard accepts); false when the browser refuses. */
export async function copyImageBlob(blob: Blob): Promise<boolean> {
  if (!canCopyImage()) return false;
  try {
    const png = await asPng(blob);
    await navigator.clipboard.write([new ClipboardItem({ "image/png": png })]);
    return true;
  } catch {
    return false;
  }
}
