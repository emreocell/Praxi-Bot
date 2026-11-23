import type { GlobalEnvelope, Message } from "./types";

const START = 1_750_000_000_000;
const CHAT = "33333333-3333-4333-8333-333333333333";
const BOT = "11111111-1111-4111-8111-111111111111";

/** Deterministic, local-only performance fixture: 2,000 messages and 100 step groups. */
export function longChatMessages(): Message[] {
  return Array.from({ length: 2000 }, (_, i) => ({
    id: `long-${i}`, chat_id: CHAT, role: i % 2 ? "assistant" : "user",
    created_at_ms: START + i * 1000,
    content: i % 2
      ? `## Result ${i}\n\nReviewed **the changes** and checked the next steps. Search-marker-${i}.\n\n` +
        "```ts\n" + Array.from({ length: 12 }, (_, n) => `const value${n} = ${i} + ${n}; // example`).join("\n") +
        "\n```\n\n- Reviewed files\n- Checked results\n- Ready to continue"
      : `Please review item ${i} and explain the result.`,
    source: "owner"
  }));
}

export function longChatEvents(): { ts_ms: number; event: GlobalEnvelope["event"] }[] {
  return Array.from({ length: 100 }, (_, i) => {
    const run_id = `long-run-${i}`;
    const events: GlobalEnvelope["event"][] = [
      { type: "run_started", run_id, bot_id: BOT, chat_id: CHAT },
      ...Array.from({ length: 3 }, (_, n): GlobalEnvelope["event"][] => [
        { type: "tool_call_requested", run_id, call_id: `${run_id}-${n}`, tool_name: "fs_read", input: { path: `example-${i}-${n}.ts` } },
        { type: "tool_call_completed", run_id, call_id: `${run_id}-${n}`, error: null }
      ]).flat(),
      { type: "run_finished", run_id, error: null }
    ];
    return events.map((event, n) => ({ ts_ms: START + (i * 20 + 1) * 1000 + n, event }));
  }).flat();
}
