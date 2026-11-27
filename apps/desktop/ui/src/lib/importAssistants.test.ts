import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { ApiError } from "../api/client.ts";
import { mockImportRun, mockImportScan } from "../api/mockImport.ts";
import type { Bot, ImportItem, ImportResult } from "../api/types.ts";
import { applyPatchOps } from "./botPatch.ts";
import {
  appendPersona,
  appendToBotPersona,
  applyInstructionResults,
  buildRunInput,
  buildScanInput,
  charCount,
  confirmedSecrets,
  groupItems,
  importedServers,
  initialSecretTicks,
  initialTicks,
  instructionsWithoutBot,
  isImportable,
  knownReason,
  PERSONA_MAX_CHARS,
  selectedIds,
  tally,
  toggled
} from "./importAssistants.ts";

const scan = () => mockImportScan({ folder: "/home/me/prompts" }).items;
const byName = (items: ImportItem[], name: string) => {
  const hit = items.find((i) => i.name === name);
  assert.ok(hit, `fixture ${name}`);
  return hit;
};

test("items are grouped by kind; anything not importable lands in the could-not list", () => {
  const g = groupItems(scan());
  assert.deepEqual(g.mcp.map((i) => i.name), ["github", "filesystem", "docs", "memory"]);
  assert.deepEqual(g.skills.map((i) => i.name), ["release-notes", "deploy-helper", "review-checklist"]);
  assert.deepEqual(g.instructions.map((i) => i.name), ["CLAUDE.md", "AGENTS.md"]);
  assert.deepEqual(g.notImported.map((i) => i.name), ["old-tool", "shell-runner", "Saved logins"]);
  // An `other` item is never importable, even if the daemon said import: true.
  const odd = { ...byName(scan(), "Saved logins"), import: true };
  assert.equal(isImportable(odd), false);
  assert.equal(groupItems([odd]).notImported.length, 1);
});

test("default ticks follow default_on: instructions and not-importable items start unticked", () => {
  const items = scan();
  const ticks = initialTicks(items);
  assert.ok(ticks.has(byName(items, "github").id));
  assert.ok(ticks.has(byName(items, "release-notes").id));
  assert.ok(!ticks.has(byName(items, "CLAUDE.md").id));
  assert.ok(!ticks.has(byName(items, "deploy-helper").id));
  assert.ok(!ticks.has(byName(items, "old-tool").id));
  // default_on on an item that cannot be imported is ignored.
  const forced = items.map((i) => (i.name === "old-tool" ? { ...i, default_on: true } : i));
  assert.ok(!initialTicks(forced).has(byName(items, "old-tool").id));
});

test("secret slots start unticked and only count while their item is ticked", () => {
  const items = scan();
  const ticks = initialTicks(items);
  assert.deepEqual(confirmedSecrets(items, ticks, initialSecretTicks()), []);

  const github = byName(items, "github");
  const docs = byName(items, "docs");
  let secrets = toggled(initialSecretTicks(), "mcp_github_GITHUB_TOKEN", true);
  secrets = toggled(secrets, "mcp_docs_Authorization", true);
  assert.deepEqual(confirmedSecrets(items, ticks, secrets), ["mcp_github_GITHUB_TOKEN", "mcp_docs_Authorization"]);

  // Unticking the item drops its confirmation from the request.
  const without = toggled(ticks, github.id, false);
  assert.deepEqual(confirmedSecrets(items, without, secrets), ["mcp_docs_Authorization"]);
  // An unknown name never gets through.
  assert.deepEqual(confirmedSecrets(items, ticks, new Set(["not_a_slot"])), []);
  assert.ok(docs.secrets.length > 0);
});

test("selectedIds keeps scan order and refuses ids that are not importable", () => {
  const items = scan();
  const ticks = new Set([byName(items, "release-notes").id, byName(items, "github").id, byName(items, "old-tool").id, "ghost"]);
  assert.deepEqual(selectedIds(items, ticks), [byName(items, "github").id, byName(items, "release-notes").id]);
});

test("run request carries only ids and secret names, plus the folder it was scanned with", () => {
  const items = scan();
  const ticks = new Set([byName(items, "github").id, byName(items, "filesystem").id]);
  const body = buildRunInput({ folder: "/home/me/prompts" }, items, ticks, new Set(["mcp_github_GITHUB_TOKEN"]));
  assert.deepEqual(body, {
    folder: "/home/me/prompts",
    ids: [byName(items, "github").id, byName(items, "filesystem").id],
    confirmed_secrets: ["mcp_github_GITHUB_TOKEN"]
  });
  assert.deepEqual(buildRunInput({}, items, new Set(), new Set()), { ids: [], confirmed_secrets: [] });
  assert.equal("only" in buildRunInput({ only: "cursor" }, items, new Set(), new Set()), true);
});

