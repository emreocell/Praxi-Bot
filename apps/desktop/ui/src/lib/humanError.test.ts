import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
import type { i18n as I18n } from "i18next";
import { createServer, type ViteDevServer } from "vite";

type Layer = typeof import("./humanError.ts");
type FirstRun = typeof import("./firstRun.ts");

let server: ViteDevServer;
let layer: Layer;
let firstRun: FirstRun;
let i18n: I18n;

before(async () => {
  server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null }
  });
  ({ i18n } = await server.ssrLoadModule("/src/i18n/index.ts"));
  layer = (await server.ssrLoadModule("/src/lib/humanError.ts")) as Layer;
  firstRun = (await server.ssrLoadModule("/src/lib/firstRun.ts")) as FirstRun;
});

after(async () => {
  await server?.close();
});

/** Every `"snake_code"` the Rust crates put next to an HTTP status in an error body. */
function rustCodes(): Set<string> {
  const root = fileURLToPath(new URL("../../../../../crates", import.meta.url));
  const found = new Set<string>();
  const walk = (dir: string) => {
    for (const name of readdirSync(dir)) {
      const p = join(dir, name);
      if (statSync(p).isDirectory()) {
        walk(p);
      } else if (name.endsWith(".rs")) {
        for (const m of readFileSync(p, "utf8").matchAll(/StatusCode::[A-Z_0-9]+\s*,\s*"([a-z][a-z_0-9]*)"/g)) {
          found.add(m[1]);
        }
      }
    }
  };
  walk(root);
  return found;
}

test("every daemon error code has a title and a hint in both languages", async () => {
  const known = new Set<string>(layer.DAEMON_ERROR_CODES);
  for (const code of rustCodes()) {
    assert.ok(known.has(code), `the daemon can emit "${code}" but the UI has no text for it`);
  }
  const english = new Map<string, { title: string; hint: string }>();
  for (const lng of ["en", "tr", "de"]) {
    await i18n.changeLanguage(lng);
    for (const code of layer.DAEMON_ERROR_CODES) {
      const h = layer.humanError({ status: 409, code, message: "raw daemon text" });
      assert.equal(h.code, code);
      assert.ok(h.title && !h.title.startsWith("errors."), `${lng}/${code} title`);
      assert.ok(h.hint && !h.hint.startsWith("errors."), `${lng}/${code} hint`);
      assert.ok(!h.title.includes("raw daemon text"));
      // No dead end: the hint says what to do next, in a real sentence of its own.
      assert.ok(h.hint.length >= 12 && h.hint !== h.title, `${lng}/${code} hint must tell what to do`);
      if (lng === "en") {
        english.set(code, h);
      } else {
        // Not left in English.
        assert.notEqual(h.hint, english.get(code)!.hint, `${lng}/${code} hint is still English`);
      }
    }
  }
});

test("daemon error codes stay sorted and unique (keeps parallel branches from conflicting)", () => {
  const codes = [...layer.DAEMON_ERROR_CODES];
  assert.deepEqual(codes, [...new Set(codes)].sort());
});

test("first-run provider failures distinguish credentials, network, endpoint and model", async () => {
  assert.equal(firstRun.classifyConnectError("authentication failed: test credential rejected", "openai"), "badKey");
  assert.equal(firstRun.classifyConnectError("connection error: error sending request", "openai"), "offline");
  assert.equal(firstRun.classifyConnectError("request timed out", "ollama"), "ollamaDown");
  assert.equal(firstRun.classifyConnectError("HTTP 404", null), "unknown");

  const cases = [
    ["provider error (404): no route for /v1/models", "provider_endpoint"],
    ["connection error: relative URL without a base", "provider_endpoint"],
    ['provider error (404): {"code":"model_not_found","message":"fixture model is unavailable"}', "provider_model"]
  ] as const;
  for (const lng of ["en", "tr"]) {
    await i18n.changeLanguage(lng);
    for (const [raw, problem] of cases) {
      assert.equal(firstRun.classifyConnectError(raw, "openai"), problem);
      const error = layer.humanProviderTestError(problem);
      assert.equal(error.code, problem);
      assert.ok(error.title && !error.title.startsWith("errors."));
      assert.ok(error.hint && !error.hint.startsWith("errors."));
      assert.ok(!error.title.includes(raw) && !error.hint.includes(raw));
      assert.equal(error.details, "");
    }
  }
});

