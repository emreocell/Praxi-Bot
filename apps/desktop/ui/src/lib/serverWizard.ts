import type { HostSetupJob, StartHostSetupInput } from "../api/types";
import { failure, stageStates, STAGES, type FailureCode, type StageState } from "./hostSetup.ts";

/**
 * The guided "Add a server" wizard: which page is showing and what the person
 * typed. Pure functions only, so the whole flow is testable without a screen.
 */
export type WizardPage = "intro" | "choose" | "oracle" | "home" | "details" | "run";
export type ServerPath = "ssh" | "oracle" | "home";
export type AuthMode = "key" | "agent";

export interface WizardState {
  page: WizardPage;
  path: ServerPath | null;
  host: string;
  user: string;
  port: string;
  auth: AuthMode;
  keyPath: string;
  /** Oracle walkthrough checklist ticks (ids from ORACLE_STEPS). */
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
  oracleDone: []
};

export type WizardAction =
  | { type: "next" }
  | { type: "back" }
  | { type: "choose"; path: ServerPath }
  | { type: "set"; field: "host" | "user" | "port" | "keyPath"; value: string }
  | { type: "auth"; auth: AuthMode }
  | { type: "tick"; id: string }
  | { type: "run" }
  | { type: "edit" };

/** Text steps of the free-server walkthrough (no screenshots; nothing secret is handled here). */
export const ORACLE_STEPS = ["account", "instance", "ssh_key", "firewall", "address"] as const;

export function wizardReducer(s: WizardState, a: WizardAction): WizardState {
  switch (a.type) {
    case "next":
      if (s.page === "intro") return { ...s, page: "choose" };
      if (s.page === "oracle") return { ...s, page: "details", user: "ubuntu" };
      return s;
    case "choose":
      if (a.path === "oracle") return { ...s, path: a.path, page: "oracle", user: "ubuntu" };
      if (a.path === "home") return { ...s, path: a.path, page: "home" };
      return { ...s, path: a.path, page: "details" };
    case "back":
      if (s.page === "choose") return { ...s, page: "intro" };
      if (s.page === "oracle" || s.page === "home") return { ...s, page: "choose" };
      if (s.page === "details") {
        return { ...s, page: s.path === "oracle" ? "oracle" : s.path === "home" ? "home" : "choose" };
      }
      if (s.page === "run") return { ...s, page: "details" };
      return s;
    case "set":
      return { ...s, [a.field]: a.value };
    case "auth":
      return { ...s, auth: a.auth };
    case "tick":
      return {
        ...s,
        oracleDone: s.oracleDone.includes(a.id)
          ? s.oracleDone.filter((x) => x !== a.id)
          : [...s.oracleDone, a.id]
      };
    case "run":
      return { ...s, page: "run" };
    case "edit":
      return { ...s, page: "details" };
  }
}

/** Number shown as "Step n of 4" (null: no counter on this page). */
export function stepNumber(page: WizardPage): number | null {
  switch (page) {
    case "choose":
      return 1;
    case "oracle":
    case "home":
    case "details":
      return 2;
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

export function toInput(s: WizardState): StartHostSetupInput {
  return {
    host: s.host.trim(),
    user: s.user.trim() || "ubuntu",
    port: Number(s.port),
    key_path: s.auth === "key" ? s.keyPath.trim() : null
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
  else if (f.code === "auth") {
    code = d.includes("password") ? "password_only" : typed.auth === "agent" ? "auth_agent" : "auth_key";
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
