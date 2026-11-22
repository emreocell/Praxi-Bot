/**
 * The run timeline and its exported report (issue #584): what a bot did in one run, step by step.
 *
 * Built only from the run events the app already holds. The report is a whitelist: it carries
 * the plain step wording, times, outcomes, approval answers and a scrubbed error, and NEVER the
 * tool input, tool output, the assistant's text, the owner's answers, credential names or
 * credential request purposes. Whatever free text does get in (an approval summary, a failure
 * message, a short chip) is scrubbed of credential shapes and clipped first.
 */
import { describeTool, isDiscoveryTool, stepPhase } from "./toolLabels.ts";
import { detectSecrets } from "./secretDetect.ts";
import type { QuestionItem, RunState, SecretRequestItem, ToolItem } from "../state/timeline.ts";

export type ReportT = (key: string, values?: Record<string, unknown>) => string;

export type StepTone = "ok" | "bad" | "warn" | "info";

export interface ReportStep {
  atMs: number;
  /** One plain sentence: "Searched the web". */
  title: string;
  tone: StepTone;
  /** Short facts under the title (host, approval answer, how long it took). */
  details: string[];
}

export interface RunReport {
  botName: string;
  startedMs: number;
  finishedMs: number | null;
  /** `done`, `failed`, `stopped` or `unfinished`. */
  outcome: "done" | "failed" | "stopped" | "unfinished";
  /** Scrubbed failure text, when the run failed. */
  error: string | null;
  steps: ReportStep[];
}

const CLIP = 200;

/** Credential shapes become `[hidden]`; the text is flattened to one clipped line. */
export function scrubText(text: string, max = CLIP): string {
  let out = "";
  let at = 0;
  for (const c of detectSecrets(text)) {
    out += text.slice(at, c.start) + "[hidden]";
    at = c.end;
  }
  out += text.slice(at);
  // A labelled value ("password: hunter2", "token=abc") is hidden even without a known shape.
  out = out.replace(
    /\b(pass(?:word|wd)?|secret|token|api[-_ ]?key|bearer|authorization)\b(\s*[:=]\s*|\s+)(\S+)/gi,
    (_m, label: string, sep: string) => `${label}${sep.includes(":") || sep.includes("=") ? sep : " "}[hidden]`
  );
  const flat = out.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}

function toolStep(item: ToolItem, t: ReportT, ms: (n: number) => string): ReportStep {
  const label = describeTool(item.name, item.input);
  const phase = stepPhase(item.status);
  const details: string[] = [];
  for (const chip of label.chips) {
    details.push(scrubText(chip.text, 60));
  }
  if (item.gated) {
    if (item.status === "denied") {
      details.push(t("runReport.declined"));
    } else if (item.status === "expired") {
      details.push(t("runReport.unanswered"));
    } else if (item.status === "awaiting_approval") {
      details.push(t("runReport.waitingForYou"));
    } else {
      details.push(t("runReport.approved"));
    }
    if (item.summary) {
      details.push(scrubText(item.summary));
    }
  }
  if (item.status === "error" && item.error) {
    details.push(scrubText(item.error));
  }
  if (item.durationMs !== undefined && item.status === "done") {
    details.push(t("runReport.tookTime", { time: ms(item.durationMs) }));
  }
  const tone: StepTone =
    item.status === "error" || item.status === "denied"
      ? "bad"
      : item.status === "expired" || item.status === "awaiting_approval"
        ? "warn"
        : "ok";
  return {
    atMs: item.requestedMs,
    title: t(`${label.key}.${phase}`, label.values),
    tone,
    details
  };
}

function questionStep(item: QuestionItem, t: ReportT): ReportStep {
  // The question text is model text and the answer is the owner's own: only the fact is kept.
  return {
    atMs: item.requestedMs,
    title: t("runReport.asked"),
    tone: "info",
    details: [item.answer !== undefined ? t("runReport.answered") : t("runReport.unanswered")]
  };
}

function secretStep(item: SecretRequestItem, t: ReportT): ReportStep {
  return {
    atMs: item.requestedMs,
    title: t("runReport.askedCredential"),
    tone: "info",
    details: [
      item.outcome === "stored"
        ? t("runReport.credentialSaved")
        : item.outcome === "cancelled"
          ? t("runReport.credentialCancelled")
          : t("runReport.unanswered")
    ]
  };
}

