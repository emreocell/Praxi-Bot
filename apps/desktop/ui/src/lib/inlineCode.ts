/**
 * Splits a plain string at `backtick` pairs so a screen can draw the command inside as code instead
 * of showing the raw backticks. An unpaired backtick stays as text. Pure, so it is tested without a browser.
 */
export type InlinePart = { text: string; code: boolean };

export function splitInlineCode(input: string): InlinePart[] {
  const parts: InlinePart[] = [];
  const pattern = /`([^`\n]+)`/g;
  let last = 0;
  for (let m = pattern.exec(input); m !== null; m = pattern.exec(input)) {
    if (m.index > last) parts.push({ text: input.slice(last, m.index), code: false });
    parts.push({ text: m[1] ?? "", code: true });
    last = m.index + m[0].length;
  }
  if (last < input.length) parts.push({ text: input.slice(last), code: false });
  return parts;
}
