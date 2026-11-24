import { type PendingFile, room, uploadErrorKey } from "./attachments.ts";
import { AttachmentPreparationError, prepareAttachment, type PreparedAttachment } from "./prepareAttachment.ts";

type Upload = (blob: Blob, name: string, signal: AbortSignal) => Promise<{ id: string; kind: string; truncated?: boolean }>;

/** One in-memory chat draft. No network activity happens on add, paste, drop, or retry. */
export class AttachmentDraft {
  private files: PendingFile[] = [];
  private listeners = new Set<() => void>();
  private work: Promise<void> = Promise.resolve();
  private cancellation = new AbortController();
  locked = false;

  private readonly prepare: (file: File) => Promise<PreparedAttachment>;
  private readonly urls: { create(blob: Blob): string; revoke(url: string): void };
  constructor(prepare: (file: File) => Promise<PreparedAttachment> = prepareAttachment,
    urls = { create: (blob: Blob) => URL.createObjectURL(blob), revoke: (url: string) => URL.revokeObjectURL(url) }) {
    this.prepare = prepare;
    this.urls = urls;
  }

  snapshot = () => this.files;
  subscribe = (listener: () => void) => { this.listeners.add(listener); return () => { this.listeners.delete(listener); }; };
  get signal() { return this.cancellation.signal; }
  get blocked() { return this.files.some((f) => f.state !== "ready"); }
  whenPrepared() { return this.work; }

  private publish(files: PendingFile[]) {
    this.files = files;
    for (const listener of this.listeners) listener();
  }
  private patch(key: string, patch: Partial<PendingFile>) {
    this.publish(this.files.map((f) => f.key === key ? { ...f, ...patch } : f));
  }

  /** Returns how many files did not fit. Reserve slots synchronously, including failed files. */
  add(incoming: File[], fallbackName: string): number {
    if (this.locked) return incoming.length;
    const accepted = incoming.slice(0, room(this.files.length, incoming.length));
    for (const file of accepted) {
      const key = crypto.randomUUID();
      const signal = this.signal;
      this.publish([...this.files, { key, name: file.name || fallbackName, size: file.size, state: "preparing" }]);
      // Decode one image at a time to keep memory bounded for a six-photo drop.
      this.work = this.work.then(async () => {
        const present = () => !signal.aborted && this.files.some((f) => f.key === key);
        if (!present()) return;
        try {
          const prepared = await this.prepare(file);
          if (!present()) return;
          const preview = prepared.kind === "image" ? this.urls.create(prepared.blob) : undefined;
          this.patch(key, { ...prepared, size: prepared.blob.size, preview, state: "ready" });
        } catch (error) {
          if (present()) this.patch(key, { state: "failed", errorKey: error instanceof AttachmentPreparationError ? error.key : "chat.attach.failed" });
        }
      });
    }
    return incoming.length - accepted.length;
  }

  remove(key: string) {
    if (this.locked) return;
    for (const file of this.files) if (file.key === key && file.preview) this.urls.revoke(file.preview);
    this.publish(this.files.filter((f) => f.key !== key));
  }

  retry(key: string) {
    if (this.locked) return;
    const file = this.files.find((f) => f.key === key);
    if (file?.blob && file.state === "failed") this.patch(key, { state: "ready", errorKey: undefined });
  }

  clear() {
    this.cancellation.abort();
    this.cancellation = new AbortController();
    for (const file of this.files) if (file.preview) this.urls.revoke(file.preview);
    this.publish([]);
    this.locked = false;
  }

  /** Called only after Send passes the model/secret checks. Reuse successful uploads on retry. */
  async uploadAll(upload: Upload): Promise<string[]> {
    if (this.files.some((f) => f.state === "preparing" || !f.blob)) throw new Error("attachment draft is not ready");
    const signal = this.signal;
    const ids: string[] = [];
    for (const file of this.files) {
      signal.throwIfAborted();
      if (file.id) { ids.push(file.id); continue; }
      this.patch(file.key, { state: "uploading" });
      try {
        const attachment = await upload(file.blob!, file.name, signal);
        signal.throwIfAborted();
        this.patch(file.key, { state: "ready", id: attachment.id, kind: attachment.kind, truncated: attachment.truncated, errorKey: undefined });
        ids.push(attachment.id);
      } catch (error) {
        if (!signal.aborted) {
          const err = error as { code?: string; status?: number };
          this.patch(file.key, { state: "failed", errorKey: uploadErrorKey(err?.code, err?.status) });
        }
        throw error;
      }
    }
    return ids;
  }
}
