import assert from "node:assert/strict";
import { test } from "node:test";
import {
  type CustomMcpDraft,
  customSecretName,
  draftPlan,
  emptyDraft,
  isDraftValid,
  suggestName,
  validateDraft
} from "./customMcp.ts";

const draft = (patch: Partial<CustomMcpDraft>): CustomMcpDraft => ({ ...emptyDraft(), ...patch });

test("the URL path is the default, with no sign-in and the usual header name", () => {
  const d = emptyDraft();
  assert.equal(d.mode, "http");
  assert.equal(d.auth, "none");
  assert.equal(d.headerName, "Authorization");
});

test("a name is suggested from the address", () => {
  assert.equal(suggestName("https://mcp.notion.com/mcp"), "notion");
  assert.equal(suggestName("https://api.githubcopilot.com/mcp/"), "githubcopilot");
  assert.equal(suggestName("https://tools.example.co.uk/sse"), "example");
  assert.equal(suggestName("http://localhost:8931/mcp"), "localhost-8931");
  assert.equal(suggestName("http://127.0.0.1:3000"), "127-0-0-1-3000");
  assert.equal(suggestName("https://mcp.notion.com/mcp", ["notion"]), "notion-2");
  assert.equal(suggestName("not a url"), "");
});

test("inline validation names the field and the reason", () => {
  const empty = validateDraft(emptyDraft(), []);
  assert.deepEqual(empty.errors, { name: "nameMissing", url: "urlMissing" });
  assert.equal(isDraftValid(empty), false);

  const ok = validateDraft(draft({ url: "https://x.example/mcp", name: "x" }), []);
  assert.deepEqual(ok, { errors: {}, warnings: {} });
  assert.equal(validateDraft(draft({ url: "ftp://x", name: "x" }), []).errors.url, "urlInvalid");
  assert.equal(validateDraft(draft({ url: "https://user:pw@x.example", name: "x" }), []).errors.url, "urlCredentials");
  assert.equal(validateDraft(draft({ url: "https://x.example", name: "bad name" }), []).errors.name, "nameFormat");
  assert.equal(validateDraft(draft({ url: "https://x.example", name: "x" }), ["x"]).errors.name, "nameTaken");
  // Plain http is accepted, with a word, except on this computer.
  assert.deepEqual(validateDraft(draft({ url: "http://x.example", name: "x" }), []).warnings, { url: "urlPlain" });
  assert.deepEqual(validateDraft(draft({ url: "http://localhost:3000", name: "x" }), []).warnings, {});

  const header = validateDraft(draft({ url: "https://x.example", name: "x", auth: "header", headerName: "Bad Header" }), []);
  assert.deepEqual(header.errors, { headerName: "headerName", headerValue: "headerValue" });

  const stdio = validateDraft(draft({ mode: "stdio", name: "x", env: "OK=1\nnot a line" }), []);
  assert.deepEqual(stdio.errors, { command: "commandMissing", env: "envLine" });
  // The URL is not asked for on the command path.
  assert.equal(stdio.errors.url, undefined);
});

test("a header key goes to the secret store; the server is saved off and only names it", () => {
  const plan = draftPlan(
    draft({ url: "https://x.example/mcp", name: "mine", auth: "header", headerValue: "  Bearer abc  " }),
    [],
    { mode: "all" },
    false
  );
  assert.ok(plan);
  assert.equal(plan.config.enabled, false);
  assert.equal(plan.config.transport, "http");
  assert.equal(plan.config.url, "https://x.example/mcp");
  assert.deepEqual(plan.secrets, [{ name: "mcp/mine/header/Authorization", value: "Bearer abc" }]);
  assert.deepEqual(plan.config.secret_headers, { Authorization: "mcp/mine/header/Authorization" });
  assert.equal(JSON.stringify(plan.config).includes("abc"), false, "the key never reaches the settings");
  assert.equal("headers" in plan.config, false);
  assert.equal("trust_hints" in plan.config, false, "the safe answer stays the default");
  assert.equal(plan.signIn, false);
});

