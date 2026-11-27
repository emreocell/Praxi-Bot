/**
 * "Connect my email and calendar": the logic behind the guided flow.
 *
 * The owner types an address; the provider is read from it, and each provider gets the easiest
 * path that really works today:
 *
 * - iCloud, Fastmail, Yahoo and any other IMAP provider: an app password (with a direct link to
 *   the page that makes one), then a live test of the mail servers and, where the provider has
 *   one, the CalDAV calendar. iCloud and Fastmail use the same password for both.
 * - Gmail and Google Workspace: Google's own sign-in (the official Gmail and Calendar servers,
 *   read-only), or an app password for mail only.
 * - Outlook and Microsoft 365: Microsoft turned off app passwords for mail, so the only way is
 *   its own sign-in, which Praxi offers through its app hub.
 *
 * Nothing here weakens a rule: secrets are only ever sent to the daemon's write-only fields (the
 * secret store keeps them), a new mailbox starts as "drafts only" (read and draft, never send), and
 * the daemon's own checks (addresses, redirects, approvals) are the ones that apply.
 */

import type {
  CalendarAccount,
  CalendarAccountInput,
  CalendarTestInput,
  CalendarTestReport,
  EmailAccount,
  EmailAccountInput,
  EmailProbeStatus,
  EmailSendMode,
  EmailTestReport,
  McpCatalogEntry,
  McpServer
} from "../api/types.ts";
import { calendarPresetUrl, isValidCalendarName } from "./calendarPresets.ts";
import { EMAIL_PRESETS, type EmailApi, type EmailPreset } from "./emailAccounts.ts";
import { googleEntries } from "./googleSetup.ts";

export type ProviderId = "google" | "microsoft" | "icloud" | "yahoo" | "fastmail" | "other";

export const PROVIDER_IDS: readonly ProviderId[] = ["google", "microsoft", "icloud", "yahoo", "fastmail", "other"];

/** How a part (mail or calendar) is connected. */
export type ConnectPath =
  /** The owner makes an app password and Praxi logs in with it (IMAP / CalDAV). */
  | "app_password"
  /** Google's own Gmail and Calendar servers, signed in once in the browser (read-only). */
  | "google_sign_in"
  /** Outlook through the Praxi app hub's hosted sign-in. */
  | "microsoft_sign_in"
  /** The provider has no connection Praxi can offer for this part. */
  | "none";

export interface ProviderProfile {
  id: ProviderId;
  /** The ways to connect mail, the easiest first. */
  mail: readonly ConnectPath[];
  /** The ways to connect the calendar, the easiest first. */
  calendar: readonly ConnectPath[];
  /** Where the owner makes an app password (a plain page of the provider), if that is the way. */
  appPasswordUrl: string | null;
  /** Where the owner can see and take back what they allowed (also revokes). */
  accessUrl: string | null;
}

const PROFILES: Record<ProviderId, ProviderProfile> = {
  google: {
    id: "google",
    mail: ["google_sign_in", "app_password"],
    calendar: ["google_sign_in"],
    appPasswordUrl: "https://myaccount.google.com/apppasswords",
    accessUrl: "https://myaccount.google.com/permissions"
  },
  microsoft: {
    id: "microsoft",
    mail: ["microsoft_sign_in"],
    calendar: ["microsoft_sign_in"],
    appPasswordUrl: null,
    accessUrl: "https://account.live.com/consent/Manage"
  },
  icloud: {
    id: "icloud",
    mail: ["app_password"],
    calendar: ["app_password"],
    appPasswordUrl: "https://account.apple.com/account/manage",
    accessUrl: "https://account.apple.com/account/manage"
  },
  yahoo: {
    id: "yahoo",
    mail: ["app_password"],
    calendar: ["none"],
    appPasswordUrl: "https://login.yahoo.com/account/security",
    accessUrl: "https://login.yahoo.com/account/security"
  },
  fastmail: {
    id: "fastmail",
    mail: ["app_password"],
    calendar: ["app_password"],
    appPasswordUrl: "https://app.fastmail.com/settings/security/integrations",
    accessUrl: "https://app.fastmail.com/settings/security/integrations"
  },
  other: {
    id: "other",
    mail: ["app_password"],
    calendar: ["app_password"],
    appPasswordUrl: null,
    accessUrl: null
  }
};

export function providerProfile(id: ProviderId): ProviderProfile {
  return PROFILES[id];
}

const ADDRESS_RE = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

export function isAddress(text: string): boolean {
  return ADDRESS_RE.test(text.trim());
}

