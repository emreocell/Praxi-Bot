/**
 * Pure rules of the MCP form card (`ElicitationCard`), kept apart from React so they can be
 * tested. The server is authoritative: these checks only spare the owner a round trip.
 */
import type { AnswerElicitation, ElicitationField } from "../api/types.ts";

export type ElicitationContent = AnswerElicitation["content"];

export type FieldProblem = "blocked" | "required" | "number" | "integer" | "tooSmall" | "tooLarge" | "tooShort" | "tooLong";

export type BuildResult =
  | { ok: true; content: ElicitationContent }
  | { ok: false; field: ElicitationField; problem: FieldProblem };

/** What an input reads: text for text, number and choice inputs, `checked` for a checkbox. */
export type RawValue = string | boolean | undefined;

/**
 * The `content` for `POST /v1/elicitations/{call_id}`, or the first field that does not fit.
 * Empty optional fields are left out; numbers are sent as numbers; a `date-time` from the
 * browser's local picker is sent as RFC 3339 (UTC). A blocked field (a suspected credential) never
 * contributes a value, whatever `raw` holds; a required one can never be satisfied.
 */
export function buildContent(fields: readonly ElicitationField[], raw: readonly RawValue[]): BuildResult {
  const content: ElicitationContent = {};
  for (const [n, field] of fields.entries()) {
    if (field.blocked === true) {
      if (field.required) return { ok: false, field, problem: "blocked" };
      continue;
    }
    const v = raw[n];
    if (field.type === "boolean") {
      content[field.name] = v === true;
      continue;
    }
    const text = typeof v === "string" ? v : "";
    if (text.trim() === "") {
      if (field.required) return { ok: false, field, problem: "required" };
      continue;
    }
    if (field.type === "number" || field.type === "integer") {
      const num = Number(text.trim());
      if (!Number.isFinite(num)) return { ok: false, field, problem: "number" };
      if (field.type === "integer" && !Number.isInteger(num)) return { ok: false, field, problem: "integer" };
      if (field.minimum !== undefined && num < field.minimum) return { ok: false, field, problem: "tooSmall" };
      if (field.maximum !== undefined && num > field.maximum) return { ok: false, field, problem: "tooLarge" };
      content[field.name] = num;
      continue;
    }
    // Lengths in characters (code points), as the server counts them.
    const chars = Array.from(text).length;
    if (field.min_length !== undefined && chars < field.min_length) return { ok: false, field, problem: "tooShort" };
    if (field.max_length !== undefined && chars > field.max_length) return { ok: false, field, problem: "tooLong" };
    if (field.format === "date-time") {
      const d = new Date(text);
      content[field.name] = Number.isNaN(d.getTime()) ? text : d.toISOString();
    } else {
      content[field.name] = text;
    }
  }
  return { ok: true, content };
}

/** True when a required field is blocked: the form can then only be declined or cancelled. */
export function hasUnsatisfiableField(fields: readonly ElicitationField[]): boolean {
  return fields.some((f) => f.blocked === true && f.required);
}

/** The browser input type for a text field. */
export function inputType(field: ElicitationField): string {
  if (field.type === "number" || field.type === "integer") return "number";
  switch (field.format) {
    case "email":
      return "email";
    case "uri":
      return "url";
    case "date":
      return "date";
    case "date-time":
      return "datetime-local";
    default:
      return "text";
  }
}

/** The schema default as an input's initial text, when it fits the field (nothing else is prefilled). */
export function defaultText(field: ElicitationField): string | undefined {
  const d = field.default;
  if (field.type === "string" && typeof d === "string") return d;
  if ((field.type === "number" || field.type === "integer") && typeof d === "number") return String(d);
  return undefined;
}

/** The schema default for a checkbox. */
export function defaultChecked(field: ElicitationField): boolean {
  return field.type === "boolean" && field.default === true;
}

/** Error codes after which the card cannot be answered any more. */
export function isElicitationGone(code: string): boolean {
  return code === "elicitation_gone";
}
