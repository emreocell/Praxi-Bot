/**
 * Reads the plain-text report a mission posts to its chat when it ends or waits for the owner
 * (`missions::digest::render_text`), so the chat can show it as a clean card in the owner's
 * language. Anything that does not match that exact shape returns null and stays a normal message.
 */

export type DigestOutcome = "done" | "blocked" | "failed" | "cancelled" | "update";

export interface DigestRow {
  key: string;
  text: string;
}

export interface DigestText {
  outcome: DigestOutcome;
  title: string;
  counts: {
    done: number;
    blocked: number;
    open: number;
    failed: number;
    skipped: number;
    total: number;
    sessions: number;
    modelSteps: number;
  } | null;
  reason: string | null;
  needs: DigestRow[];
  failed: DigestRow[];
  decisions: string[];
}

/** The first line's bilingual headline, as `render_text` writes it. */
const HEADLINES: ReadonlyArray<[string, DigestOutcome]> = [
  ["Mission finished / Görev tamamlandı", "done"],
  ["Mission needs you / Görev senden bir şey bekliyor", "blocked"],
  ["Mission failed / Görev başarısız oldu", "failed"],
  ["Mission stopped / Görev durduruldu", "cancelled"],
  ["Mission update / Görev durumu", "update"]
];

const COUNTS =
  /^(\d+) done, (\d+) blocked, (\d+) to do, (\d+) given up, (\d+) skipped \(of (\d+)\)\. (\d+) sessions?, (\d+) model calls?\.$/;

function row(line: string): DigestRow {
  const body = line.slice(2);
  const at = body.indexOf(": ");
  return at > 0 ? { key: body.slice(0, at), text: body.slice(at + 2) } : { key: body, text: "" };
}

export function parseMissionDigest(text: string): DigestText | null {
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  const first = lines[0] ?? "";
  const head = HEADLINES.find(([h]) => first.startsWith(`${h}: `));
  if (!head) {
    return null;
  }
  const out: DigestText = {
    outcome: head[1],
    title: first.slice(head[0].length + 2).trim(),
    counts: null,
    reason: null,
    needs: [],
    failed: [],
    decisions: []
  };
  const c = COUNTS.exec((lines[1] ?? "").trim());
  if (c) {
    const n = c.slice(1).map(Number);
    out.counts = {
      done: n[0],
      blocked: n[1],
      open: n[2],
      failed: n[3],
      skipped: n[4],
      total: n[5],
      sessions: n[6],
      modelSteps: n[7]
    };
  }
  let section: "needs" | "failed" | "decisions" | null = null;
  for (const raw of lines.slice(c ? 2 : 1)) {
    const line = raw.trimEnd();
    if (line.startsWith("Why: ") && section === null) {
      out.reason = line.slice(5).trim();
    } else if (line === "Needs you:") {
      section = "needs";
    } else if (line === "Given up:") {
      section = "failed";
    } else if (line === "Decisions I made:") {
      section = "decisions";
    } else if (line.startsWith("- ") && section) {
      if (section === "decisions") {
        out.decisions.push(line.slice(2));
      } else {
        out[section].push(row(line));
      }
    } else if (line.trim() !== "") {
      // A line the report never writes: not a mission report after all, show the text as it is.
      return null;
    }
  }
  return out;
}