/** The part after the last @, lower case; empty when there is none. */
export function domainOf(address: string): string {
  const at = address.trim().lastIndexOf("@");
  return at < 0 ? "" : address
    .trim()
    .slice(at + 1)
    .toLowerCase();
}

const GOOGLE_DOMAINS = ["gmail.com", "googlemail.com"];
const MICROSOFT_DOMAINS = [
  "outlook.com",
  "hotmail.com",
  "live.com",
  "msn.com",
  "outlook.de",
  "outlook.fr",
  "outlook.es",
  "outlook.com.tr",
  "hotmail.co.uk",
  "hotmail.de",
  "hotmail.fr",
  "hotmail.es",
  "hotmail.it",
  "hotmail.com.tr",
  "live.co.uk",
  "live.de",
  "live.fr",
  "live.nl",
  "live.it"
];
const ICLOUD_DOMAINS = ["icloud.com", "me.com", "mac.com"];
const YAHOO_DOMAINS = [
  "yahoo.com",
  "ymail.com",
  "rocketmail.com",
  "yahoo.co.uk",
  "yahoo.de",
  "yahoo.fr",
  "yahoo.es",
  "yahoo.it",
  "yahoo.ca",
  "yahoo.com.au",
  "yahoo.co.in",
  "yahoo.in",
  "yahoo.com.br",
  "yahoo.com.tr",
  "yahoo.com.mx",
  "yahoo.co.jp",
  "yahoo.nl",
  "yahoo.se",
  "yahoo.ie",
  "yahoo.pl",
  "yahoo.gr"
];
const FASTMAIL_DOMAINS = ["fastmail.com", "fastmail.fm", "messagingengine.com"];

const endsWithAny = (domain: string, list: readonly string[]) =>
  list.some((d) => domain === d || domain.endsWith(`.${d}`));

/**
 * The provider an address belongs to, or `null` while it is not an address yet. An address on the
 * owner's own domain (a work address) cannot be told apart from here: it comes back as `other`
 * and the owner can say it is Google Workspace or Microsoft 365 with one click.
 */
export function detectProvider(address: string): ProviderId | null {
  if (!isAddress(address)) {
    return null;
  }
  const domain = domainOf(address);
  if (endsWithAny(domain, GOOGLE_DOMAINS)) return "google";
  if (endsWithAny(domain, MICROSOFT_DOMAINS)) return "microsoft";
  if (endsWithAny(domain, ICLOUD_DOMAINS)) return "icloud";
  if (endsWithAny(domain, YAHOO_DOMAINS)) return "yahoo";
  if (endsWithAny(domain, FASTMAIL_DOMAINS)) return "fastmail";
  return "other";
}

/** The IMAP / SMTP servers of a provider; a guess from the domain for `other`. */
export interface MailServers {
  imapHost: string;
  imapPort: number;
  smtpHost: string;
  smtpPort: number;
  smtpSecurity: "implicit" | "start_tls";
}

const FASTMAIL_SERVERS: MailServers = {
  imapHost: "imap.fastmail.com",
  imapPort: 993,
  smtpHost: "smtp.fastmail.com",
  smtpPort: 465,
  smtpSecurity: "implicit"
};

function fromPreset(p: EmailPreset): MailServers {
  return {
    imapHost: p.imapHost,
    imapPort: p.imapPort,
    smtpHost: p.smtpHost,
    smtpPort: p.smtpPort,
    smtpSecurity: p.smtpSecurity
  };
}

/** The servers to try. For a provider Praxi does not know, `imap.<domain>` and `smtp.<domain>`. */
export function mailServers(provider: ProviderId, address: string): MailServers {
  const preset = (id: EmailPreset["id"]) => {
    const p = EMAIL_PRESETS.find((x) => x.id === id);
    return p ? fromPreset(p) : null;
  };
  const known =
    provider === "google"
      ? preset("gmail")
      : provider === "microsoft"
        ? preset("outlook")
        : provider === "icloud"
          ? preset("icloud")
          : provider === "yahoo"
            ? preset("yahoo")
            : provider === "fastmail"
              ? FASTMAIL_SERVERS
              : null;
  if (known) {
    return known;
  }
  const domain = domainOf(address) || "example.com";
  return {
    imapHost: `imap.${domain}`,
    imapPort: 993,
    smtpHost: `smtp.${domain}`,
    smtpPort: 465,
    smtpSecurity: "implicit"
  };
}

