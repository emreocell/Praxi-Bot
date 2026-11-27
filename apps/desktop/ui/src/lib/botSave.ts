import type { ApiClient } from "../api/client.ts";
import type { Bot, McpServer, PutBotSkillsInput } from "../api/types.ts";
import type { BotEditorSection } from "./botEditor.ts";
import { isEmptyPatch, conflictBot, profilePatch } from "./botPatch.ts";
import type { BotDraft } from "./botProfile.ts";
import { MAX_ASSIGNED_SKILLS, exceedsSkillLimit } from "./botSkills.ts";
import { applyAppsSelection, botHasAccess, changedApps } from "./mcpApps.ts";
import { humanLine } from "./humanError.ts";

/**
 * The combined Save of the "Edit bot" dialog (profile, app ticks, skills) as ONE controller: the
 * component only collects its state, calls `performEditorSave` and renders the outcome.
 *
 * - The profile is written with `PATCH /v1/bots/{id}`: only the fields the user edited (the draft
 *   against the draft the dialog opened with), against the profile `version` it loaded.  The daemon
 *   applies the ops atomically and refuses a stale version with `409` + the current bot, so a
 *   concurrent change by another client can neither be lost nor overwritten behind its back.
 * - "Overwrite" repeats the same ops (still only the user's) against the version the conflict
 *   returned - it cannot touch a field the user did not edit.
 * - A skills-only or apps-only Save sends no profile request.
 */

type Json = Record<string, unknown>;

/** JSON with sorted object keys, so equal profiles compare equal whatever the key order. */
export function stableJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (value && typeof value === "object") {
    const o = value as Json;
    return `{${Object.keys(o)
      .sort()
      .map((k) => `${JSON.stringify(k)}:${stableJson(o[k])}`)
      .join(",")}}`;
  }
  return JSON.stringify(value) ?? "null";
}

export type ProfilePart = "identity" | "models" | "permissions" | "runtime" | "other";

/** What another client changed in the stored profile since `base` (for the conflict notice). */
export function changedProfileParts(
  base: Pick<Bot, "name" | "config">,
  fresh: Pick<Bot, "name" | "config">
): ProfilePart[] {
  const a = (base.config ?? {}) as Json;
  const b = (fresh.config ?? {}) as Json;
  const differs = (x: unknown, y: unknown) => stableJson(x ?? null) !== stableJson(y ?? null);
  const out: ProfilePart[] = [];
  if (base.name !== fresh.name || differs(a.bot, b.bot)) out.push("identity");
  if (differs(a.models, b.models)) out.push("models");
  if (differs(a.permissions, b.permissions)) out.push("permissions");
  if (differs(a.runtime, b.runtime)) out.push("runtime");
  const known = new Set(["bot", "models", "permissions", "runtime"]);
  const rest = (c: Json) => Object.fromEntries(Object.entries(c).filter(([k]) => !known.has(k)));
  if (differs(rest(a), rest(b))) out.push("other");
  return out;
}

export type SkillsOutcome =
  | { ok: true }
  | { ok: false; failure: "conflict" | "invalid" | "error"; message: string };

/** A message for the user as a translation key + values (the component translates it). */
export interface Msg {
  key: string;
  params?: Record<string, string | number>;
}

export interface EditorSaveInput {
  client: Pick<ApiClient, "patchBot" | "listMcpServers" | "updateMcpServer">;
  /** The stored bot the dialog was opened on (or last saved); carries the `version` it loaded. */
  baseline: Bot;
  draft: BotDraft;
  /** The draft the dialog was opened on (or last saved). */
  openedDraft: BotDraft;
  /** What blocks Save (`editorProblems`), with the section that fixes it. */
  problems: ReadonlyArray<{ problem: string; section: BotEditorSection }>;
  initialApps: Record<string, boolean>;
  allowed: Record<string, boolean>;
  allBotIds: string[];
  /** The assignment to save when the skills were edited, and how. */
  skills: {
    payload: PutBotSkillsInput | null;
    save: (payload: PutBotSkillsInput) => Promise<SkillsOutcome>;
  };
  /** "Overwrite": the version the conflict returned. */
  expectedVersion?: number;
}

