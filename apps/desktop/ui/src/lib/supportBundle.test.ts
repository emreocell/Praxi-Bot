import assert from "node:assert/strict";
import { test } from "node:test";
import type { Health } from "../api/types.ts";
import { filterLogs, supportFiles, supportZip } from "./supportBundle.ts";

test("search and level filters select exactly the text to copy", () => {
  const lines = ['{"level":"warn","fields":{"status":"409"}}', '{"level":"error","fields":{"status":"503"}}'];
  assert.deepEqual(filterLogs(lines, " 503 ", "error"), [lines[1]]);
  assert.deepEqual(filterLogs(lines, "409", "error"), []);
  assert.deepEqual(filterLogs([...lines, "invalid"], "", ""), lines);
});

test("bundle allowlist excludes host identity and secrets; ZIP saves the exact preview", () => {
  const health = { version: "0.1.0", api_version: "1.0", status: "ok", startup: "ready", components_ready: true, host: { name: "private@example.test", path: "C:/Users/canary", token: "sk-canary" }, startup_reason: "private@example.test" } as unknown as Health;
  const files = supportFiles(health, "0.2.0", "# Redacted doctor\nÄ Türkçe", ['{"level":"warn"}']);
  assert.deepEqual(files.map((f) => f.name), ["versions.json", "diagnostics.json", "recent-logs.jsonl", "doctor.md"]);
  assert.equal(JSON.parse(files[0].text).api, "1.0");
  assert.doesNotMatch(JSON.stringify(files), /canary|private@example/);
  const zip = supportZip(files);
  const view = new DataView(zip.buffer);
  let offset = 0;
  for (const file of files) {
    assert.equal(view.getUint32(offset, true), 0x04034b50);
    const size = view.getUint32(offset + 18, true);
    const nameLength = view.getUint16(offset + 26, true);
    assert.equal(new TextDecoder().decode(zip.subarray(offset + 30, offset + 30 + nameLength)), file.name);
    assert.equal(new TextDecoder().decode(zip.subarray(offset + 30 + nameLength, offset + 30 + nameLength + size)), file.text);
    offset += 30 + nameLength + size;
  }
  assert.equal(view.getUint32(offset, true), 0x02014b50);
  assert.equal(view.getUint32(zip.length - 22, true), 0x06054b50);
  assert.equal(view.getUint16(zip.length - 12, true), 4);
  assert.throws(() => supportZip([{ name: "chats.json", text: "private" }]));
});
