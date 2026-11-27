import type { HostSetupJob, StartHostSetupInput } from "../api/types";
import { failure, stageStates, STAGES, type FailureCode, type StageState } from "./hostSetup.ts";

/**
 * The guided "Add a server" wizard: which page is showing and what the person
 * typed. Pure functions only, so the whole flow is testable without a screen.
 */
export type WizardPage =
  | "intro"
  | "local"
  | "need"
  | "oracle"
  | "vps"
  | "home"
  | "have"
  | "checks"
  | "details"
  | "run";
/** What the person picked on the first screen. */
export type ServerPath = "local" | "have" | "need";
export type AuthMode = "key" | "agent";

export interface WizardState {
  page: WizardPage;
  path: ServerPath | null;
  host: string;
  user: string;
  port: string;
  auth: AuthMode;
  keyPath: string;
  /**
   * Only an intent: the basic tool set is never installed by the wizard. When on, the app opens the
   * Tools plan (dry run, then confirm) once the server is added. Off until the owner switches it on.
   */
  basePacks: boolean;
  /** Guide checklist ticks (ids from GUIDE_STEPS), for the guide that is open. */
  oracleDone: string[];
}

export const initialWizard: WizardState = {
  page: "intro",
  path: null,
  host: "",
  user: "ubuntu",
  port: "22",
  auth: "key",
  keyPath: "",
  basePacks: false,
  oracleDone: []
};

export type GuideId = "oracle" | "vps" | "home";

export type WizardAction =
  | { type: "choose"; path: ServerPath }
  | { type: "guide"; id: GuideId }
  | { type: "have" }
  | { type: "advanced" }
  | { type: "back" }
  | { type: "set"; field: "host" | "user" | "port" | "keyPath"; value: string }
  | { type: "auth"; auth: AuthMode }
  | { type: "basePacks"; on: boolean }
  | { type: "tick"; id: string }
  | { type: "run" }
  | { type: "checks" }
  | { type: "edit" };

/** Text steps of the free-server walkthrough (no screenshots; nothing secret is handled here). */
export const ORACLE_STEPS = ["account", "instance", "firewall", "console"] as const;
export const VPS_STEPS = ["provider", "create", "console"] as const;
export const HOME_STEPS = ["pc", "ubuntu", "internet", "terminal"] as const;
export const GUIDE_STEPS: Record<GuideId, readonly string[]> = {
  oracle: ORACLE_STEPS,
  vps: VPS_STEPS,
  home: HOME_STEPS
};

export function wizardReducer(s: WizardState, a: WizardAction): WizardState {
  switch (a.type) {
    case "choose":
      if (a.path === "local") return { ...s, path: a.path, page: "local" };
      if (a.path === "have") return { ...s, path: a.path, page: "have" };
      return { ...s, path: a.path, page: "need" };
    case "guide":
      return { ...s, page: a.id, oracleDone: [] };
    case "have":
      return { ...s, path: "have", page: "have" };
    case "advanced":
      return { ...s, page: "details" };
    case "back":
      switch (s.page) {
        case "local":
        case "need":
        case "have":
          return { ...s, page: "intro" };
        case "oracle":
        case "vps":
        case "home":
          return { ...s, page: "need" };
        case "details":
          return { ...s, page: "have" };
        case "run":
          return { ...s, page: "details" };
        case "checks":
          return { ...s, page: "have" };
        default:
          return s;
      }
    case "set":
      return { ...s, [a.field]: a.value };
    case "auth":
      return { ...s, auth: a.auth };
    case "basePacks":
      return { ...s, basePacks: a.on };
    case "tick":
      return {
        ...s,
        oracleDone: s.oracleDone.includes(a.id)
          ? s.oracleDone.filter((x) => x !== a.id)
          : [...s.oracleDone, a.id]
      };
    case "run":
      return { ...s, page: "run" };
    case "checks":
      return { ...s, page: "checks" };
    case "edit":
      return { ...s, page: "details" };
  }
}

/** Pages worth coming back to after the dialog was closed (never one that holds a running job). */
const RESUMABLE: readonly WizardPage[] = ["need", "oracle", "vps", "home", "have", "details"];
const STORE_KEY = "praxi.addServer.v1";