/** What the owner has typed and chosen in the flow. */
export interface GuidePlan {
  provider: ProviderId;
  address: string;
  /** The login when it is not the address (an Apple ID that is another address). Empty = the address. */
  login: string;
  /** The app password. Write-only: it goes to the daemon and nowhere else. */
  password: string;
  mail: { on: boolean; servers: MailServers; bot: string | null; allowSend: boolean } | null;
  calendar: { on: boolean; server: string } | null;
}

export const loginOf = (plan: Pick<GuidePlan, "address" | "login">): string => plan.login.trim() || plan.address.trim();

/** The send mode a new mailbox gets: drafts only until the owner opts in to sending. */
export function sendModeOf(allowSend: boolean): EmailSendMode {
  return allowSend ? "ask" : "draft_only";
}

/** A short unique name for the local account: `icloud`, then `icloud-2` ... */
export function uniqueName(base: string, taken: readonly string[]): string {
  const clean = base.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 30) || "mail";
  if (!taken.includes(clean)) {
    return clean;
  }
  for (let n = 2; n < 100; n++) {
    if (!taken.includes(`${clean}-${n}`)) {
      return `${clean}-${n}`;
    }
  }
  return clean;
}

/** The base of an account name: the provider, or the domain for an unknown one. */
export function nameBase(provider: ProviderId, address: string): string {
  if (provider === "other") {
    return (domainOf(address).split(".")[0] ?? "") || "mail";
  }
  return provider === "google" ? "gmail" : provider === "microsoft" ? "outlook" : provider;
}

/** Body of the mail test and of the account: password in the write-only field only. */
export function mailInput(plan: GuidePlan, name: string): EmailAccountInput {
  const m = plan.mail;
  if (!m) {
    throw new Error("mail is not part of this plan");
  }
  const login = loginOf(plan);
  return {
    name,
    provider: "imap_smtp",
    address: plan.address.trim(),
    username: login === plan.address.trim() ? null : login,
    imap: { host: m.servers.imapHost, port: m.servers.imapPort },
    smtp: { host: m.servers.smtpHost, port: m.servers.smtpPort, security: m.servers.smtpSecurity },
    password: plan.password,
    bot: m.bot,
    shared_with: [],
    default_for: [],
    send_mode: sendModeOf(m.allowSend),
    allowed_senders: []
  };
}

/** The CalDAV address to try first. `other` uses what the owner typed; the daemon finds the rest. */
export function calendarUrl(plan: Pick<GuidePlan, "provider" | "address" | "login" | "calendar">): string {
  const cal = plan.calendar;
  if (!cal) return "";
  const login = loginOf(plan);
  if (plan.provider === "icloud") {
    return calendarPresetUrl("icloud", { username: login, server: "" });
  }
  if (plan.provider === "fastmail") {
    return calendarPresetUrl("fastmail", { username: login, server: "" });
  }
  return normaliseServerAddress(cal.server);
}

/**
 * `caldav.example.com` becomes `https://caldav.example.com/`. What the owner typed is otherwise
 * passed on as it is (a path stays, a scheme stays): the daemon decides what is allowed.
 */
export function normaliseServerAddress(raw: string): string {
  const text = raw.trim();
  if (text === "") return "";
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(text)) return text;
  return text.includes("/") ? `https://${text}` : `https://${text}/`;
}

export function calendarTestInput(plan: GuidePlan): CalendarTestInput {
  return { url: calendarUrl(plan), username: loginOf(plan), password: plan.password };
}

/** A local name for a calendar: the provider, or the provider and the calendar's own name. */
export function calendarAccountName(base: string, calendar: string, taken: readonly string[], several: boolean): string {
  const slug = calendar
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 20);
  const wanted = several && slug !== "" ? `${base}-${slug}` : base;
  const name = uniqueName(wanted, taken);
  return isValidCalendarName(name) ? name : uniqueName(base, taken);
}

// ---- plain-words failures ---------------------------------------------------

/** The i18n key (under `settingsModal.connectMail.fix.`) that says what to do about a mail failure. */
export function mailFixKey(provider: ProviderId, status: EmailProbeStatus): string | null {
  switch (status) {
    case "ok":
      return null;
    case "auth_failed":
      return `mail.auth.${provider}`;
    case "unreachable":
      return provider === "other" ? "mail.unreachableGuess" : "mail.unreachable";
    case "tls":
      return provider === "other" ? "mail.tlsGuess" : "mail.tls";
    case "protocol":
      return provider === "other" ? "mail.protocolGuess" : "mail.protocol";
    case "timeout":
      return "mail.timeout";
  }
}

