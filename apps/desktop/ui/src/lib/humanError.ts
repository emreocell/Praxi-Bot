import i18n from "i18next";
import type { StartupDiagnostics } from "../api/connection.ts";
import { hostRefusalKey } from "./hostMode.ts";

/**
 * The one place that turns "anything that went wrong" into words a person can act on:
 * a title (what happened), a hint (what to do), an optional one-click action, and the raw text
 * kept apart as `details` (shown only behind a "Details" disclosure, never as primary text).
 *
 * Input can be a daemon error envelope (`ApiError`: status + stable `code` + message), a network
 * failure, a Tauri string error, a tool-result error text from the chat step chain, or anything
 * thrown. Known codes map through a table; unknown ones fall back to the status class, then to a
 * generic sentence.
 */

/** Every `code` the daemon can put in its error envelope (checked against the Rust sources by a test). */
// Keep this list sorted, one code per line: parallel branches then add lines in different places.
export const DAEMON_ERROR_CODES = [
  "admin_rate_limit",
  "approval_once_only",
  "attachment_too_large",
  "bad_composio_request",
  "bad_email_config",
  "bad_engine",
  "bad_mcp_config",
  "bad_profile",
  "bad_request",
  "bad_schedule",
  "bad_voice_config",
  "bad_web_config",
  "busy",
  "component_conflict",
  "composio_error",
  "composio_not_configured",
  "composio_rate_limited",
  "conflict",
  "could_not_verify",
  "disabled",
  "email_provider_error",
  "engine_owner_only",
  "forbidden",
  "host_admin_forbidden",
  "host_admin_required",
  "host_link_failed",
  "host_not_permitted",
  "host_outdated",
  "host_setup_conflict",
  "host_unreachable",
  "input_busy",
  "input_rate_limited",
  "install_copy_only",
  "internal",
  "invalid_mcp_grant",
  "invalid_signature",
  "invalid_skill",
  "invalid_skill_name",
  "invite_rate_limit",
  "knowledge_busy",
  "login_busy",
  "login_files_changed",
  "login_forbidden",
  "login_manual",
  "login_stale",
  "login_untrusted_location",
  "mcp_approval_timeout",
  "mcp_rate_limit",
  "message_not_editable",
  "message_not_last",
  "modified_since",
  "not_found",
  "not_installed",
  "owner_key_required",
  "payload_too_large",
  "precondition_required",
  "prerequisite_missing",
  "reload_failed",
  "repair_failed",
  "restore_in_progress",
  "resync_required",
  "run_files_in_use",
  "schedule_moved",
  "schedule_retry_limited",
  "secret_request_exists",
  "secret_request_gone",
  "secret_store_failed",
  "secrets_unavailable",
  "session_expired",
  "setup_required",
  "skill_changed",
  "skill_conflict",
  "skill_contains_secret",
  "skill_not_found",
  "skill_source_unreachable",
  "skill_storage",
  "skill_too_large",
  "spend_limit_reached",
  "starting",
  "terminal_busy",
  "terminal_gone",
  "terminal_start_failed",
  "terminal_unsupported",
  "too_many_requests",
  "too_many_skills",
  "too_many_streams",
  "unauthorized",
  "unavailable",
  "unsupported_attachment",
  "unsupported_media_type",
  "version_conflict",
  "view_media_type",
  "view_quota",
  "voice_not_configured",
  "voice_upstream_failed",
] as const;

export type ErrorAction = "retry" | "reload" | "restartService" | "settings" | "fixEngine";

export interface HumanError {
  /** Stable key: the daemon code, a client code, or `generic`. */
  code: string;
  title: string;
  hint: string;
  action?: ErrorAction;
  /** The raw text for the "Details" disclosure and for copying. */
  details: string;
}

export type ProviderTestProblem = "provider_endpoint" | "provider_model";