test("unknown codes and raw strings fall back to a human sentence; raw text only in details", async () => {
  await i18n.changeLanguage("en");
  const unknown = layer.humanError({ status: 0, code: "brand_new_code", message: "kaboom: stack at foo.rs:12" });
  assert.equal(unknown.code, "generic");
  assert.ok(!unknown.title.includes("kaboom") && !unknown.hint.includes("kaboom"));
  assert.match(unknown.details, /kaboom/);

  const control = layer.humanToolError(
    "Error: execution error: The user is controlling the screen right now (observation paused), so this read was not performed"
  );
  assert.match(control.title, /could not look at the screen/i);
  const uv = layer.humanToolError("cannot start MCP server `sqlite`: `uvx` was not found on PATH. Install uv");
  assert.match(uv.title, /uvx is not installed/);

  // A paired host's refusal: one sentence naming the server (the former client-side formatter, now here).
  for (const lng of ["en", "tr"]) {
    await i18n.changeLanguage(lng);
    const refused = layer.humanError({ status: 403, code: "forbidden", host: "Praxi host", message: "the execution policy is managed on the host itself" });
    assert.equal(refused.code, "host_refusal");
    assert.match(refused.title, /Praxi host/);
    assert.doesNotMatch(refused.title, /execution policy is managed/);
  }

  await i18n.changeLanguage("tr");
  assert.doesNotMatch(layer.humanError({ status: 409, code: "version_conflict", message: "PUT requires expected_version" }).title, /expected_version/);
  await i18n.changeLanguage("en");
});

test("a tool failure that names the wrong machine or an offline target says so", async () => {
  await i18n.changeLanguage("tr");
  const wrong = layer.humanToolError('wrong computer: "/opt/praxi-browser/chrome" looks like a Linux or macOS program or path, but this desktop is a Windows computer.');
  assert.equal(wrong.code, "toolWrongMachine");
  assert.match(wrong.title, /yanlış bilgisayar/);
  assert.equal(layer.humanToolError("PC desktop unavailable: peer networking is disabled").code, "toolTargetOffline");
  assert.notEqual(layer.humanToolError("something odd happened").code, "toolWrongMachine");
});

test("a service that does not start is worded by its cause, with the exit code and log in the details", async () => {
  const base: Omit<import("../api/connection.ts").StartupDiagnostics, "stage" | "exit_code"> & { exit_code: number | null } = {
    elapsed_ms: 21_000, port: 47821, base_url: "http://127.0.0.1:47821", data_dir: "C:/Users/x/data",
    log_path: "C:/Users/x/data/desktop-daemon.log", sidecar: "C:/Program Files/Praxi/bot.exe", sidecar_exists: true,
    pid: null, exit_code: null, error: null, log_tail: ["startup: web tools registered (+40 ms)"], app_version: "0.1.0", os: "windows x86_64"
  };
  for (const lng of ["en", "tr"]) {
    await i18n.changeLanguage(lng);
    const dll = layer.humanStartup({ ...base, stage: "exited", exit_code: -1073741515 });
    assert.equal(dll.code, "startup.exitedDll");
    assert.match(dll.title, /0xC0000135/);
    assert.match(dll.details, /exit code: -1073741515 \(0xC0000135\)/);
    assert.match(dll.details, /web tools registered/);
    const stages = ["spawning", "waiting", "sidecar_missing", "sidecar_blocked", "spawn_failed", "exited", "not_responding", "port_in_use", "unreachable"] as const;
    for (const stage of stages) {
      const h = layer.humanStartup({ ...base, stage });
      assert.ok(h.title && !h.title.startsWith("errors."), `${lng}/${stage} title`);
      assert.ok(h.hint && !h.hint.startsWith("errors."), `${lng}/${stage} hint`);
    }
    assert.match(layer.humanStartup({ ...base, stage: "port_in_use" }).title, /47821/);
  }
  await i18n.changeLanguage("en");
});

test("takeover and pause failures name their real reason, never the generic sentence", async () => {
  for (const lng of ["en", "tr", "de"]) {
    await i18n.changeLanguage(lng);
    const generic = i18n.t("liveView.errors.generic");
    const cases: Array<[Record<string, unknown>, string]> = [
      [{ status: 403, code: "forbidden", host: "Praxi host", message: "only the local user may do this" }, "host_refusal"],
      [{ status: 403, code: "forbidden", host: "Praxi host", message: "some brand new refusal" }, "host_refusal"],
      [{ status: 409, code: "conflict", message: "this screen target has no input backend, so it cannot be taken over" }, "noInputBackend"],
      [{ status: 409, code: "conflict", message: "workflows are disabled in safe mode" }, "safeModeBlocked"],
      [{ status: 503, code: "unavailable", message: "the executor is busy; try again" }, "hostBusy"],
      [{ status: 503, code: "unavailable", message: "the display target did not confirm the key reset; the latch is kept, try again" }, "screenNoConfirm"],
      [{ status: 504, code: "timeout", host: "Praxi host", message: "gateway timeout" }, "host_silent"],
      [{ status: 0, code: "network", host: "Praxi host", message: "fetch failed" }, "host_silent"]
    ];
    for (const [err, code] of cases) {
      const h = layer.humanLine(err);
      assert.equal(layer.humanError(err).code, code, `${lng} ${JSON.stringify(err)}`);
      assert.notEqual(h, generic);
      assert.ok(h.length > 10);
    }
    // The unknown host refusal keeps the daemon's reason in the sentence.
    assert.match(layer.humanLine(cases[1][0]), /some brand new refusal/);
  }
  await i18n.changeLanguage("en");
});