test("scan request: blank folder is left out, a typed one is trimmed", () => {
  assert.deepEqual(buildScanInput("   "), {});
  assert.deepEqual(buildScanInput(" /srv/prompts "), { folder: "/srv/prompts" });
  assert.deepEqual(buildScanInput("", "codex"), { only: "codex" });
});

test("an instruction item needs a bot that exists before it can run", () => {
  const items = scan();
  const md = byName(items, "CLAUDE.md");
  const ticks = new Set([md.id]);
  assert.deepEqual(instructionsWithoutBot(items, ticks, {}, ["b1"]).map((i) => i.id), [md.id]);
  assert.deepEqual(instructionsWithoutBot(items, ticks, { [md.id]: "gone" }, ["b1"]).map((i) => i.id), [md.id]);
  assert.deepEqual(instructionsWithoutBot(items, ticks, { [md.id]: "b1" }, ["b1"]), []);
  assert.deepEqual(instructionsWithoutBot(items, new Set(), {}, ["b1"]), []);
});

test("knownReason accepts the contract codes and ignores new ones", () => {
  assert.equal(knownReason("never_list"), "never_list");
  assert.equal(knownReason("something_new"), null);
  assert.equal(knownReason(null), null);
  const codes = new Set(scan().map((i) => i.reason));
  for (const c of codes) assert.ok(knownReason(c), c);
});

// --- persona append ----------------------------------------------------------------------

test("persona text is added after two line breaks; an empty persona just takes it", () => {
  assert.deepEqual(appendPersona("Be kind.", "Be brief."), { ok: true, value: "Be kind.\n\nBe brief." });
  assert.deepEqual(appendPersona("", "Be brief."), { ok: true, value: "Be brief." });
  assert.deepEqual(appendPersona("  \n", "Be brief."), { ok: true, value: "Be brief." });
  assert.deepEqual(appendPersona("Be kind.", "  \n"), { ok: false, reason: "empty" });
});

test("persona limit is 8000 characters (code points), refused rather than cut", () => {
  const old = "a".repeat(100);
  const fits = "b".repeat(PERSONA_MAX_CHARS - 102);
  const ok = appendPersona(old, fits);
  assert.equal(ok.ok, true);
  assert.equal(ok.ok && charCount(ok.value), PERSONA_MAX_CHARS);
  assert.deepEqual(appendPersona(old, `${fits}b`), { ok: false, reason: "too_long" });
  // Emoji are one character each, as the daemon counts them.
  assert.equal(charCount("😀😀"), 2);
  assert.equal(appendPersona("", "😀".repeat(PERSONA_MAX_CHARS)).ok, true);
  assert.equal(appendPersona("", "😀".repeat(PERSONA_MAX_CHARS + 1)).ok, false);
});

function fakeBot(id: string, persona: string, version = 1): Bot {
  return { id, name: id, version, config: { bot: { name: id, persona } } } as unknown as Bot;
}

/** The daemon's PATCH semantics: a stale expected_version is a 409 carrying the current bot. */
function fakeDaemon(initial: Bot, opts: { bumpOnce?: boolean } = {}) {
  let bot = initial;
  let bump = opts.bumpOnce ?? false;
  const patches: unknown[] = [];
  return {
    patches,
    get bot() {
      return bot;
    },
    client: {
      async getBot() {
        return bot;
      },
      async patchBot(_id: string, input: { expected_version: number; ops?: Array<{ op: "set"; path: string; value: unknown }> }) {
        patches.push(input);
        if (bump) {
          // Someone else saved in between.
          bump = false;
          bot = { ...bot, version: bot.version + 1, config: applyPatchOps(bot.config, [{ op: "set", path: "/bot/persona", value: `${(bot.config as { bot: { persona: string } }).bot.persona} (edited)` }]) } as Bot;
        }
        if (input.expected_version !== bot.version) {
          throw new ApiError(409, "version_conflict", "stale", { current: bot });
        }
        bot = { ...bot, version: bot.version + 1, config: applyPatchOps(bot.config, input.ops ?? []) } as Bot;
        return bot;
      }
    }
  };
}

const persona = (b: Bot) => (b.config as { bot: { persona: string } }).bot.persona;

test("appendToBotPersona patches only /bot/persona with the loaded version", async () => {
  const d = fakeDaemon(fakeBot("b1", "Be kind."));
  const out = await appendToBotPersona(d.client, d.bot, "Be brief.");
  assert.equal(out.kind, "saved");
  assert.equal(persona(d.bot), "Be kind.\n\nBe brief.");
  assert.deepEqual(d.patches, [
    { expected_version: 1, ops: [{ op: "set", path: "/bot/persona", value: "Be kind.\n\nBe brief." }] }
  ]);
});

test("appendToBotPersona retries once on a version conflict, appending to the fresh persona", async () => {
  const d = fakeDaemon(fakeBot("b1", "Be kind."), { bumpOnce: true });
  const out = await appendToBotPersona(d.client, fakeBot("b1", "Be kind."), "Be brief.");
  assert.equal(out.kind, "saved");
  assert.equal(persona(d.bot), "Be kind. (edited)\n\nBe brief.");
  assert.equal(d.patches.length, 2);
});

