import type { GlobalEnvelope } from "../api/types.ts";

/** Batch visual text updates once per frame. All control/approval events remain immediate.
 * Flush before a non-text event so tool boundaries and run completion retain stream order.
 * Raw event subscribers must still receive each original envelope immediately.
 */
export function createStreamBatch(
  publish: (events: readonly GlobalEnvelope[]) => void,
  request: (callback: () => void) => number,
  cancel: (id: number) => void
) {
  let pending: GlobalEnvelope[] = [];
  let frame: number | undefined;
  const flush = () => {
    if (frame !== undefined) cancel(frame);
    frame = undefined;
    if (pending.length) {
      const batch = pending;
      pending = [];
      publish(batch);
    }
  };
  return {
    push(env: GlobalEnvelope) {
      if (env.event.type !== "message_delta") {
        flush();
        publish([env]);
        return;
      }
      pending.push(env);
      // Bound the queue even when a background window suspends animation frames.
      if (pending.length >= 256) flush();
      else if (frame === undefined) frame = request(flush);
    },
    flush,
    dispose() {
      if (frame !== undefined) cancel(frame);
      frame = undefined;
      pending = [];
    }
  };
}
