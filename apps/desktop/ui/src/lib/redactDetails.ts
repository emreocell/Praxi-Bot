import { detectSecrets } from "./secretDetect.ts";

/**
 * Makes error text safe to show behind a "Details" disclosure and to copy into a bug report:
 * credential shapes, token-bearing URLs, the person's user-profile folder and stack frames are
 * removed. Line breaks are kept (service logs are multi-line); nothing here changes meaning.
 */
export function redactDetails(text: string): string {
  if (!text) return text;
  let out = "";
  let at = 0;
  for (const c of detectSecrets(text)) {
    out += text.slice(at, c.start) + "[hidden]";
    at = c.end;
  }
  out += text.slice(at);
  return (
    out
      // user:password@host in a URL
      .replace(/(\b[a-z][a-z0-9+.-]*:\/\/)[^\s/@]+@/gi, "$1[hidden]@")
      // ?token=… / &key=… / #access_token=… in a URL or query string
      .replace(
        /([?&#](?:[\w-]*(?:token|key|secret|sig|signature|password|passwd|auth|code|session)[\w-]*))=[^\s&#"']*/gi,
        "$1=[hidden]"
      )
      .replace(/\b(bearer|basic)\s+(?!\[hidden\])[^\s"',;]+/gi, "$1 [hidden]")
      // "password: hunter2", "token=abc"
      .replace(
        /\b(pass(?:word|wd)?|secret|token|api[-_ ]?key|bearer|authorization)\b(\s*[:=]\s*|\s+)(?!\[hidden\])([^\s"',;]+)/gi,
        (_m, label: string, sep: string) => `${label}${sep.includes(":") || sep.includes("=") ? sep : " "}[hidden]`
      )
      // C:\Users\name\… , /Users/name/… , /home/name/…  ->  ~\…
      .replace(/[A-Za-z]:[\\/]+Users[\\/]+[^\\/"'<>|:]+/gi, "~")
      .replace(/\/(?:Users|home)\/[^/\s"'<>|:]+/g, "~")
      // JS / Rust stack frames
      .replace(/^[ \t]*at\s+.+(?:\(.*:\d+:\d+\)|:\d+:\d+)[ \t]*$/gm, "")
      .replace(/^[ \t]*\d+:\s+0x[0-9a-f]+.*$/gim, "")
      .replace(/\n{3,}/g, "\n\n")
      .trim()
  );
}
