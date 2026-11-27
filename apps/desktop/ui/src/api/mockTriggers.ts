// Mock mode: a bot's event rules (mail, webhook, time) for the Automation tab. One mail rule and
// one signed address are seeded for the first bot so the tab shows both kinds of rows.
import { ApiError } from "./client.ts";
import { newTrigger, type BotTrigger, type TriggerInput } from "../lib/triggers.ts";

const HOUR = 3_600_000;

export class MockTriggers {
  private readonly rows = new Map<string, BotTrigger[]>();
  private next = 1;

  constructor(seedBot: string) {
    const base = newTrigger();
    this.rows.set(seedBot, [
      {
        ...base,
        id: "trg-1",
        bot_id: seedBot,
        origin: "rule",
        author: "unknown",
        revision: 1,
        name: "Muhasebeciden gelen faturalar",
        instruction: "Faturayı özetle, son ödeme tarihini söyle.",
        enabled: true,
        config: { ...base.config, filters: { ...base.config.filters, sender: "muhasebe@ornek.com.tr" } },
        last_fired_at_ms: Date.now() - 26 * HOUR,
        count_today: 0,
        queued: 0,
        last_error: null
      },
      {
        ...base,
        id: "hook-1",
        bot_id: seedBot,
        origin: "webhook",
        author: "unknown",
        revision: 1,
        name: "Web sitesi iletişim formu",
        instruction: "Yeni mesajı özetle.",
        enabled: true,
        config: { ...base.config, source: { kind: "webhook", id: "hook-1" } },
        last_fired_at_ms: null,
        count_today: null,
        queued: null,
        last_error: null
      }
    ]);
  }

  route(method: string, seg: string[], body: unknown): unknown {
    const bot = seg[1] ?? "";
    const list = this.rows.get(bot) ?? [];
    const id = seg[3];
    if (method === "GET" && !id) return structuredClone(list);
    if (method === "POST" && id === "preview") {
      return { text: "Instruction and protected event text (mock).", matches: true };
    }
    if (method === "POST" && !id) {
      const input = body as TriggerInput;
      const row: BotTrigger = { ...input, id: `trg-new-${this.next++}`, bot_id: bot, origin: "rule", author: "owner", revision: 1, last_fired_at_ms: null, count_today: 0, queued: 0, last_error: null };
      this.rows.set(bot, [...list, row]);
      return structuredClone(row);
    }
    const index = list.findIndex((r) => r.id === id);
    if (index < 0) throw new ApiError(404, "not_found", "trigger");
    if (method === "DELETE") {
      this.rows.set(bot, list.filter((r) => r.id !== id));
      return undefined;
    }
    if (method === "POST" && seg[4] === "confirm") {
      const asked = (body as { revision?: number } | undefined)?.revision;
      if (asked !== list[index].revision) throw new ApiError(409, "conflict", "this trigger changed after you looked at it");
      list[index] = { ...list[index], author: "owner", revision: list[index].revision + 1 };
      return structuredClone(list[index]);
    }
    if (method === "PUT") {
      list[index] = { ...list[index], ...(body as TriggerInput), author: "owner", revision: list[index].revision + 1 };
      return structuredClone(list[index]);
    }
    throw new ApiError(404, "not_found", `mock: ${method} triggers`);
  }
}