test("appendToBotPersona does not write when the text would not fit", async () => {
  const d = fakeDaemon(fakeBot("b1", "x".repeat(7990)));
  const out = await appendToBotPersona(d.client, d.bot, "y".repeat(50));
  assert.deepEqual(out, { kind: "too_long" });
  assert.equal(d.patches.length, 0);
});

test("instruction results: the text goes to the chosen bot; too long and missing text are failures", async () => {
  const items = scan();
  const claude = byName(items, "CLAUDE.md");
  const hermes = byName(items, "AGENTS.md");
  const github = byName(items, "github");
  const d = fakeDaemon(fakeBot("b1", "x".repeat(7990)));
  const results: ImportResult[] = [
    { id: github.id, status: "imported", message: "ok", secrets_stored: [], secrets_left_out: [] },
    { id: claude.id, status: "imported", message: "ok", secrets_stored: [], secrets_left_out: [], text: "y".repeat(50) },
    { id: hermes.id, status: "imported", message: "ok", secrets_stored: [], secrets_left_out: [], text: "short" }
  ];
  const { outcomes } = await applyInstructionResults(
    d.client,
    items,
    results,
    { [claude.id]: "b1", [hermes.id]: "b1" },
    [d.bot],
    (b) => `Bot ${b.id}`
  );
  assert.equal(outcomes[0].status, "imported");
  assert.equal(outcomes[0].local, undefined);
  assert.equal(outcomes[1].status, "failed");
  assert.equal(outcomes[1].local, "persona_too_long");
  assert.equal(outcomes[1].botName, "Bot b1");
  // The failure of one item does not stop the next one.
  assert.equal(outcomes[2].status, "imported");
  assert.equal(outcomes[2].local, "persona_saved");
  assert.equal(persona(d.bot), `${"x".repeat(7990)}\n\nshort`);

  const none = await applyInstructionResults(d.client, items, [{ ...results[1], text: null }], { [claude.id]: "b1" }, [d.bot]);
  assert.equal(none.outcomes[0].local, "persona_failed");
  const noBot = await applyInstructionResults(d.client, items, [results[2]], {}, [d.bot]);
  assert.equal(noBot.outcomes[0].status, "failed");
});

test("two instruction items for one bot are appended in order", async () => {
  const items = scan();
  const a = byName(items, "CLAUDE.md");
  const b = byName(items, "AGENTS.md");
  const d = fakeDaemon(fakeBot("b1", "Start."));
  const mk = (id: string, text: string): ImportResult => ({ id, status: "imported", message: "", secrets_stored: [], secrets_left_out: [], text });
  const { outcomes } = await applyInstructionResults(d.client, items, [mk(a.id, "One."), mk(b.id, "Two.")], { [a.id]: "b1", [b.id]: "b1" }, [d.bot]);
  assert.deepEqual(outcomes.map((o) => o.local), ["persona_saved", "persona_saved"]);
  assert.equal(persona(d.bot), "Start.\n\nOne.\n\nTwo.");
});

// --- results ----------------------------------------------------------------------------------

test("the mock daemon leaves out unconfirmed secrets and reports them by key", () => {
  const items = scan();
  const ids = [byName(items, "github").id, byName(items, "docs").id, byName(items, "memory").id, byName(items, "deploy-helper").id];
  const { results } = mockImportRun({ ids, confirmed_secrets: ["mcp_github_GITHUB_TOKEN"] });
  assert.deepEqual(results.map((r) => r.status), ["imported", "imported", "skipped", "failed"]);
  assert.deepEqual(results[0].secrets_stored, ["mcp_github_GITHUB_TOKEN"]);
  assert.deepEqual(results[1].secrets_left_out, ["Authorization"]);
  assert.deepEqual(tally(results), { imported: 2, skipped: 1, failed: 1 });
  assert.equal(importedServers(items, results), true);
  assert.equal(importedServers(items, results.filter((r) => r.id !== ids[0] && r.id !== ids[1])), false);
});

test("every reason code the contract names has words in every strict locale", () => {
  const codes = [
    "ready", "ready_with_secrets", "already_have", "duplicate", "unpinned", "never_list", "unsupported",
    "invalid", "unreadable", "secret_in_command", "variables", "review_first", "too_long", "never_copied", "not_imported",
    "not_found", "failed", "secret_failed"
  ];
  for (const locale of ["en", "tr", "de", "es"]) {
    const json = JSON.parse(readFileSync(new URL(`../i18n/locales/${locale}.json`, import.meta.url), "utf8")) as {
      importAssistants: { reason: Record<string, string> };
    };
    for (const code of codes) {
      assert.equal(typeof json.importAssistants.reason[code], "string", `${locale}: reason.${code}`);
      assert.ok(knownReason(code), code);
    }
  }
});
