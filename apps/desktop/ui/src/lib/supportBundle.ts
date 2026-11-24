import type { Health } from "../api/types.ts";

export interface SupportFile { name: string; text: string }

export function filterLogs(lines: readonly string[], search: string, level: string): string[] {
  const query = search.trim().toLocaleLowerCase();
  return lines.filter((line) => {
    try {
      const entry: unknown = JSON.parse(line);
      return typeof entry === "object" && entry !== null && "level" in entry
        && (!level || entry.level === level) && line.toLocaleLowerCase().includes(query);
    } catch { return false; }
  });
}

const version = (v: string) => /^\d+\.\d+(?:\.\d+)?(?:[-+][a-zA-Z0-9.-]+)?$/.test(v) ? v : "unknown";

/** Explicit allowlist. Never serialize Health's host identity or connection
 * objects, settings, raw doctor checks, chats or credentials into a bundle. */
export function supportFiles(health: Health, appVersion: string, doctor: string, lines: readonly string[]): SupportFile[] {
  return [
    { name: "versions.json", text: JSON.stringify({ app: version(appVersion), daemon: version(health.version), api: version(health.api_version) }, null, 2) },
    { name: "diagnostics.json", text: JSON.stringify({
      status: health.status === "ok" ? "ok" : "setup_required",
      startup: ["starting", "ready", "degraded"].includes(health.startup ?? "") ? health.startup : "unknown",
      components_ready: health.components_ready === true,
      channels_active: health.channels_active === true,
      safe_mode: health.safe_mode === true
    }, null, 2) },
    { name: "recent-logs.jsonl", text: lines.join("\n") + (lines.length ? "\n" : "") },
    { name: "doctor.md", text: doctor }
  ];
}

/** Upper bound for the bundle's uncompressed payload (the preview is already log-capped). */
export const MAX_SUPPORT_ZIP_BYTES = 8 * 1024 * 1024;

function crc32(bytes: Uint8Array): number {
  let crc = 0xffffffff;
  for (const b of bytes) {
    crc ^= b;
    for (let i = 0; i < 8; i++) crc = (crc >>> 1) ^ ((crc & 1) ? 0xedb88320 : 0);
  }
  return (crc ^ 0xffffffff) >>> 0;
}

/** Small ZIP writer (stored/UTF-8): no compression worker or network needed.
 * Input is the immutable four-file preview, never filesystem paths. */
export function supportZip(files: readonly SupportFile[]): Uint8Array<ArrayBuffer> {
  const encoder = new TextEncoder();
  const local: Uint8Array[] = [];
  const central: Uint8Array[] = [];
  let offset = 0;
  let centralSize = 0;
  let total = 0;
  for (const file of files) {
    if (!["versions.json", "diagnostics.json", "recent-logs.jsonl", "doctor.md"].includes(file.name)) throw new Error("Unexpected support file");
    const name = encoder.encode(file.name);
    const data = encoder.encode(file.text);
    total += data.length;
    if (total > MAX_SUPPORT_ZIP_BYTES) throw new Error("Support bundle is too large");
    const crc = crc32(data);
    const header = new Uint8Array(30 + name.length);
    const view = new DataView(header.buffer);
    view.setUint32(0, 0x04034b50, true);
    view.setUint16(4, 20, true);
    view.setUint16(6, 0x0800, true);
    view.setUint16(12, 0x0021, true); // 1980-01-01; no user-specific metadata.
    view.setUint32(14, crc, true);
    view.setUint32(18, data.length, true);
    view.setUint32(22, data.length, true);
    view.setUint16(26, name.length, true);
    header.set(name, 30);
    local.push(header, data);
    const directory = new Uint8Array(46 + name.length);
    const d = new DataView(directory.buffer);
    d.setUint32(0, 0x02014b50, true);
    d.setUint16(4, 20, true);
    directory.set(header.subarray(4, 30), 6);
    d.setUint32(42, offset, true);
    directory.set(name, 46);
    central.push(directory);
    centralSize += directory.length;
    offset += header.length + data.length;
  }
  const end = new Uint8Array(22);
  const e = new DataView(end.buffer);
  e.setUint32(0, 0x06054b50, true);
  e.setUint16(8, files.length, true);
  e.setUint16(10, files.length, true);
  e.setUint32(12, centralSize, true);
  e.setUint32(16, offset, true);
  const result = new Uint8Array(offset + centralSize + end.length);
  let position = 0;
  for (const part of [...local, ...central, end]) { result.set(part, position); position += part.length; }
  return result;
}