test("browser sign-in needs no secret and asks for a sign-in after saving", () => {
  const plan = draftPlan(draft({ url: "https://x.example/mcp", name: "mine", auth: "oauth" }), [], { mode: "all" }, true);
  assert.ok(plan);
  assert.deepEqual(plan.secrets, []);
  assert.equal(plan.signIn, true);
  assert.equal(plan.config.trust_hints, true);
  assert.equal(plan.config.secret_headers, undefined);
});

test("a local command keeps plain variables and moves keys to the secret store", () => {
  const plan = draftPlan(
    draft({ mode: "stdio", name: "local", command: 'npx -y "@scope/server"', env: "LOG_LEVEL=debug\nGITHUB_TOKEN=ghp_x\nAPI_KEY=\nDB_PASSWORD=p w" }),
    ["other"],
    { mode: "some", ids: ["b1"] },
    false
  );
  assert.ok(plan);
  assert.equal(plan.config.enabled, false);
  assert.equal(plan.config.command, "npx");
  assert.deepEqual(plan.config.args, ["-y", "@scope/server"]);
  assert.deepEqual(plan.config.env, { LOG_LEVEL: "debug", API_KEY: "" });
  assert.deepEqual(plan.config.secret_env, {
    GITHUB_TOKEN: "mcp/local/env/GITHUB_TOKEN",
    DB_PASSWORD: "mcp/local/env/DB_PASSWORD"
  });
  assert.deepEqual(plan.secrets.map((s) => s.name), ["mcp/local/env/GITHUB_TOKEN", "mcp/local/env/DB_PASSWORD"]);
  assert.deepEqual(plan.config.bots, ["b1"]);
  assert.equal(plan.config.url, undefined);
});

test("names differing only in case are one name (the Windows credential store ignores case)", () => {
  assert.equal(validateDraft(draft({ url: "https://x.example", name: "Linear" }), ["linear"]).errors.name, "nameTaken");
  assert.equal(suggestName("https://mcp.linear.app/mcp", ["LINEAR"]), "linear-2");
});

test("an upper-case scheme is saved lower-case, as the daemon requires", () => {
  const plan = draftPlan(draft({ url: " HTTPS://X.example/MCP ", name: "x" }), [], { mode: "all" }, false);
  assert.equal(plan?.config.url, "https://X.example/MCP");
});

test("a key left in the address is pointed out; passwords in values and *_PAT names go to the store", () => {
  assert.deepEqual(validateDraft(draft({ url: "https://x.example/mcp?api_key=abc", name: "x" }), []).warnings, { url: "urlKey" });
  assert.deepEqual(validateDraft(draft({ url: "https://x.example/mcp?page=2", name: "x" }), []).warnings, {});
  const plan = draftPlan(
    draft({ mode: "stdio", name: "db", command: "srv", env: "DATABASE_URL=postgres://u:pw@h/db\nGITHUB_PAT=abc\nPATH_EXTRA=/x\nHOME_URL=https://example.com" }),
    [],
    { mode: "all" },
    false
  );
  assert.ok(plan);
  assert.deepEqual(Object.keys(plan.config.secret_env ?? {}), ["DATABASE_URL", "GITHUB_PAT"]);
  assert.deepEqual(plan.config.env, { PATH_EXTRA: "/x", HOME_URL: "https://example.com" });
});

test("an invalid draft has no plan, and secret names stay inside the store's alphabet", () => {
  assert.equal(draftPlan(emptyDraft(), [], { mode: "all" }, false), null);
  assert.equal(customSecretName("s", "header", "X-Api~Key"), "mcp/s/header/X-Api-Key");
  assert.match(customSecretName("a".repeat(48), "env", "K".repeat(64)), /^[A-Za-z0-9._:/-]{1,128}$/);
});