type KeyValueStore = Pick<Storage, "getItem" | "setItem" | "removeItem">;

function defaultStore(): KeyValueStore | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

/**
 * Remember only where the person was and which guide boxes they ticked. Nothing typed
 * (address, user, key path) and no pairing code is ever stored.
 */
export function saveWizard(s: WizardState, store: KeyValueStore | null = defaultStore()): void {
  try {
    if (!store) return;
    if (!RESUMABLE.includes(s.page)) {
      store.removeItem(STORE_KEY);
      return;
    }
    store.setItem(STORE_KEY, JSON.stringify({ page: s.page, path: s.path, oracleDone: s.oracleDone }));
  } catch {
    /* storage can be blocked: the wizard just starts from the top next time */
  }
}

export function clearWizard(store: KeyValueStore | null = defaultStore()): void {
  try {
    store?.removeItem(STORE_KEY);
  } catch {
    /* nothing to clear */
  }
}

export function loadWizard(store: KeyValueStore | null = defaultStore()): WizardState {
  try {
    const raw = store?.getItem(STORE_KEY);
    if (!raw) return initialWizard;
    const v = JSON.parse(raw) as { page?: unknown; path?: unknown; oracleDone?: unknown };
    const page = RESUMABLE.find((p) => p === v.page);
    if (!page) return initialWizard;
    const path = (["local", "have", "need"] as const).find((p) => p === v.path) ?? null;
    const done = Array.isArray(v.oracleDone) ? v.oracleDone.filter((x): x is string => typeof x === "string").slice(0, 12) : [];
    return { ...initialWizard, page, path, oracleDone: done };
  } catch {
    return initialWizard;
  }
}

/** Which of the three progress dots is lit: 1 choose, 2 set up, 3 check (null: no dots). */
export const TOTAL_STEPS = 3;
export function stepNumber(page: WizardPage): number | null {
  switch (page) {
    case "intro":
    case "need":
      return 1;
    case "local":
    case "oracle":
    case "vps":
    case "home":
    case "have":
    case "details":
      return 2;
    case "checks":
    case "run":
      return 3;
    default:
      return null;
  }
}

export type FieldProblem =
  | "hostEmpty"
  | "hostScheme"
  | "hostSpaces"
  | "hostUser"
  | "hostPort"
  | "userInvalid"
  | "portInvalid"
  | "keyEmpty"
  | "keyPublic";

export interface FormCheck {
  host: FieldProblem | null;
  user: FieldProblem | null;
  port: FieldProblem | null;
  key: FieldProblem | null;
  ok: boolean;
}

/**
 * Live validation of the details page. It only catches typing mistakes that
 * would waste a minute of waiting: the server has the last word.
 */
