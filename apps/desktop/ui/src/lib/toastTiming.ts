export type ToastTone = "info" | "success" | "danger";

const TOAST_MS = 4000;

/** Errors stay a little longer than confirmations: they need to be read. */
export function toastDuration(tone: ToastTone): number {
  return tone === "danger" ? TOAST_MS + 3000 : TOAST_MS;
}
