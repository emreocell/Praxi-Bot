import assert from "node:assert/strict";
import { test } from "node:test";
import type { McpCatalogEntry, McpServer, McpServerConfig } from "../api/types.ts";
import {
  GOOGLE_SECRET_NAME,
  GoogleClientError,
  GOOGLE_STEPS,
  addGoogleApp,
  checkGoogleClient,
  googleClientIdOf,
  googleEntries,
  googleScopes,
  googleServerConfig,
  googleServerOf,
  googleServices,
  isGoogleConsoleLink,
  testFailure
} from "./googleSetup.ts";

const G = "https://www.googleapis.com/auth/";
function entry(id: string, scopes: string[], over: Partial<McpCatalogEntry["install"]> = {}): McpCatalogEntry {
  return {
    id,
    name: id,
    description: "",
    category: "accounts",
    homepage: "https://example.com",
    icon: null,
    install: {
      transport: "http",
      url: `https://${id}.example/mcp/v1`,
      required_secrets: [],
      tool_allow: ["search_*"],
      oauth_client: { issuer: "https://accounts.google.com", scopes: scopes.map((s) => G + s), offline_access: true, setup: "google" },
      ...over
    }
  } as McpCatalogEntry;
}
const gmail = entry("google-gmail", ["gmail.readonly"]);
const drive = entry("google-drive", ["drive.readonly"]);
const calendar = entry("google-calendar", ["calendar.events.readonly", "calendar.calendarlist.readonly"]);
const other = { ...entry("linear", []), install: { transport: "http", url: "https://l", required_secrets: [] } } as McpCatalogEntry;

const CLIENT_ID = "123456789012-abc123def456.apps.googleusercontent.com";
const SECRET = "GOCSPX-abcdefghijk";

function server(name: string, over: Partial<McpServer> = {}): McpServer {
  return { name, transport: "http", status: "connected", ...over } as McpServer;
}

test("only the bring-your-own-client entries are Google apps", () => {
  assert.deepEqual(googleEntries([gmail, other, drive]).map((e) => e.id), ["google-gmail", "google-drive"]);
});

test("the client is checked without echoing it", () => {
  assert.equal(checkGoogleClient(CLIENT_ID, SECRET), null);
  assert.equal(checkGoogleClient(" " + CLIENT_ID + " ", " " + SECRET + " "), null);
  assert.equal(checkGoogleClient("not-an-id", SECRET), "client_id");
  assert.equal(checkGoogleClient("123-abc.example.com", SECRET), "client_id");
  assert.equal(checkGoogleClient(CLIENT_ID, "short"), "client_secret");
  assert.equal(checkGoogleClient(CLIENT_ID, "has a space inside"), "client_secret");
});

test("the server settings carry the read-only scopes and the secret's NAME, never a value", () => {
  const config = googleServerConfig(gmail, CLIENT_ID);
  assert.equal(config.name, "google-gmail");
  assert.equal(config.url, "https://google-gmail.example/mcp/v1");
  assert.deepEqual(config.tool_allow, ["search_*"]);
  assert.deepEqual(config.oauth, {
    client_id: CLIENT_ID,
    client_secret_ref: GOOGLE_SECRET_NAME,
    issuer: "https://accounts.google.com",
    scopes: [G + "gmail.readonly"],
    offline_access: true
  });
  assert.ok(!JSON.stringify(config).includes(SECRET));
  assert.throws(() => googleServerConfig(other, CLIENT_ID));
});

test("a typed client is stored write-only first, then the app is added", async () => {
  const order: string[] = [];
  const api = {
    setSecret: async (name: string, value: string) => {
      order.push(`secret:${name}:${value}`);
    },
    createMcpServer: async (config: McpServerConfig) => {
      order.push(`create:${config.name}`);
      return server(config.name);
    },
    updateMcpServer: async () => assert.fail("nothing to update")
  };
  await addGoogleApp(api, gmail, { clientId: CLIENT_ID, clientSecret: ` ${SECRET} ` }, []);
  assert.deepEqual(order, [`secret:${GOOGLE_SECRET_NAME}:${SECRET}`, "create:google-gmail"]);
});

test("a bad client stores nothing", async () => {
  let touched = 0;
  const api = {
    setSecret: async () => {
      touched += 1;
    },
    createMcpServer: async (c: McpServerConfig) => {
      touched += 1;
      return server(c.name);
    },
    updateMcpServer: async () => {
      touched += 1;
      return server("x");
    }
  };
  await assert.rejects(
    () => addGoogleApp(api, gmail, { clientId: "nope", clientSecret: SECRET }, []),
    (e: unknown) => e instanceof GoogleClientError && e.field === "client_id"
  );
  await assert.rejects(
    () => addGoogleApp(api, gmail, null, []),
    (e: unknown) => e instanceof GoogleClientError && e.field === "missing"
  );
  assert.equal(touched, 0);
});

test("the second Google app reuses the first one's client without typing", async () => {
  const have = [server("google-gmail", { oauth: { client_id: CLIENT_ID, issuer: "https://accounts.google.com/" } })];
  assert.equal(googleClientIdOf(have), CLIENT_ID);
  assert.equal(googleClientIdOf([server("linear")]), null);
  assert.equal(googleClientIdOf([server("x", { oauth: { client_id: "c", issuer: "https://other.example" } })]), null);
  const created: unknown[] = [];
  const api = {
    setSecret: async () => assert.fail("no secret is typed again"),
    createMcpServer: async (c: McpServerConfig) => {
      created.push(c.oauth?.client_id);
      return server("google-drive");
    },
    updateMcpServer: async () => assert.fail("nothing to update")
  };
  await addGoogleApp(api, drive, null, have);
  assert.deepEqual(created, [CLIENT_ID]);
});