/** Whether the fix is about the server names (so the server fields should open). */
export function isServerProblem(status: EmailProbeStatus): boolean {
  return status === "unreachable" || status === "tls" || status === "protocol";
}

export type CalendarProblem = "auth" | "notFound" | "unreachable" | "redirect" | "other";

/** Sorts the daemon's own wording of a failed calendar login into a few causes. */
export function calendarProblem(error: string | null | undefined): CalendarProblem {
  const text = (error ?? "").toLowerCase();
  if (/redirect/.test(text)) return "redirect";
  if (/(refused the login|login|password|unauthori[sz]ed|401|403|forbidden|credential)/.test(text)) return "auth";
  if (/(not found|404|no calendar)/.test(text)) return "notFound";
  if (/(reach|connect|dns|resolve|timed out|timeout|tls|certificate|network)/.test(text)) return "unreachable";
  return "other";
}

/** The i18n key (under `settingsModal.connectMail.fix.`) for a failed calendar test. */
export function calendarFixKey(provider: ProviderId, error: string | null | undefined): string {
  const problem = calendarProblem(error);
  if (problem === "auth") {
    return provider === "icloud" || provider === "fastmail" ? `calendar.auth.${provider}` : "calendar.auth.other";
  }
  return `calendar.${problem}`;
}

// ---- running the test and saving ---------------------------------------------

export type PartResult<R> =
  | { state: "ok"; report: R }
  | { state: "failed"; report: R }
  /** The daemon refused the request itself (bad input, not a login problem). */
  | { state: "error"; error: unknown };

export interface GuideTest {
  mail: PartResult<EmailTestReport> | null;
  calendar: PartResult<CalendarTestReport> | null;
}

export type GuideTestApi = Pick<EmailApi, "testEmailDraft"> & {
  testCalendar: (input: CalendarTestInput) => Promise<CalendarTestReport>;
};

/** Both live tests; nothing is saved and nothing is sent. A part that is not wanted is `null`. */
export async function testPlan(api: GuideTestApi, plan: GuidePlan): Promise<GuideTest> {
  const run = async <R extends { ok: boolean }>(
    call: () => Promise<R>,
    usable: (report: R) => boolean
  ): Promise<PartResult<R>> => {
    try {
      const report = await call();
      return usable(report) ? { state: "ok", report } : { state: "failed", report };
    } catch (error) {
      return { state: "error", error };
    }
  };
  const allowSend = plan.mail?.allowSend === true;
  // One after the other: the daemon lets only two connection tests run at once, and the owner may
  // have another one going.
  const mail = plan.mail?.on
    ? await run(() => api.testEmailDraft(mailInput(plan, "test")), (r) => mailUsable(r, allowSend))
    : null;
  const calendar = plan.calendar?.on ? await run(() => api.testCalendar(calendarTestInput(plan)), (r) => r.ok) : null;
  return { mail, calendar };
}

/**
 * Whether a mailbox is good enough to connect. Reading needs only the incoming server, so while the
 * bot may not send, a refused outgoing server does not stop the connection (it is shown as a note).
 */
export function mailUsable(report: EmailTestReport, allowSend: boolean): boolean {
  return report.ok || (!allowSend && report.imap === "ok");
}

export function allPassed(test: GuideTest): boolean {
  return [test.mail, test.calendar].every((p) => p === null || p.state === "ok");
}

export function anyPassed(test: GuideTest): boolean {
  return [test.mail, test.calendar].some((p) => p?.state === "ok");
}

export type GuideSaveApi = Pick<EmailApi, "createEmailAccount"> & {
  createCalendarAccount: (input: CalendarAccountInput) => Promise<CalendarAccount>;
};

export interface GuideSaved {
  mail: { name: string } | null;
  calendars: string[];
  /** Parts that passed the test but could not be saved. */
  failed: Array<{ part: "mail" | "calendar"; error: unknown }>;
}

/**
 * Saves the parts that passed their test. `chosen` are the calendar addresses the owner ticked
 * (empty = the address that was tested, which the daemon accepts when it is one calendar).
 */