/** The steps of one run in the order they happened. Works for failed and stopped runs too. */
export function buildRunReport(
  run: RunState,
  botName: string,
  t: ReportT,
  formatDuration: (ms: number) => string
): RunReport {
  const steps: ReportStep[] = [];
  for (const item of run.items) {
    if (item.kind === "tool" && !isDiscoveryTool(item.name)) {
      steps.push(toolStep(item, t, formatDuration));
    } else if (item.kind === "question") {
      steps.push(questionStep(item, t));
    } else if (item.kind === "secret") {
      steps.push(secretStep(item, t));
    }
  }
  steps.sort((a, b) => a.atMs - b.atMs);
  const finished = run.finishedMs !== undefined;
  const outcome: RunReport["outcome"] = !finished
    ? "unfinished"
    : run.stopped
      ? "stopped"
      : run.error
        ? "failed"
        : "done";
  return {
    botName: scrubText(botName, 80),
    startedMs: run.startedMs,
    finishedMs: run.finishedMs ?? null,
    outcome,
    error: outcome === "failed" && run.error ? scrubText(run.error) : null,
    steps
  };
}

export interface ReportText {
  title: string;
  /** Already translated label/value pairs: outcome, start time, how long it took. */
  facts: { label: string; value: string }[];
  noSteps: string;
  footer: string;
  language: string;
  /** Formats an epoch time for people (local time). */
  time: (ms: number) => string;
}

const MD_SPECIAL = /([\\`*_{}[\]<>()#+!|~])/g;
const mdEscape = (s: string) => s.replace(MD_SPECIAL, "\\$1");

export function renderMarkdown(report: RunReport, text: ReportText): string {
  const lines = [`# ${mdEscape(text.title)}`, "", `**${mdEscape(report.botName)}**`, ""];
  for (const h of text.facts) {
    lines.push(`- ${mdEscape(h.label)}: ${mdEscape(h.value)}`);
  }
  if (report.error) {
    lines.push(`- ${mdEscape(report.error)}`);
  }
  lines.push("");
  if (report.steps.length === 0) {
    lines.push(mdEscape(text.noSteps));
  }
  report.steps.forEach((s, i) => {
    lines.push(`${i + 1}. ${mdEscape(text.time(s.atMs))} - **${mdEscape(s.title)}**`);
    for (const d of s.details) {
      lines.push(`   - ${mdEscape(d)}`);
    }
  });
  lines.push("", `_${mdEscape(text.footer)}_`, "");
  return lines.join("\n");
}

const HTML_ESCAPES: Record<string, string> = {
  "&": "&amp;",
  "<": "&lt;",
  ">": "&gt;",
  '"': "&quot;",
  "'": "&#39;"
};
const esc = (s: string) => s.replace(/[&<>"']/g, (c) => HTML_ESCAPES[c] ?? c);

const TONE_COLOR: Record<StepTone, string> = {
  ok: "#2e7d4f",
  bad: "#b3261e",
  warn: "#9a6700",
  info: "#4a5568"
};

/** One self-contained page: inline style, no script, no image, no link, no network. */
export function renderHtml(report: RunReport, text: ReportText): string {
  const steps = report.steps
    .map(
      (s) =>
        `<li style="border-left-color:${TONE_COLOR[s.tone]}"><time>${esc(text.time(s.atMs))}</time> <strong>${esc(s.title)}</strong>` +
        (s.details.length > 0 ? `<ul>${s.details.map((d) => `<li>${esc(d)}</li>`).join("")}</ul>` : "") +
        "</li>"
    )
    .join("");
  const facts = text.facts
    .map((h) => `<dt>${esc(h.label)}</dt><dd>${esc(h.value)}</dd>`)
    .join("");
  return `<!doctype html>
<html lang="${esc(text.language)}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'">
<title>${esc(text.title)}</title>
<style>
:root{color-scheme:light dark;font-family:system-ui,sans-serif}
body{max-width:46rem;margin:2rem auto;padding:0 1rem;line-height:1.5}
dl{display:grid;grid-template-columns:max-content 1fr;gap:.25rem 1rem}
dt{opacity:.7}dd{margin:0}
ol{padding:0;list-style:none}
ol>li{border-left:4px solid;padding:.25rem .75rem;margin:.5rem 0}
ol ul{margin:.25rem 0 0;padding-left:1.25rem;opacity:.85}
time{opacity:.7;margin-right:.5rem}
footer{opacity:.6;font-size:.85em;margin-top:2rem}
</style>
</head>
<body>
<h1>${esc(text.title)}</h1>
<h2>${esc(report.botName)}</h2>
<dl>${facts}</dl>
${report.error ? `<p>${esc(report.error)}</p>` : ""}
${report.steps.length === 0 ? `<p>${esc(text.noSteps)}</p>` : `<ol>${steps}</ol>`}
<footer>${esc(text.footer)}</footer>
</body>
</html>
`;
}

/** Hand the text to the user as a file. The name carries no run content. */
export function saveTextFile(name: string, mime: string, body: string): void {
  const url = URL.createObjectURL(new Blob([body], { type: `${mime};charset=utf-8` }));
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = name;
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}