test("a new client re-keys the apps added earlier, and a leftover try is updated, not duplicated", async () => {
  const NEW_ID = "999999999999-newclient.apps.googleusercontent.com";
  const have = [
    server("google-gmail", { oauth: { client_id: CLIENT_ID, issuer: "https://accounts.google.com" } }),
    server("linear")
  ];
  const updates: [string, unknown][] = [];
  const api = {
    setSecret: async () => undefined,
    createMcpServer: async () => {
      throw Object.assign(new Error("exists"), { status: 409 });
    },
    updateMcpServer: async (name: string, patch: { oauth?: unknown }) => {
      updates.push([name, patch.oauth]);
      return server(name);
    }
  };
  await addGoogleApp(api, drive, { clientId: NEW_ID, clientSecret: SECRET }, have, [gmail, drive, calendar]);
  assert.deepEqual(updates.map(([n]) => n), ["google-gmail", "google-drive"]);
  for (const [, oauth] of updates) {
    assert.equal((oauth as { client_id: string }).client_id, NEW_ID);
    assert.equal((oauth as { client_secret_ref: string }).client_secret_ref, GOOGLE_SECRET_NAME);
  }
  // Another failure is not swallowed as "already there".
  const failing = { ...api, createMcpServer: async () => { throw Object.assign(new Error("boom"), { status: 500 }); } };
  await assert.rejects(() => addGoogleApp(failing, drive, null, have, [gmail, drive]), /boom/);
});

test("an old archived stdio server with the same name blocks the add and is never touched", async () => {
  let touched = 0;
  const api = {
    setSecret: async () => { touched += 1; },
    createMcpServer: async () => { touched += 1; return server("x"); },
    updateMcpServer: async () => { touched += 1; return server("x"); }
  };
  await assert.rejects(
    () => addGoogleApp(api, drive, { clientId: CLIENT_ID, clientSecret: SECRET }, [server("google-drive", { transport: "stdio" })]),
    (e: unknown) => e instanceof GoogleClientError && e.field === "legacy"
  );
  assert.equal(touched, 0);
});

test("an old stdio server with the same name is not the Google app", () => {
  const legacy = server("google-drive", { transport: "stdio" });
  assert.equal(googleServerOf(drive, [legacy]), undefined);
  assert.equal(googleServerOf(drive, [server("google-drive")])?.name, "google-drive");
});

test("the guide lists the console pages and the exact services and scopes of the chosen apps", () => {
  for (const step of GOOGLE_STEPS) {
    for (const link of step.links) assert.match(link.href, /^https:\/\/console\.cloud\.google\.com\//);
  }
  assert.deepEqual(GOOGLE_STEPS.map((s) => s.id), ["project", "apis", "branding", "audience", "scopes", "client"]);
  assert.deepEqual(googleServices([gmail, drive]).map((s) => s.id), [
    "gmail.googleapis.com",
    "gmailmcp.googleapis.com",
    "drive.googleapis.com",
    "drivemcp.googleapis.com"
  ]);
  assert.equal(googleServices([drive])[0].href, "https://console.cloud.google.com/apis/library/drive.googleapis.com");
  assert.deepEqual(googleScopes([gmail, calendar, gmail]), [
    G + "gmail.readonly",
    G + "calendar.events.readonly",
    G + "calendar.calendarlist.readonly"
  ]);
  // Read-only first: nothing in the guide asks for a scope that changes things.
  assert.ok(googleScopes([gmail, drive, calendar]).every((s) => /\.(readonly)$/.test(s)));
});

test("the test button's failures map to a plain reason", () => {
  const e = (message: string, code = "bad_mcp_config") => Object.assign(new Error(message), { code });
  assert.equal(testFailure(e("x", "host_admin_required")), "admin_needed");
  assert.equal(testFailure(e("the app is not connected")), "not_connected");
  assert.equal(testFailure(e("this app has no test")), "no_test");
  assert.equal(testFailure(e("authorization is required")), "signed_out");
  assert.equal(testFailure(e("HTTP 403 forbidden")), "permission");
  assert.equal(testFailure(e("HTTP 429 too many")), "rate");
  assert.equal(testFailure(e("boom")), "other");
});

test("the setup guide opens pages of the Google Cloud console and nothing else", () => {
  for (const step of GOOGLE_STEPS) {
    for (const l of step.links) assert.ok(isGoogleConsoleLink(l.href), l.href);
  }
  for (const l of googleServices([entry("google-gmail", [])])) assert.ok(isGoogleConsoleLink(l.href), l.href);
  for (const bad of [
    "http://console.cloud.google.com/",
    "https://console.cloud.google.com.evil.example/",
    "https://evil.example/console.cloud.google.com",
    "https://user@console.cloud.google.com/",
    "https://accounts.google.com/",
    "https://console.cloud.google.com:8443/",
    "javascript:alert(1)",
    ""
  ]) {
    assert.equal(isGoogleConsoleLink(bad), false, bad);
  }
});
