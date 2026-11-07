import type { ApiClient } from "./client";
import type { DaemonEvent, GlobalEnvelope } from "./types";

export interface EventStreamOptions {
  /** Resume after this daemon-wide `gseq` (`Last-Event-ID`); `0` replays everything. */
  lastEventId: number | null;
  onEvent: (e: GlobalEnvelope) => void;
  /** Called when the stream is up (`true`) or lost (`false`). */
  onConnection: (connected: boolean) => void;
}

export interface SseMessage {
  id: string | null;
  event: string;
  data: string;
}

/**
 * Incremental parser of a `text/event-stream` body.  Feed decoded text chunks;
 * each complete event is passed to `emit`.
 */
export function createSseParser(
  emit: (m: SseMessage) => void
): (chunk: string) => void {
  let buffer = "";
  let id: string | null = null;
  let event = "message";
  let data: string[] = [];

  const dispatch = () => {
    if (data.length > 0) {
      emit({ id, event, data: data.join("\n") });
    }
    event = "message";
    data = [];
  };

  return (chunk: string) => {
    buffer += chunk;
    let nl = buffer.indexOf("\n");
    while (nl >= 0) {
      let line = buffer.slice(0, nl);
      buffer = buffer.slice(nl + 1);
      nl = buffer.indexOf("\n");
      if (line.endsWith("\r")) {
        line = line.slice(0, -1);
      }
      if (line === "") {
        dispatch();
      } else if (!line.startsWith(":")) {
        const colon = line.indexOf(":");
        const field = colon < 0 ? line : line.slice(0, colon);
        let value = colon < 0 ? "" : line.slice(colon + 1);
        if (value.startsWith(" ")) {
          value = value.slice(1);
        }
        if (field === "id") {
          id = value;
        } else if (field === "event") {
          event = value;
        } else if (field === "data") {
          data.push(value);
        }
      }
    }
  };
}

const BACKOFF_MS = [500, 1000, 2000, 4000, 8000];

/**
 * Subscribe to the daemon-wide event stream with automatic reconnect.  The
 * `Authorization` header and `Last-Event-ID` are sent on every (re)connect, so
 * no event is missed or duplicated across a dropped connection (and the token
 * never appears in a URL).  Returns a function that stops the subscription.
 */
export function subscribeEvents(
  client: ApiClient,
  opts: EventStreamOptions
): () => void {
  const controller = new AbortController();
  let cursor = opts.lastEventId;

  const run = async () => {
    let attempt = 0;
    while (!controller.signal.aborted) {
      try {
        const headers: Record<string, string> = {
          ...client.authHeaders(),
          Accept: "text/event-stream"
        };
        if (cursor !== null) {
          headers["Last-Event-ID"] = String(cursor);
        }
        const res = await fetch(`${client.baseUrl}/v1/events`, {
          headers,
          signal: controller.signal
        });
        if (!res.ok || !res.body) {
          throw new Error(`HTTP ${res.status}`);
        }
        opts.onConnection(true);
        attempt = 0;
        const parse = createSseParser((m) => {
          try {
            const env = JSON.parse(m.data) as {
              v: number;
              seq: number;
              ts_ms: number;
              event: DaemonEvent;
            };
            const parsed = m.id === null ? Number.NaN : Number(m.id);
            const gseq = Number.isFinite(parsed) ? parsed : (cursor ?? 0);
            if (Number.isFinite(parsed)) {
              cursor = parsed;
            }
            opts.onEvent({ ...env, gseq });
          } catch {
            // ignore malformed events
          }
        });
        const reader = res.body.pipeThrough(new TextDecoderStream()).getReader();
        for (;;) {
          const { value, done } = await reader.read();
          if (done) {
            break;
          }
          parse(value);
        }
      } catch {
        if (controller.signal.aborted) {
          return;
        }
      }
      opts.onConnection(false);
      const delay = BACKOFF_MS[Math.min(attempt, BACKOFF_MS.length - 1)];
      attempt += 1;
      await new Promise((r) => setTimeout(r, delay));
    }
  };
  void run();
  return () => controller.abort();
}