/** Classify provider probe output in its own context, where a 404 can mean a bad endpoint. */
export function providerTestProblem(raw: string): ProviderTestProblem | undefined {
  if (/\bmodel_not_found\b|\bunknown model\b|\bmodel\b[^\r\n]{0,160}\b(?:not found|does not exist)\b|\bmodels\/[^\s'"]+ is not found\b/i.test(raw)) {
    return "provider_model";
  }
  if (/\bprovider error \(404\)|\bHTTP 404\b|\binvalid (?:base )?URL\b|\b(?:relative URL without a base|URL scheme is not allowed)\b/i.test(raw)) {
    return "provider_endpoint";
  }
  return undefined;
}

/** Render a provider-specific title and hint without including the probe's raw output. */
export function humanProviderTestError(problem: ProviderTestProblem): HumanError {
  return pack(`codes.${problem}`, problem, "");
}

/** Codes that are worth a Retry / Reload / restart button. */
const ACTIONS: Record<string, ErrorAction> = {
  network: "restartService",
  unauthorized: "restartService",
  reload_failed: "restartService",
  unavailable: "retry",
  starting: "retry",
  internal: "retry",
  busy: "retry",
  hostBusy: "retry",
  screenNoConfirm: "retry",
  http_error: "retry",
  timeout: "retry",
  sendFailed: "retry",
  conflict: "reload",
  version_conflict: "reload",
  precondition_required: "reload",
  setup_required: "settings",
  secret_store_failed: "retry",
  composio_not_configured: "settings",
  disabled: "settings",
  engineNeedsNative: "fixEngine",
  cliSignedOut: "settings",
  cliNotInstalled: "settings"
};

/** Status classes for a code the table does not know. */
const STATUS_CODE: Record<number, string> = {
  400: "bad_request",
  401: "unauthorized",
  403: "forbidden",
  404: "not_found",
  409: "conflict",
  413: "payload_too_large",
  415: "unsupported_media_type",
  428: "precondition_required",
  429: "too_many_requests",
  503: "unavailable"
};

/** Daemon refusals with a concrete reason: they beat the bare status/code class (a 409 `conflict` is not just "reload"). */
const REASONS: ReadonlyArray<readonly [RegExp, string]> = [
  [/screen:(input|view) denied|input grant for this device/i, "screenInputDenied"],
  [/only the local user (may do this|can clear it)|local user stopped the takeover/i, "localOnly"],
  [/disabled in safe mode|unavailable in safe mode/i, "safeModeBlocked"],
  [/no input backend/i, "noInputBackend"],
  [/did not confirm|pressed-key reset did not complete|did not answer in time/i, "screenNoConfirm"],
  [/executor is busy|is busy; (try|retry)|previous input still draining|already in progress/i, "hostBusy"]
];

/** Raw-text patterns, for tool results and plain errors that carry no code. First match wins. */
const PATTERNS: ReadonlyArray<readonly [RegExp, string, ((m: RegExpExecArray) => Record<string, string>)?]> = [
  // A coding CLI that cannot answer yet (the daemon's stable markers, see `cli_exec_error_for`).
  [/engine_needs_native|only runs with its own tools|only works with its own tools|native_tools_open|isolation flags/i, "engineNeedsNative"],
  [/\(signed_out\)|is signed out/i, "cliSignedOut"],
  [/\(not_installed\)|is not installed or needs an update/i, "cliNotInstalled"],
  [/\(quota_exceeded|usage limit reached/i, "cliQuota"],
  [/user is controlling the screen|observation paused/i, "toolScreenControl"],
  [/wrong computer:/i, "toolWrongMachine"],
  [/waiting for the pc|pc desktop unavailable|computer is offline|desktop backend is unavailable|peer is not connected/i, "toolTargetOffline"],
  [/`?([\w.-]+)`? was not found on path|command not found: ?`?([\w.-]+)/i, "toolMissingProgram", (m) => ({ name: m[1] ?? m[2] ?? "" })],
  [/managed on the host itself/i, "hostManaged"],
  [/no such file or directory|cannot find the (file|path)|os error 2(?![0-9])/i, "toolNotFound"],
  [/expected_version|reload and retry/i, "codes.version_conflict"],
  [/spend_limit_reached[\s\S]*saved limit could not be read/, "codes.spend_limit_unreadable"],
  [/spend_limit_reached/, "codes.spend_limit_reached"],
  [/providers_require_app_daemon|_outside_tauri|daemon_unavailable/i, "desktop_only"],
  [/denied|not allowed by the user|user rejected/i, "toolDenied"],
  [/failed to fetch|networkerror|load failed|econnrefused|enotfound|fetch failed|connection (refused|reset|lost)/i, "network"],
  [/timed? ?out|etimedout/i, "timeout"],
  [/^step 'model' failed/i, "modelFailed"]
];

/** `t` that still returns a string before i18next is initialised (plain-node tests). */
function tr(key: string, params?: Record<string, string>): string {
  const v = i18n.t(key, params);
  return typeof v === "string" ? v : key;
}

function hasKey(key: string): boolean {
  return i18n.exists(`errors.${key}`);
}

function pack(key: string, code: string, details: string, params?: Record<string, string>): HumanError {
  const base = key.startsWith("codes.") || hasKey(`${key}.title`) ? key : "generic";
  return {
    code,
    title: tr(`errors.${base}.title`, params),
    hint: tr(`errors.${base}.hint`, params),
    action: ACTIONS[code] ?? ACTIONS[base.replace("codes.", "")],
    details
  };
}

interface Envelope {
  host?: unknown;
  status?: unknown;
  code?: unknown;
  message?: unknown;
}

function rawOf(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  if (err && typeof err === "object" && typeof (err as Envelope).message === "string") {
    return (err as Envelope).message as string;
  }
  try {
    return JSON.stringify(err) ?? String(err);
  } catch {
    return String(err);
  }
}

/** Describe any error for display. Pure apart from reading the current UI language. */
export function humanError(err: unknown): HumanError {
  const raw = rawOf(err);
  const env = (err && typeof err === "object" ? err : {}) as Envelope;
  const status = typeof env.status === "number" ? env.status : undefined;
  const code = typeof env.code === "string" ? env.code : undefined;
  const details = [status ? `HTTP ${status}` : "", code ?? "", raw].filter(Boolean).join(" · ");

  // A paired host's refusal: one sentence naming the server (the raw daemon text stays in details).
  const host = typeof env.host === "string" ? env.host : undefined;
  // The host never answered (no connection, or the proxy timed out waiting for it): not a refusal.
  if (host && (code === "network" || status === 502 || status === 504)) {
    return {
      code: "host_silent",
      title: tr("errors.hostSilent.title", { name: host }),
      hint: tr("errors.hostSilent.hint", { name: host }),
      action: "retry",
      details
    };
  }
  const refusal = host ? hostRefusalKey(status, raw) : null;
  if (host && refusal) {
    return {
      code: "host_refusal",
      title: tr(`hostMode.refusal.${refusal}`, { name: host, reason: raw }),
      hint: "",
      details
    };
  }
  // The concrete-reason table only describes daemon refusals: a host refusal, or a conflict /
  // unavailable answer. A tool result or plain error that merely contains these words is not one.
  if (host || code === "conflict" || code === "unavailable") {
    for (const [re, key] of REASONS) {
      if (re.test(raw)) {
        return pack(key, key, details);
      }
    }
  }
  if (code === "network") {
    return pack("network", "network", details);
  }
  if (code && (DAEMON_ERROR_CODES as readonly string[]).includes(code)) {
    return pack(`codes.${code}`, code, details);
  }
  // Pattern match first: it is more specific than a bare status class (a tool failure inside a 200,
  // a Tauri string error, a message that names the program that is missing).
  for (const [re, key, params] of PATTERNS) {
    const m = re.exec(raw);
    if (m) {
      return pack(key, key.replace("codes.", ""), details, params?.(m));
    }
  }
  if (status !== undefined && status > 0) {
    const mapped = STATUS_CODE[status] ?? (status >= 500 ? "internal" : undefined);
    if (mapped) {
      return pack(`codes.${mapped}`, mapped, details);
    }
    return pack("http_error", "http_error", details);
  }
  return pack("generic", "generic", details);
}

/** One line for toasts: "Title. Hint". */
export function humanLine(err: unknown): string {
  const h = humanError(err);
  return h.hint ? `${h.title}. ${h.hint}` : h.title;
}

/**
 * A failed tool step in the chat: one human sentence (what the bot could not do and why).
 * `text` is the tool result's error text; unknown text gets the generic "could not finish this step".
 */
export function humanToolError(text: string): HumanError {
  const h = humanError(text);
  return h.code === "generic" ? pack("toolFailed", "toolFailed", text) : h;
}

/** The local service's state, for the banner: starting, stopped, or the wrong target. */
export type ServiceProblem = "starting" | "down" | "wrongTarget";

export function humanService(problem: ServiceProblem, raw = ""): HumanError {
  const key = problem === "starting" ? "serviceStarting" : problem === "down" ? "serviceDown" : "serviceWrongTarget";
  return {
    code: key,
    title: tr(`errors.${key}.title`),
    hint: tr(`errors.${key}.hint`),
    action: problem === "starting" ? undefined : "restartService",
    details: raw
  };
}

/** NTSTATUS codes a Windows process ends with when it dies before it can print anything. */
const EXIT_KINDS: Record<number, "exitedDll" | "exitedBadImage" | "exitedCrash"> = {
  [-1073741515]: "exitedDll", // 0xC0000135 STATUS_DLL_NOT_FOUND
  [-1073741701]: "exitedBadImage", // 0xC000007B STATUS_INVALID_IMAGE_FORMAT
  [-1073741502]: "exitedDll", // 0xC0000142 STATUS_DLL_INIT_FAILED
  [-1073741795]: "exitedCrash", // 0xC000001D STATUS_ILLEGAL_INSTRUCTION
  [-1073741819]: "exitedCrash", // 0xC0000005 STATUS_ACCESS_VIOLATION
  [-1073741571]: "exitedCrash", // 0xC00000FD STATUS_STACK_OVERFLOW
  [-1073740791]: "exitedCrash" // 0xC0000409 STATUS_STACK_BUFFER_OVERRUN
};

/** `-1073741515` as `-1073741515 (0xC0000135)`; small codes as they are. */
export function exitCodeLabel(code: number): string {
  return code < 0 ? `${code} (0x${(code >>> 0).toString(16).toUpperCase()})` : String(code);
}

/** Which `errors.startup.*` text describes this start-up state. */
export function startupKey(d: StartupDiagnostics): string {
  switch (d.stage) {
    case "sidecar_missing": return "sidecarMissing";
    case "sidecar_blocked": return "sidecarBlocked";
    case "spawn_failed": return "spawnFailed";
    case "port_in_use": return "portInUse";
    case "unreachable": return "unreachable";
    case "not_responding": return "notResponding";
    case "exited": return d.exit_code === null ? "exited" : (EXIT_KINDS[d.exit_code] ?? "exited");
    case "spawning": return "spawning";
    default: return "starting";
  }
}

/** Everything worth pasting into a bug report about a service that does not start. */
export function startupReport(d: StartupDiagnostics, raw = ""): string {
  const lines = [
    `Praxi ${d.app_version} on ${d.os}`,
    `stage: ${d.stage} after ${Math.round(d.elapsed_ms / 1000)} s`,
    `service: ${d.sidecar} (file present: ${d.sidecar_exists ? "yes" : "no"})`,
    `address: ${d.base_url}`,
    `data folder: ${d.data_dir}`,
    `log: ${d.log_path}`
  ];
  if (d.pid !== null) lines.push(`process id: ${d.pid}`);
  if (d.exit_code !== null) lines.push(`exit code: ${exitCodeLabel(d.exit_code)}`);
  if (d.error) lines.push(`error: ${d.error}`);
  if (raw && raw !== d.error) lines.push(`app said: ${raw}`);
  if (d.log_tail.length > 0) lines.push("", "last lines of the service log:", ...d.log_tail);
  return lines.join("\n");
}

/** The state of a local service that is slow or does not start, in words, with the facts behind it. */
export function humanStartup(d: StartupDiagnostics, raw = ""): HumanError {
  const key = startupKey(d);
  const params = {
    code: d.exit_code === null ? "" : exitCodeLabel(d.exit_code),
    port: String(d.port),
    file: d.sidecar
  };
  return {
    code: `startup.${key}`,
    title: tr(`errors.startup.${key}.title`, params),
    hint: tr(`errors.startup.${key}.hint`, params),
    action: undefined,
    details: startupReport(d, raw)
  };
}
