import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { redactDetails } from "./redactDetails.ts";

const SRC = fileURLToPath(new URL("..", import.meta.url));

function sources(dir = SRC, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) sources(p, out);
    else if (/\.tsx?$/.test(name) && !/\.test\.tsx?$/.test(name)) out.push(p);
  }
  return out;
}

/** Ways to put a raw exception text, stack or console dump in front of a person. */
const FORBIDDEN: ReadonlyArray<readonly [string, RegExp]> = [
  ["JSX renders a raw error message", /\{\s*(?:e|err|error|[a-z]+Error)\.(?:message|stack)\s*\}/],
  ["JSX renders String(err)", /\{\s*String\(\s*(?:e|err|error)\s*\)\s*\}/],
  ["toast shows a raw error text", /toast\.show\(\s*(?:e|err|error)(?:\.message|\.stack)?\s*[,)]/],
  ["toast shows String(err) or err.message", /toast\.show\([^;\n]*(?:String\(\s*(?:e|err|error)\s*\)|\b(?:e|err|error)\.(?:message|stack))/],
  ["state setter stores a raw error text", /set(?:Error|Problem|Notice|Message)\(\s*(?:e|err|error)(?:\.message)?\s*\)/],
  ["a stack trace is read", /\b(?:e|err|error)\.stack\b/],
  ["console output meant for the user", /\bconsole\.error\(/],
  ["native alert dialog", /\balert\(/]
];

/**
 * Places that keep the raw text on purpose. Each is stored for a "Details" disclosure or a
 * classifier and goes through `humanError` / `humanService` (redacted) before it is shown.
 */
const ALLOWED = new Set(["state/DaemonContext.tsx"]);

test("no source file renders a raw error message or stack to the user", () => {
  const hits: string[] = [];
  for (const file of sources()) {
    const rel = relative(SRC, file).split("\\").join("/");
    if (ALLOWED.has(rel)) continue;
    readFileSync(file, "utf8")
      .split("\n")
      .forEach((line, i) => {
        if (/^\s*(\/\/|\*|\/\*)/.test(line)) return;
        for (const [why, re] of FORBIDDEN) if (re.test(line)) hits.push(`${rel}:${i + 1}: ${why}: ${line.trim()}`);
      });
  }
  assert.deepEqual(hits, []);
});

test("details are redacted: tokens, token URLs, profile paths and stack frames", () => {
  const raw = [
    "GET https://api.example.com/v1/x?api_key=sk-abc123&page=2 failed",
    "Authorization: Bearer abcdef0123456789",
    "https://user:hunter2@host.example/path",
    "could not open C:\\Users\\Emre Ocel\\AppData\\Roaming\\praxi\\db and /home/emre/.config/praxi",
    "    at load (file:///C:/app/main.js:10:5)",
    "ok line"
  ].join("\n");
  const out = redactDetails(raw);
  for (const leak of ["sk-abc123", "abcdef0123456789", "hunter2", "Emre", "/home/emre", "main.js"]) {
    assert.ok(!out.includes(leak), `leaked ${leak}: ${out}`);
  }
  assert.ok(out.includes("page=2") && out.includes("ok line"), out);
  assert.ok(out.includes("~"), out);
});