export type EditorSaveOutcome =
  | { kind: "blocked"; section: BotEditorSection; message?: Msg }
  | { kind: "conflict"; fresh: Bot; parts: ProfilePart[] }
  | { kind: "failed"; message: string }
  | {
      kind: "saved";
      /** The stored bot afterwards (the written one, or the untouched baseline). */
      bot: Bot;
      profileWritten: boolean;
      messages: Msg[];
      /** A fresh server list after an app step that did not fully apply. */
      servers: McpServer[] | null;
      /** The ticks to show / treat as saved afterwards. */
      allowed: Record<string, boolean>;
      initialApps: Record<string, boolean>;
      /** Close the dialog: everything was saved. */
      close: boolean;
    };

export async function performEditorSave(i: EditorSaveInput): Promise<EditorSaveOutcome> {
  const first = i.problems[0];
  if (first) return { kind: "blocked", section: first.section };
  if (i.skills.payload && exceedsSkillLimit(i.skills.payload)) {
    return {
      kind: "blocked",
      section: "apps",
      message: { key: "bots.skills.tooMany", params: { max: MAX_ASSIGNED_SKILLS } }
    };
  }
  try {
    let bot = i.baseline;
    let profileWritten = false;
    const patch = profilePatch(i.openedDraft, i.draft);
    if (!isEmptyPatch(patch)) {
      try {
        bot = await i.client.patchBot(i.baseline.id, {
          expected_version: i.expectedVersion ?? i.baseline.version,
          ...(patch.name === undefined ? {} : { name: patch.name }),
          ops: patch.ops
        });
        profileWritten = true;
      } catch (e) {
        const current = conflictBot(e);
        if (current) {
          return { kind: "conflict", fresh: current, parts: changedProfileParts(i.baseline, current) };
        }
        throw e;
      }
    }

    const messages: Msg[] = [];
    const apps = await applyAppsSelection(
      i.client,
      bot.id,
      i.initialApps,
      i.allowed,
      i.allBotIds
    );
    let servers: McpServer[] | null = null;
    let allowed = i.allowed;
    let initialApps = i.allowed;
    if (apps.failed.length > 0 || apps.notApplied.length > 0) {
      servers = await i.client.listMcpServers().catch(() => null);
      const problem = new Set([...apps.failed, ...apps.notApplied]);
      const actual = (name: string): boolean | undefined => {
        const srv = servers?.find((x) => x.name === name);
        return srv ? botHasAccess(srv, bot.id) : undefined;
      };
      allowed = { ...i.allowed };
      for (const name of apps.notApplied) {
        allowed[name] = actual(name) ?? i.allowed[name] ?? false;
      }
      initialApps = { ...i.initialApps };
      for (const name of changedApps(i.initialApps, i.allowed)) {
        if (apps.notApplied.includes(name)) {
          initialApps[name] = actual(name) ?? i.initialApps[name] ?? false;
        } else if (!problem.has(name)) {
          initialApps[name] = i.allowed[name] ?? false;
        }
      }
      if (apps.failed.length > 0) {
        messages.push({ key: "botForm.apps.partialFail", params: { names: apps.failed.join(", ") } });
      }
      if (apps.notApplied.length > 0) {
        messages.push({ key: "botForm.apps.notApplied", params: { names: apps.notApplied.join(", ") } });
      }
    }

    if (i.skills.payload) {
      const sk = await i.skills.save(i.skills.payload);
      if (!sk.ok) {
        messages.push(
          sk.failure === "conflict"
            ? { key: "bots.skills.conflict" }
            : sk.failure === "invalid"
              ? { key: "bots.skills.invalid", params: { detail: sk.message } }
              : { key: "botEdit.skillsFailed", params: { detail: sk.message } }
        );
      }
    }
    return {
      kind: "saved",
      bot,
      profileWritten,
      messages,
      servers,
      allowed,
      initialApps,
      close: messages.length === 0
    };
  } catch (e) {
    return { kind: "failed", message: humanLine(e) };
  }
}
