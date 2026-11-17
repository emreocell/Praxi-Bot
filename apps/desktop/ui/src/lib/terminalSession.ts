/**
 * The desktop's side of the server terminal: a poll loop over the paired link (no SSH, no new
 * port). The host keeps the shell and a bounded output buffer; this reads it by absolute byte
 * offset and sends typed bytes in order. Pure logic, no DOM: the view feeds the output to the
 * terminal widget and the widget's input to `send`.
 */

export interface TerminalOutputDto {
  /** Base64 of the next output bytes. */
  data: string;
  next: number;
  dropped: boolean;
  exited?: number | null;
}

export interface TerminalApiLike {
  open(cols: number, rows: number): Promise<{ id: string }>;
  output(id: string, after: number, waitMs: number, signal: AbortSignal): Promise<TerminalOutputDto>;
  input(id: string, base64: string): Promise<void>;
  resize(id: string, cols: number, rows: number): Promise<void>;
  close(id: string): Promise<void>;
}

export interface TerminalSink {
  write(bytes: Uint8Array): void;
  /** The shell ended, or the session is gone (`code` null). */
  ended(code: number | null): void;
  /** The link failed for good; `error` is for `humanError`. */
  failed(error: unknown): void;
}

/** Longest wait the host honours; shorter keeps the abort snappy. */
export const POLL_WAIT_MS = 2500;
/** Typed bytes are sent in chunks of at most this many bytes (the host takes 16 KiB). */
export const MAX_INPUT_CHUNK = 8 * 1024;
/** A failing poll is retried this many times (with growing pauses) before the link gives up. */
export const MAX_POLL_FAILURES = 5;

export function bytesToBase64(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) {
    s += String.fromCharCode(b);
  }
  return btoa(s);
}

export function base64ToBytes(b64: string): Uint8Array {
  const s = atob(b64);
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i += 1) {
    out[i] = s.charCodeAt(i);
  }
  return out;
}

/** Split typed text into UTF-8 chunks the host accepts. */
export function inputChunks(text: string): Uint8Array[] {
  const bytes = new TextEncoder().encode(text);
  const chunks: Uint8Array[] = [];
  for (let i = 0; i < bytes.length; i += MAX_INPUT_CHUNK) {
    chunks.push(bytes.subarray(i, i + MAX_INPUT_CHUNK));
  }
  return chunks;
}

/** `true` for the host's "session ended or never yours" answer. */
export function isGone(error: unknown): boolean {
  const e = error as { status?: number; code?: string } | null;
  return e?.code === "terminal_gone" || e?.status === 404;
}

export class TerminalLink {
  private id: string | null = null;
  private after = 0;
  private readonly abort = new AbortController();
  private disposed = false;
  private sending: Promise<void> = Promise.resolve();

  private readonly api: TerminalApiLike;
  private readonly sink: TerminalSink;
  private readonly sleep: (ms: number) => Promise<void>;

  constructor(
    api: TerminalApiLike,
    sink: TerminalSink,
    sleep: (ms: number) => Promise<void> = (ms) => new Promise((resolve) => setTimeout(resolve, ms))
  ) {
    this.api = api;
    this.sink = sink;
    this.sleep = sleep;
  }

  /** Open the shell and start reading. Resolves once it is open (reading continues). */
  async start(cols: number, rows: number): Promise<void> {
    try {
      const info = await this.api.open(cols, rows);
      if (this.disposed) {
        void this.api.close(info.id).catch(() => undefined);
        return;
      }
      this.id = info.id;
    } catch (e) {
      this.sink.failed(e);
      return;
    }
    void this.pump();
  }

  private async pump(): Promise<void> {
    let failures = 0;
    while (!this.disposed && this.id) {
      try {
        const out = await this.api.output(this.id, this.after, POLL_WAIT_MS, this.abort.signal);
        failures = 0;
        this.after = out.next;
        if (out.data) {
          this.sink.write(base64ToBytes(out.data));
        }
        if (out.exited !== undefined && out.exited !== null) {
          this.finish(out.exited);
          return;
        }
      } catch (e) {
        if (this.disposed) {
          return;
        }
        if (isGone(e)) {
          this.finish(null);
          return;
        }
        failures += 1;
        if (failures >= MAX_POLL_FAILURES) {
          this.disposed = true;
          this.sink.failed(e);
          return;
        }
        await this.sleep(300 * failures);
      }
    }
  }

  private finish(code: number | null): void {
    this.disposed = true;
    this.sink.ended(code);
  }

  /** Type text (what the terminal widget produced, or a pasted command: never auto-executed). */
  send(text: string): void {
    const id = this.id;
    if (!id || this.disposed || text === "") {
      return;
    }
    // In order, one request at a time.
    this.sending = this.sending.then(async () => {
      for (const chunk of inputChunks(text)) {
        if (this.disposed) {
          return;
        }
        try {
          await this.api.input(id, bytesToBase64(chunk));
        } catch (e) {
          if (isGone(e)) {
            this.finish(null);
          }
          return;
        }
      }
    });
  }

  resize(cols: number, rows: number): void {
    if (this.id && !this.disposed) {
      void this.api.resize(this.id, cols, rows).catch(() => undefined);
    }
  }

  /** Close the view: the host ends the shell (it also does so when the polls stop). */
  dispose(): void {
    const id = this.id;
    this.disposed = true;
    this.abort.abort();
    if (id) {
      this.id = null;
      void this.api.close(id).catch(() => undefined);
    }
  }
}