export async function savePlan(
  api: GuideSaveApi,
  plan: GuidePlan,
  test: GuideTest,
  taken: { mail: readonly string[]; calendar: readonly string[] },
  chosen: readonly { name: string; url: string }[]
): Promise<GuideSaved> {
  const saved: GuideSaved = { mail: null, calendars: [], failed: [] };
  const base = nameBase(plan.provider, plan.address);
  if (plan.mail?.on && test.mail?.state === "ok") {
    try {
      const name = uniqueName(base, taken.mail);
      await api.createEmailAccount(mailInput(plan, name));
      saved.mail = { name };
    } catch (error) {
      saved.failed.push({ part: "mail", error });
    }
  }
  if (plan.calendar?.on && test.calendar?.state === "ok") {
    // Nothing ticked out of several found calendars means none; the tested address stands in only
    // when it was itself the one calendar.
    const found = test.calendar.state === "ok" ? test.calendar.report.calendars.length : 0;
    const targets = chosen.length > 0 ? chosen : found > 1 ? [] : [{ name: "", url: calendarUrl(plan) }];
    const used = [...taken.calendar];
    for (const target of targets) {
      try {
        const name = calendarAccountName(base, target.name, used, targets.length > 1);
        await api.createCalendarAccount({
          name,
          url: target.url,
          username: loginOf(plan),
          password: plan.password,
          email: plan.address.trim()
        });
        used.push(name);
        saved.calendars.push(name);
      } catch (error) {
        saved.failed.push({ part: "calendar", error });
      }
    }
  }
  return saved;
}

// ---- what is connected, and disconnecting it ---------------------------------

/** The Google apps of the catalog that read mail or calendar (not Drive). */
export function googleMailCalendarEntries(catalog: readonly McpCatalogEntry[]): McpCatalogEntry[] {
  return googleEntries(catalog).filter((e) => /gmail|calendar/i.test(e.id));
}

export interface Connected {
  mailboxes: EmailAccount[];
  calendars: CalendarAccount[];
  googleApps: McpServer[];
}

export type ConnectedApi = Pick<EmailApi, "listEmailAccounts"> & {
  listCalendarAccounts: () => Promise<CalendarAccount[]>;
  listMcpServers: () => Promise<McpServer[]>;
  mcpCatalog: () => Promise<McpCatalogEntry[]>;
};

/**
 * Everything that gives a bot the owner's mail or calendar. A bot's own AgentMail address is not
 * the owner's mail, so it is left out. A part that cannot be read comes back empty.
 */
export async function loadConnected(api: ConnectedApi): Promise<Connected> {
  const [accounts, calendars, servers, catalog] = await Promise.all([
    api.listEmailAccounts().catch(() => [] as EmailAccount[]),
    api.listCalendarAccounts().catch(() => [] as CalendarAccount[]),
    api.listMcpServers().catch(() => [] as McpServer[]),
    api.mcpCatalog().catch(() => [] as McpCatalogEntry[])
  ]);
  const ids = new Set(googleMailCalendarEntries(catalog).map((e) => e.id));
  return {
    mailboxes: accounts.filter((a) => a.provider !== "agentmail"),
    calendars,
    googleApps: servers.filter((s) => s.transport === "http" && ids.has(s.name))
  };
}

export const connectedCount = (c: Connected): number => c.mailboxes.length + c.calendars.length + c.googleApps.length;

export type DisconnectApi = Pick<EmailApi, "deleteEmailAccount"> & {
  deleteCalendarAccount: (name: string) => Promise<void>;
  deleteMcpServer: (name: string, opts?: { forgetClientSecret?: boolean }) => Promise<void>;
};

export interface Disconnected {
  removed: number;
  failed: string[];
}

/**
 * "Disconnect everything": removes every mailbox, calendar and Google mail/calendar app. The
 * passwords go with them (the daemon deletes them from the secret store). One that fails does not
 * stop the rest; its name is reported. The Google client the owner made is kept: Drive may use it.
 */
export async function disconnectAll(api: DisconnectApi, connected: Connected): Promise<Disconnected> {
  const out: Disconnected = { removed: 0, failed: [] };
  const run = async (name: string, call: () => Promise<void>) => {
    try {
      await call();
      out.removed += 1;
    } catch {
      out.failed.push(name);
    }
  };
  for (const a of connected.mailboxes) {
    await run(a.name, () => api.deleteEmailAccount(a.name, { deleteInbox: false }));
  }
  for (const c of connected.calendars) {
    await run(c.name, () => api.deleteCalendarAccount(c.name));
  }
  for (const s of connected.googleApps) {
    await run(s.name, () => api.deleteMcpServer(s.name, { forgetClientSecret: false }));
  }
  return out;
}

/** Whether a Google mail or calendar app is signed in (the check after the browser step). */
export function googleSignedIn(servers: readonly McpServer[], ids: readonly string[]): boolean {
  return servers.some((s) => s.transport === "http" && ids.includes(s.name) && s.status === "connected");
}
