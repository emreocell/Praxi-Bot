import { ApiError } from "./client.ts";
import type {
  CreateMission,
  Mission,
  MissionAnswer,
  MissionAnswersResult,
  MissionDetail,
  MissionDigest,
  MissionItem,
  MissionProgress
} from "./types.ts";

/** Sample missions for mock mode only (design review, screenshots): one running, one finished. */

const MIN = 60_000;
const BUDGETS: Mission["budgets"] = {
  max_sessions: 40,
  max_model_steps: 2_000,
  max_minutes: 600,
  max_cost_usd: 5,
  max_stalled_sessions: 3,
  max_failed_sessions: 3
};
const FULL_REQUIRED = "a mission runs without asking, so the bot has to be on the Full level first";

function item(key: string, status: MissionItem["status"], at: number, extra: Partial<MissionItem> = {}): MissionItem {
  return {
    key,
    kind: "site",
    title: `Register on ${key}`,
    status,
    attempts: status === "todo" ? 0 : 1,
    result: null,
    last_error: null,
    blocked_reason: null,
    needs_live_owner: false,
    owner_answer: null,
    waiting_for: [],
    updated_at_ms: at,
    ...extra
  };
}

function progressOf(items: readonly MissionItem[]): MissionProgress {
  const n = (s: string) => items.filter((i) => i.status === s).length;
  const [todo, doing, blocked] = [n("todo"), n("doing"), n("blocked")];
  return {
    total: items.length,
    done: n("done"),
    blocked,
    waiting: n("waiting"),
    todo,
    doing,
    failed: n("failed"),
    skipped: n("skipped"),
    left: todo + doing
  };
}

export class MockMissions {
  private readonly missions: Mission[];
  private readonly items = new Map<string, MissionItem[]>();
  private readonly digests = new Map<string, MissionDigest>();

  constructor(botId: string, now = Date.now(), inChat: { id: string; chatId: string } | null = null) {
    const sites = ["alpha.io", "bravo.dev", "charlie.app", "delta.net", "echo.co", "foxtrot.org", "golf.com", "hotel.io", "india.dev", "juliet.app", "kilo.net", "lima.co", "mike.org", "nova.com", "oscar.io"];
    const running: MissionItem[] = sites.map((key, i) => {
      const at = now - (15 - i) * MIN;
      if (i < 7) return item(key, "done", at, { result: "Profile created, confirmation mail received." });
      if (i === 7) return item(key, "blocked", at, { blocked_reason: "A CAPTCHA appeared on the sign-up form." });
      if (i === 8) {
        return item(key, "blocked", at, {
          blocked_reason: "The site wants the 6-digit code it sent to your phone.",
          needs_live_owner: true
        });
      }
      if (i === 10) {
        return item(key, "waiting", at, {
          waiting_for: [
            {
              kind: "mail",
              from_domain: key,
              subject_contains: null,
              keyword: null,
              webhook_name: null,
              due_at_ms: now + 45 * MIN,
              reason: "Waiting for the verification mail after the sign-up form."
            }
          ]
        });
      }
      if (i === 11) {
        return item(key, "waiting", at, {
          waiting_for: [
            { kind: "after", from_domain: null, subject_contains: null, keyword: null, webhook_name: null, due_at_ms: now + 3 * 60 * MIN, reason: "The site reviews new accounts after a few hours." }
          ]
        });
      }
      return item(key, i === 9 ? "doing" : "todo", at);
    });
    this.items.set("mission-running", running);
    const ended: MissionItem[] = [
      ...Array.from({ length: 10 }, (_, i) => item(`site-${i + 1}.org`, "done", now - 3 * 86_400_000, { result: "Listing published." })),
      item("paywall.biz", "failed", now - 3 * 86_400_000, { last_error: "The site only takes paid listings." }),
      item("closed.net", "skipped", now - 3 * 86_400_000)
    ];
    this.items.set("mission-done", ended);
    this.digests.set("mission-done", {
      at_ms: now - 3 * 86_400_000,
      outcome: "done",
      reason: null,
      progress: progressOf(ended),
      blocked: [],
      failed: [{ key: "paywall.biz", title: "Register on paywall.biz", text: "The site only takes paid listings." }],
      done_sample: ended.slice(0, 3).map((i) => ({ key: i.key, title: i.title, text: i.result })),
      decisions: ["Used the owner's work address for every sign-up.", "Left out sites that ask for a payment."]
    });
    const base = {
      bot_id: botId,
      chat_id: "chat-mission",
      cost_usd: 1.4,
      budgets: BUDGETS,
      target_done: null
    };
    this.missions = [
      {
        ...base,
        id: "mission-running",
        title: "Backlinks from 15 sites",
        goal: "Register me on these 15 sites so I get backlinks, and report what worked.",
        state: "running",
        state_reason: null,
        progress: progressOf(running),
        sessions: 4,
        model_steps: 212,
        session_running: true,
        started_at_ms: now - 90 * MIN,
        updated_at_ms: now - MIN,
        finished_at_ms: null
      },
      {
        ...base,
        id: "mission-done",
        title: "Directory listings",
        goal: "List my shop in the local web directories.",
        state: "done",
        state_reason: null,
        progress: progressOf(ended),
        sessions: 6,
        model_steps: 301,
        cost_usd: 2.1,
        session_running: false,
        started_at_ms: now - 3.5 * 86_400_000,
        updated_at_ms: now - 3 * 86_400_000,
        finished_at_ms: now - 3 * 86_400_000
      }
    ];
    const moved = inChat ? this.missions.find((m) => m.id === inChat.id) : undefined;
    if (moved && inChat) moved.chat_id = inChat.chatId;
  }

