import type { Bot } from "../api/types.ts";
import { globMatches } from "./botMail.ts";
import type { PatchOp } from "./botPatch.ts";

/** What "use files and commands" means in a profile: the tools that write a file and run a command. */
const FILE_TOOLS = ["fs_write", "fs_edit", "checkpoint_restore"] as const;
const COMMAND_TOOL = "shell_run";
/** The globs the card adds to an "only these tools" list. */
const GRANT = ["fs_*", "checkpoint_*", COMMAND_TOOL] as const;

function list(config: unknown, key: "allow" | "exclude"): string[] {
  const tools = (config as { tools?: Record<string, unknown> } | null | undefined)?.tools;
  const v = tools?.[key];
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
}

const matchesAny = (globs: readonly string[], name: string) => globs.some((g) => globMatches(g, name));

function offered(config: unknown, name: string): boolean {
  const allow = list(config, "allow");
  return (allow.length === 0 || matchesAny(allow, name)) && !matchesAny(list(config, "exclude"), name);
}

/** A "Folder only" bot never gets a shell on purpose (it cannot be confined to a folder). */
function folderOnly(config: unknown): boolean {
  return (config as { files?: { mode?: unknown } } | null | undefined)?.files?.mode === "folder";
}

/**
 * Whether the bot's tool list leaves out writing files or running commands. Only the profile's
 * tool lists count here: the permission level (ask / allow / deny) is a separate, visible setting.
 */
export function filesAndCommandsMissing(config: unknown): boolean {
  if (!offered(config, "fs_write") || !offered(config, "fs_edit")) return true;
  return !folderOnly(config) && !offered(config, COMMAND_TOOL);
}

/**
 * Profile edits that put files and commands back in the tool list: they add to `allow` only when
 * the list is a filter, and drop the `exclude` entries that hide exactly these tools. Nothing else
 * (permission level, file folders, other tools) is touched, so every approval still applies.
 */
export function filesAndCommandsOps(config: unknown): PatchOp[] {
  const allow = list(config, "allow");
  const exclude = list(config, "exclude");
  const ops: PatchOp[] = [];
  const hides = (g: string) => [...FILE_TOOLS, COMMAND_TOOL].some((n) => globMatches(g, n)) && g !== "*";
  const rest = exclude.filter((g) => !hides(g));
  if (rest.length !== exclude.length) {
    ops.push(rest.length > 0 ? { op: "set", path: "/tools/exclude", value: rest } : { op: "unset", path: "/tools/exclude" });
  }
  if (allow.length > 0) {
    const missing = GRANT.filter((g) => !allow.includes(g) && !matchesAny(allow, g.replace("*", "x")));
    if (missing.length > 0) {
      ops.push({ op: "set", path: "/tools/allow", value: [...allow, ...missing] });
    }
  }
  return ops;
}

export function filesAndCommandsPatch(bot: Bot): { expected_version: number; ops: PatchOp[] } {
  return { expected_version: bot.version, ops: filesAndCommandsOps(bot.config) };
}