export function checkForm(s: Pick<WizardState, "host" | "user" | "port" | "auth" | "keyPath">): FormCheck {
  const host = s.host.trim();
  let hostProblem: FieldProblem | null = null;
  if (!host) hostProblem = "hostEmpty";
  else if (/^[a-z][a-z0-9+.-]*:\/\//i.test(host)) hostProblem = "hostScheme";
  else if (/\s/.test(host)) hostProblem = "hostSpaces";
  else if (host.includes("@")) hostProblem = "hostUser";
  else if (/^[^:[\]]+:\d+$/.test(host)) hostProblem = "hostPort";

  const user = s.user.trim();
  const userProblem: FieldProblem | null =
    user === "" || /^[a-z_][a-z0-9_.-]{0,31}$/i.test(user) ? null : "userInvalid";

  const portNumber = Number(s.port);
  const portProblem: FieldProblem | null =
    Number.isInteger(portNumber) && portNumber >= 1 && portNumber <= 65535 ? null : "portInvalid";

  let key: FieldProblem | null = null;
  if (s.auth === "key") {
    const k = s.keyPath.trim();
    if (!k) key = "keyEmpty";
    else if (/\.pub$/i.test(k)) key = "keyPublic";
  }
  return {
    host: hostProblem,
    user: userProblem,
    port: portProblem,
    key,
    ok: !hostProblem && !userProblem && !portProblem && !key
  };
}

/**
 * The pairing code out of whatever was pasted: the server prints it among other
 * lines (and a prompt after it). The code is one unbroken word. Returns null when
 * there is no `bot-pair:` code in the text.
 */
export function extractTicket(text: string): string | null {
  const m = /bot-pair:[A-Za-z2-7]+/.exec(text);
  return m ? m[0] : null;
}

export function toInput(s: WizardState): StartHostSetupInput {
  return {
    host: s.host.trim(),
    user: s.user.trim() || "ubuntu",
    port: Number(s.port),
    key_path: s.auth === "key" ? s.keyPath.trim() : null,
    base_packs: s.basePacks
  };
}

/** A failure explained in plain words: refines the engine's coarse code with what the person typed. */
export type Diagnosis =
  | "auth_key"
  | "auth_agent"
  | "key_permissions"
  | "key_passphrase"
  | "password_only"
  | "port_closed"
  | "port_refused"
  | "bad_address"
  | "wrong_port"
  | FailureCode;

export function diagnose(
  job: HostSetupJob,
  typed: Pick<WizardState, "auth" | "port" | "user">
): { code: Diagnosis; stepId: string } | null {
  const f = failure(job);
  if (!f) return null;
  const d = `${f.step.detail} ${f.step.hint ?? ""}`.toLowerCase();
  let code: Diagnosis = f.code;
  if (d.includes("unprotected private key") || d.includes("bad permissions")) code = "key_permissions";
  else if (d.includes("passphrase")) code = "key_passphrase";
  else if (f.step.detail.toLowerCase().includes("only offered password authentication")) code = "password_only";
  else if (f.code === "auth") {
    code = typed.auth === "agent" ? "auth_agent" : "auth_key";
  } else if (f.code === "network") {
    if (d.includes("resolve") || d.includes("name or service") || d.includes("no such host")) code = "bad_address";
    else if (d.includes("refused")) code = typed.port !== "22" ? "wrong_port" : "port_refused";
    else if (d.includes("timed out") || d.includes("timeout")) code = "port_closed";
  }
  return { code, stepId: f.step.id };
}

/** Rows of the friendly pre-flight checklist and the engine steps behind each. */
export const CHECKS = ["reach", "login", "system", "room", "ports"] as const;
export type CheckId = (typeof CHECKS)[number];
export type CheckState = "pending" | "running" | "pass" | "warn" | "fail";

const CHECK_STEPS: Record<CheckId, (id: string) => boolean> = {
  reach: (id) => id === "hostkey",
  login: (id) => id === "connect",
  system: (id) => ["preflight.os", "preflight.arch", "preflight.systemd", "preflight.sudo"].includes(id),
  room: (id) => id === "preflight.ram" || id === "preflight.disk",
  ports: (id) => id === "preflight.port" || id === "preflight.https"
};

export function checklist(job: HostSetupJob): { id: CheckId; state: CheckState }[] {
  let open = true;
  return CHECKS.map((id) => {
    const steps = job.steps.filter((s) => CHECK_STEPS[id](s.id));
    if (steps.some((s) => s.status === "fail")) {
      open = false;
      return { id, state: "fail" };
    }
    if (steps.length > 0) {
      return { id, state: steps.some((s) => s.status === "warn") ? "warn" : "pass" };
    }
    const active = job.state === "running" || job.state === "awaiting_host_key";
    if (open && active) {
      open = false;
      return { id, state: "running" };
    }
    return { id, state: "pending" };
  });
}

/** 0-100 for the install bar: finished rows count fully, the open one half. */
export function progressPercent(job: HostSetupJob): number {
  if (job.state === "succeeded") return 100;
  const states = stageStates(job);
  const score = STAGES.reduce((n, s) => n + weight(states[s]), 0);
  return Math.min(95, Math.round((score / STAGES.length) * 100));
}

function weight(s: StageState): number {
  switch (s) {
    case "done":
    case "skipped":
    case "warn":
      return 1;
    case "running":
    case "waiting":
      return 0.5;
    default:
      return 0;
  }
}

/** Log lines for the expandable log: technical, from the engine, never any secret. */
export function logLines(job: HostSetupJob): string[] {
  const out: string[] = [];
  for (const s of job.steps) out.push(`[${s.status}] ${s.title}: ${s.detail}`);
  for (const n of job.notes) out.push(n);
  return out;
}