  private detail(m: Mission): MissionDetail {
    const items = this.items.get(m.id) ?? [];
    return {
      mission: { ...m, progress: progressOf(items) },
      items,
      blocked: items.filter((i) => i.status === "blocked"),
      digest: this.digests.get(m.id) ?? null
    };
  }

  private find(id: string): Mission {
    const m = this.missions.find((x) => x.id === id);
    if (!m) throw new ApiError(404, "not_found", "mission not found");
    return m;
  }

  list(botId: string | undefined, state: string | undefined): Mission[] {
    return this.missions
      .filter((m) => (!botId || m.bot_id === botId) && (state !== "active" || ["running", "waiting", "paused", "blocked"].includes(m.state)))
      .map((m) => this.detail(m).mission)
      .sort((a, b) => b.updated_at_ms - a.updated_at_ms);
  }

  get(id: string): MissionDetail {
    return this.detail(this.find(id));
  }

  start(input: CreateMission, fullBots: readonly string[]): MissionDetail {
    if (!fullBots.includes(input.bot_id)) throw new ApiError(400, "bad_request", FULL_REQUIRED);
    const goal = input.goal.trim();
    if (!goal) throw new ApiError(400, "bad_request", "the goal must be 1 to 4000 characters");
    const now = Date.now();
    const m: Mission = {
      id: `mission-${this.missions.length + 1}`,
      bot_id: input.bot_id,
      chat_id: "chat-mission",
      title: input.title ?? goal.slice(0, 60),
      goal,
      state: "running",
      state_reason: null,
      progress: progressOf([]),
      target_done: input.target_done ?? null,
      sessions: 0,
      model_steps: 0,
      cost_usd: 0,
      session_running: true,
      budgets: BUDGETS,
      started_at_ms: now,
      updated_at_ms: now,
      finished_at_ms: null
    };
    this.missions.push(m);
    this.items.set(m.id, []);
    return this.detail(m);
  }

  steer(id: string, action: string): MissionDetail {
    const m = this.find(id);
    const now = Date.now();
    const next = action === "pause" ? "paused" : action === "resume" ? "running" : "cancelled";
    if (["done", "failed", "cancelled"].includes(m.state)) throw new ApiError(409, "conflict", "the mission has ended");
    m.state = next;
    m.state_reason = action === "pause" ? "Paused by you." : null;
    m.session_running = next === "running";
    m.updated_at_ms = now;
    if (next === "cancelled") {
      m.finished_at_ms = now;
      this.digests.set(id, {
        at_ms: now,
        outcome: "cancelled",
        reason: "Stopped by you.",
        progress: progressOf(this.items.get(id) ?? []),
        blocked: (this.items.get(id) ?? []).filter((i) => i.status === "blocked").map((i) => ({ key: i.key, title: i.title, text: i.blocked_reason })),
        failed: [],
        done_sample: [],
        decisions: []
      });
    }
    return this.detail(m);
  }

  answer(id: string, answers: readonly MissionAnswer[]): MissionAnswersResult {
    const m = this.find(id);
    const items = this.items.get(id) ?? [];
    let released = 0;
    let skipped = 0;
    for (const a of answers) {
      const it = items.find((i) => i.key === a.key && i.status === "blocked");
      if (!it) continue;
      if (a.skip) {
        it.status = "skipped";
        skipped += 1;
      } else if (it.needs_live_owner) {
        // As the daemon: a code is never taken as an answer.
        throw new ApiError(400, "bad_request", `item ${a.key} needs you live: a code is never taken as an answer`);
      } else if (a.answer?.trim()) {
        it.status = "todo";
        it.owner_answer = a.answer.trim();
        released += 1;
      } else {
        throw new ApiError(400, "bad_request", `item ${a.key} needs an answer or skip`);
      }
      it.blocked_reason = null;
      it.updated_at_ms = Date.now();
    }
    m.updated_at_ms = Date.now();
    return { released, skipped };
  }
}
