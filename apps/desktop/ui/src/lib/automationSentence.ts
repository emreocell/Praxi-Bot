/**
 * "Every weekday at 9:00, check my inbox and summarise" → when (a simple schedule) + what (the task).
 * English and Turkish, pure and deterministic: no model call. The person always sees what was
 * understood before anything is saved, so a miss costs one edit, never a wrong schedule.
 */
import { DEFAULT_SIMPLE, type SimpleSchedule } from "./scheduleBuilder.ts";

export interface AutomationSentence {
  /** The schedule, or `null` when no "when" was recognised. */
  simple: SimpleSchedule | null;
  /** The words read as "when" (empty when none). */
  whenText: string;
  /** The rest: what the bot should do. */
  task: string;
}

const EN_DAYS = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"];
// Turkish day names, written with [ıi] so "SALI" and "Salı" both match after folding.
const TR_DAYS = ["pazar", "pazartes[ıi]", "sal[ıi]", "çarşamba", "perşembe", "cuma", "cumartes[ıi]"];

/** A dotless/dotted I fold that keeps the string length (indices must match the original). */
function fold(text: string): string {
  let out = "";
  for (const c of text) {
    out += c === "İ" || c === "I" ? "i" : c.toLowerCase().length === 1 ? c.toLowerCase() : c;
  }
  return out;
}

const pad = (n: number) => String(n).padStart(2, "0");
const isoDay = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;

type Found = Partial<Pick<SimpleSchedule, "freq" | "weekday" | "monthDay" | "date">> & { time?: string };

/** One phrase at the very start of `rest`: its length and what it says, or `null`. */
type Rule = { re: RegExp; read: (m: RegExpExecArray, today: Date) => Found };

const ORD = String.raw`(\d{1,2})(?:st|nd|rd|th)?`;
const TR_SUFFIX = String.raw`(?:'|’)?(?:[dt][ae]|[ıiuü]n?[dt][ae]|n?[ıiuü]|s[ıiuü])?`;
const END = String.raw`(?=[\s,.;:!?—–-]|$)`;

function clock(h: number, m: number, mer?: string): string | null {
  let hour = h;
  if (mer) {
    if (hour < 1 || hour > 12) return null;
    hour = (hour % 12) + (mer.startsWith("p") ? 12 : 0);
  }
  return hour <= 23 && m <= 59 ? `${pad(hour)}:${pad(m)}` : null;
}

const RULES: Rule[] = [
  // Time: "at 9", "at 9:30", "9am", "9:00", "09.00", "saat 9", "9'da", "9:00'da". A bare number is not a time.
  {
    re: new RegExp(
      String.raw`^(?:(at|saat)\s+)?(\d{1,2})(?:[:.](\d{2}))?\s*(a\.?m\.?|p\.?m\.?)?(` + TR_SUFFIX + ")" + END
    ),
    read: (m) => {
      const marked = Boolean(m[1] || m[3] || m[4] || m[5]);
      const time = marked ? clock(Number(m[2]), Number(m[3] ?? 0), m[4]?.replace(/\./g, "")) : null;
      return time ? { time } : {};
    }
  },
  { re: new RegExp(String.raw`^(?:every|each|on)\s+(?:week\s*day|working\s+day|work\s*day)s?` + END), read: () => ({ freq: "weekdays" }) },
  { re: new RegExp(String.raw`^(?:week\s*days|mon(?:day)?\s*-\s*fri(?:day)?)` + END), read: () => ({ freq: "weekdays" }) },
  { re: new RegExp(String.raw`^(?:hafta\s+içi|iş\s+günleri)(?:\s+her\s+gün)?(?:\s+saat)?` + END), read: () => ({ freq: "weekdays" }) },
  { re: new RegExp(String.raw`^(?:every|each)\s+(?:day|morning|evening|night)` + END), read: () => ({ freq: "daily" }) },
  { re: new RegExp(String.raw`^(?:daily|every\s*day)` + END), read: () => ({ freq: "daily" }) },
  { re: new RegExp(String.raw`^(?:her\s+(?:gün|sabah|akşam|gece)|günlük|her\s+gün\s+saat)` + END), read: () => ({ freq: "daily" }) },
  // Weekly needs "every Monday" / "on Mondays" / "her pazartesi" / "pazartesileri": a bare
  // "Monday" is a one-off day, which this reader does not guess.
  {
    re: new RegExp(String.raw`^(?:(?:every|each)\s+(` + EN_DAYS.join("|") + String.raw`)|on\s+(` + EN_DAYS.join("|") + ")s)" + END),
    read: (m) => ({ freq: "weekly", weekday: EN_DAYS.indexOf(m[1] ?? m[2]) })
  },
  {
    re: new RegExp(String.raw`^(?:her\s+(` + TR_DAYS.join("|") + String.raw`)(?:\s+günü)?|(` + TR_DAYS.join("|") + ")l[ae]r[ıi])" + END),
    read: (m) => ({ freq: "weekly", weekday: TR_DAYS.findIndex((d) => new RegExp(`^${d}$`).test(m[1] ?? m[2])) })
  },
  {
    re: new RegExp(String.raw`^(?:(?:every|each)\s+month\s+on\s+the\s+` + ORD + String.raw`|on\s+the\s+` + ORD + String.raw`\s+of\s+(?:every|each|the)\s+month|monthly\s+on\s+the\s+` + ORD + ")" + END),
    read: (m) => ({ freq: "monthly", monthDay: Number(m[1] ?? m[2] ?? m[3]) })
  },
  { re: new RegExp(String.raw`^(?:monthly|every\s+month)` + END), read: () => ({ freq: "monthly", monthDay: 1 }) },
  {
    re: new RegExp(String.raw`^her\s+ay[ıi]n\s+(\d{1,2})\.?` + TR_SUFFIX + String.raw`(?:\s+günü)?` + END),
    read: (m) => ({ freq: "monthly", monthDay: Number(m[1]) })
  },
  { re: new RegExp(String.raw`^(?:her\s+ay|ayl[ıi]k)` + END), read: () => ({ freq: "monthly", monthDay: 1 }) },
  {
    re: new RegExp(String.raw`^(tomorrow|today|tonight|yar[ıi]n|bugün|bu\s+akşam)` + END),
    read: (m, today) => {
      const d = new Date(today);
      if (/^(tomorrow|yar)/.test(m[1])) d.setDate(d.getDate() + 1);
      return { freq: "once", date: isoDay(d) };
    }
  }
];