test("concrete refusal reasons apply only to host refusals or conflict/unavailable answers", async () => {
  await i18n.changeLanguage("en");
  // The same words in a plain tool error or an unrelated code must not be rewritten.
  assert.notEqual(layer.humanError({ message: "the executor is busy; try again" }).code, "hostBusy");
  assert.notEqual(layer.humanError({ status: 500, code: "internal", message: "no input backend" }).code, "noInputBackend");
  assert.equal(layer.humanError({ status: 409, code: "conflict", message: "no input backend" }).code, "noInputBackend");
  assert.equal(layer.humanError({ status: 403, code: "forbidden", host: "h", message: "no input backend" }).code, "host_refusal");
});

test("the live-view error line falls back to the humanized message, not the generic text", async () => {
  const mod = (await server.ssrLoadModule("/src/lib/liveViewError.ts")) as typeof import("./liveViewError.ts");
  await i18n.changeLanguage("en");
  assert.equal(mod.liveViewErrorText({ code: "forbidden", message: "Praxi host did not allow this: x" }), "Praxi host did not allow this: x");
  assert.equal(mod.liveViewErrorText({ code: "pause_changed" }), i18n.t("liveView.errors.pause_changed"));
  assert.equal(mod.liveViewErrorText({ code: "zzz" }), i18n.t("liveView.errors.generic"));
});

test("providerTestProblem distinguishes model, endpoint, length bounds and unrelated errors", () => {
  const longModelErr = `model ${"x".repeat(161)} not found`;
  const cases: Array<[string, "provider_model" | "provider_endpoint" | undefined]> = [
    ["", undefined],
    ["MODEL_NOT_FOUND", "provider_model"],
    ["Unknown model: foo", "provider_model"],
    ["model `gpt-99` does not exist", "provider_model"],
    ["models/gemini-missing is not found", "provider_model"],
    [longModelErr, undefined],
    ["model\nnot found", undefined],
    ["HTTP 404: page missing", "provider_endpoint"],
    ["Provider error (404)", "provider_endpoint"],
    ["invalid base URL: missing host", "provider_endpoint"],
    ["URL scheme is not allowed", "provider_endpoint"],
    ["HTTP 500 internal server error", undefined]
  ];
  for (const [raw, expected] of cases) {
    assert.equal(layer.providerTestProblem(raw), expected, raw);
  }
});

test("exitCodeLabel formats negative NTSTATUS codes in hex and leaves non-negative codes plain", () => {
  const cases: Array<[number, string]> = [
    [0, "0"],
    [1, "1"],
    [137, "137"],
    [-1, "-1 (0xFFFFFFFF)"],
    [-1073741819, "-1073741819 (0xC0000005)"],
    [-1073741515, "-1073741515 (0xC0000135)"]
  ];
  for (const [code, expected] of cases) {
    assert.equal(layer.exitCodeLabel(code), expected, String(code));
  }
});

test("humanError handles non-object inputs, circular objects, status fallbacks and reason precedence", async () => {
  await i18n.changeLanguage("en");
  const circular: Record<string, unknown> = {};
  circular.self = circular;

  assert.equal(layer.humanError(null).code, "generic");
  assert.equal(layer.humanError(undefined).code, "generic");
  assert.equal(layer.humanError(circular).code, "generic");
  assert.equal(layer.humanError(new Error("ECONNREFUSED 127.0.0.1")).code, "network");
  assert.equal(layer.humanError({ status: 413, message: "too big" }).code, "payload_too_large");
  assert.equal(layer.humanError({ status: 429, message: "slow down" }).code, "too_many_requests");
  assert.equal(layer.humanError({ status: 599, message: "bad gateway" }).code, "internal");
  assert.equal(layer.humanError({ status: 418, message: "teapot" }).code, "http_error");
  // Concrete daemon reasons outrank generic HTTP status/code mappings.
  assert.equal(
    layer.humanError({ status: 409, code: "conflict", message: "missing input grant for this device" }).code,
    "screenInputDenied"
  );
  // Missing program pattern interpolates program name from both phrasing variants.
  const missingCmd = layer.humanError("command not found: `ffmpeg`");
  assert.equal(missingCmd.code, "toolMissingProgram");
  assert.match(missingCmd.title, /ffmpeg/);
});
