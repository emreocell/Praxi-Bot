/** Where each calendar service keeps its CalDAV calendars, so the owner does not have to look it up. */

export type CalendarPresetId = "google" | "icloud" | "fastmail" | "nextcloud" | "other";

export interface CalendarPreset {
  id: CalendarPresetId;
  /** The address the preset starts from; `{user}` and `{server}` are filled in by `calendarPresetUrl`. */
  template: string;
  /** Needs the address of the owner's own server (Nextcloud). */
  needsServer: boolean;
}

export const CALENDAR_PRESETS: readonly CalendarPreset[] = [
  { id: "icloud", template: "https://caldav.icloud.com/", needsServer: false },
  { id: "fastmail", template: "https://caldav.fastmail.com/dav/calendars/user/{user}/", needsServer: false },
  { id: "nextcloud", template: "https://{server}/remote.php/dav/calendars/{user}/", needsServer: true },
  { id: "google", template: "https://apidata.googleusercontent.com/caldav/v2/{user}/events/", needsServer: false },
  { id: "other", template: "", needsServer: false }
];

/** `https://cloud.example.com/index.php` and `cloud.example.com/` both become `cloud.example.com`. */
export function cleanServer(raw: string): string {
  return raw
    .trim()
    .replace(/^[a-z][a-z0-9+.-]*:\/\//i, "")
    .replace(/[/?#].*$/, "");
}

/**
 * The address to try for `preset`. Empty while a part it needs is still missing, so the
 * form never shows a half-filled address. The user name goes into the path percent-encoded.
 */
export function calendarPresetUrl(
  id: CalendarPresetId,
  parts: { username: string; server: string }
): string {
  const preset = CALENDAR_PRESETS.find((p) => p.id === id);
  if (!preset || preset.template === "") {
    return "";
  }
  const user = parts.username.trim();
  const server = cleanServer(parts.server);
  if (preset.template.includes("{user}") && user === "") {
    return "";
  }
  if (preset.needsServer && server === "") {
    return "";
  }
  return preset.template
    .replace("{user}", encodeURIComponent(user))
    .replace("{server}", server);
}

/** A short name for the calendar the bot will use, from what the owner typed or the service. */
export function suggestCalendarName(id: CalendarPresetId, existing: readonly string[]): string {
  const base = id === "other" ? "calendar" : id;
  if (!existing.includes(base)) {
    return base;
  }
  for (let n = 2; n < 100; n++) {
    if (!existing.includes(`${base}-${n}`)) {
      return `${base}-${n}`;
    }
  }
  return base;
}

/** The name rules of the daemon: 1 to 40 letters, digits, `-` and `_`. */
export function isValidCalendarName(name: string): boolean {
  return /^[A-Za-z0-9_-]{1,40}$/.test(name);
}