// Small words between the parts of a "when": "at", "on", "and", "saat", a comma before the time.
const FILLER = /^(?:at|on|in\s+the|the|and|ve|saat|,)(?=[\s,]|$)\s*/;

/**
 * Reads the "when" at the start of `text` and returns the rest as the task. A "when" needs a
 * frequency or a day ("every weekday", "tomorrow"); a time alone ("at 9, …") is not enough,
 * because it does not say how often.
 */
export function parseAutomationSentence(text: string, today: Date = new Date()): AutomationSentence {
  const source = text.trim();
  const folded = fold(source);
  let at = 0;
  let found: Found = {};
  let lastWhen = 0;
  for (;;) {
    const rest = folded.slice(at);
    const space = /^\s+/.exec(rest)?.[0].length ?? 0;
    if (space) {
      at += space;
      continue;
    }
    const filler = FILLER.exec(rest);
    const rule = RULES.map((r) => ({ r, m: r.re.exec(rest) })).find((x) => x.m && x.m[0].length > 0);
    if (rule && rule.m) {
      const got = rule.r.read(rule.m, today);
      if (Object.keys(got).length === 0) break; // a number that is not a time: the task starts here
      if ((got.freq && found.freq) || (got.time && found.time)) break; // a second "when": belongs to the task
      found = { ...found, ...got };
      at += rule.m[0].length;
      lastWhen = at;
      continue;
    }
    if (filler && filler[0].length > 0 && lastWhen > 0) {
      at += filler[0].length;
      continue;
    }
    break;
  }
  if (!found.freq) {
    return { simple: null, whenText: "", task: source };
  }
  const simple: SimpleSchedule = {
    ...DEFAULT_SIMPLE,
    freq: found.freq,
    time: found.time ?? (found.freq === "once" && /tonight|akşam/.test(folded.slice(0, lastWhen)) ? "19:00" : DEFAULT_SIMPLE.time),
    weekday: found.weekday ?? DEFAULT_SIMPLE.weekday,
    monthDay: Math.min(28, Math.max(1, found.monthDay ?? DEFAULT_SIMPLE.monthDay)),
    date: found.date ?? ""
  };
  const task = source
    .slice(lastWhen)
    .replace(/^[\s,;:.—–-]+/, "")
    .replace(/^(?:and|then|ve|sonra)\s+/i, "")
    .trim();
  return { simple, whenText: source.slice(0, lastWhen).trim(), task };
}

/** A short name from the task: its first words, with a capital letter, at most `max` characters. */
export function nameFromTask(task: string, locale?: string, max = 48): string {
  const words = task.replace(/\s+/g, " ").replace(/[.!?]+$/, "").trim();
  if (!words) return "";
  let name = words;
  if (name.length > max) {
    const cut = name.slice(0, max + 1);
    const space = cut.lastIndexOf(" ");
    name = `${cut.slice(0, space > max / 2 ? space : max).trimEnd()}…`;
  }
  return name.charAt(0).toLocaleUpperCase(locale) + name.slice(1);
}
