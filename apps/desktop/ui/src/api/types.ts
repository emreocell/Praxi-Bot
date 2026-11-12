import type { components } from "./schema";

/** Types generated from `docs/api/openapi.json` (`pnpm --filter desktop-ui gen:api`). */
type S = components["schemas"];

export type Bot = S["BotDto"];
export type BotInput = S["BotInput"];
export type Chat = S["ChatDto"];
export type Message = S["MessageDto"];
/**
 * A run.  `schedule_id` is not in the generated `RunDto` yet (the daemon only
 * reports it on the `run_started` event); typed here so it works once added.
 */
export type Run = S["RunDto"] & { schedule_id?: string | null };
export type Approval = S["ApprovalDto"];
export type Health = S["Health"];
export type UsageSummary = S["UsageSummary"];
export type UsageTotals = S["UsageTotals"];
export type EffectivePolicy = S["EffectivePolicyDto"];
export type Grant = S["GrantDto"];
export type GrantTarget = S["GrantTargetDto"];
export type Scope = S["Scope"];
export type Decision = S["Decision"];
export type SendMode = S["Mode"];
export type SendResponse = S["SendResponse"];
export type DeliveryKind = S["DeliveryKind"];
export type RememberResponse = S["RememberResponse"];
export type Schedule = S["ScheduleDto"];
export type ScheduleInput = S["ScheduleInput"];
export type ParsedSchedule = S["ParsedSchedule"];
export type AcceptedResponse = S["AcceptedResponse"];
export type ErrorBody = S["ErrorBody"];
export type ControlStateDto = S["ControlStateDto"];
export type ControlModeName = NonNullable<ControlStateDto["mode"]>;
export type ControlVerdict = S["ControlVerdict"];
export type PolicyDto = S["PolicyDto"];
export type PolicyInput = S["PolicyInput"];
export type ProvidersDto = S["ProvidersDto"];
export type ConfiguredProviderDto = S["ConfiguredProviderDto"];
export type CatalogProviderDto = S["CatalogProviderDto"];

/** The permission preset names accepted by a bot profile (`permissions.preset`). */
export type PermissionPreset =
  | "read-only"
  | "approve-destructive"
  | "read-and-draft"
  | "full";

/** Permission categories (ADR-0012) a profile can set a level for. */
export const PERMISSION_CATEGORIES = [
  "fs",
  "shell",
  "desktop",
  "browser",
  "accounts",
  "messaging",
  "money",
  "system",
  "remote",
  "script"
] as const;
export type PermissionCategoryName = (typeof PERMISSION_CATEGORIES)[number];
export type PermissionLevel = "deny" | "ask" | "allow";

// ---- SSE events (`protocol::Envelope` / `protocol::Event`) -----------------

export interface TodoItem {
  text: string;
  status: "pending" | "in_progress" | "done";
}

/**
 * The route a turn took (`route_decided.route`, tagged by `kind`); the same
 * shape `POST /v1/runs/{id}/remember` returns for the cached route.
 */
export type RouteInfo =
  | { kind: "command"; name: string }
  | {
      kind: "workflow";
      name: string;
      params?: unknown;
      confidence?: number;
    }
  | { kind: "tool_hint"; tools: string[] }
  | { kind: "skill_hint"; skills: string[] }
  | { kind: "agent" }
  /** A kind this UI does not know yet (forward compatible). */
  | { kind: "unknown" };

/** Level that decided a route: K0 rules, K1 cache, K2 retrieval, K3 classifier, K4 none. */
export type RouteLevel = "K0" | "K1" | "K2" | "K3" | "K4";

export type DaemonEvent =
  | {
      type: "run_started";
      run_id: string;
      chat_id: string;
      bot_id: string;
      /** The schedule that started this run (scheduled runs only). */
      schedule_id?: string | null;
    }
  | { type: "run_finished"; run_id: string; error: string | null }
  | { type: "message_delta"; run_id: string; text: string }
  | {
      type: "tool_call_requested";
      run_id: string;
      call_id: string;
      tool_name: string;
      input: unknown;
    }
  | {
      type: "approval_needed";
      run_id: string;
      call_id: string;
      tool_name: string;
      input: unknown;
      summary: string;
    }
  | {
      type: "tool_call_approved";
      run_id: string;
      call_id: string;
      approved_by: string;
    }
  | {
      type: "tool_call_denied";
      run_id: string;
      call_id: string;
      denied_by: string;
      reason: string | null;
    }
  | {
      type: "tool_call_completed";
      run_id: string;
      call_id: string;
      error: string | null;
      /** Redacted start of the output, at most 2 KB. */
      output_preview?: string | null;
      /** Time the tool itself ran (excludes approval waits). */
      duration_ms?: number | null;
    }
  | { type: "todo_updated"; run_id: string; items: TodoItem[] }
  | {
      type: "question_asked";
      run_id: string;
      call_id: string;
      question: string;
      options: string[];
    }
  | {
      type: "model_fallback";
      run_id: string;
      from: string;
      to: string;
      reason: string;
    }
  | {
      type: "route_decided";
      run_id: string;
      level: string;
      /** Tagged by `kind` (see {@link RouteInfo}); untrusted shape. */
      route: unknown;
      /** `0..=1`. */
      confidence: number;
      latency_ms: number;
      reason: string;
    }
  | {
      type: "workflow_step_started";
      run_id: string | null;
      workflow: string;
      step_index: number;
      kind: string;
    }
  | {
      type: "workflow_step_completed";
      run_id: string | null;
      workflow: string;
      step_index: number;
      resolved_via: string | null;
    }
  | {
      type: "workflow_drift";
      run_id: string | null;
      workflow: string;
      step_index: number;
      reason: string;
    }
  | {
      type: "control_requested";
      request_id: string;
      run_id: string;
      bot_id: string;
      /** Global physical screen pixels. */
      rect: { x: number; y: number; width: number; height: number } | null;
      label: string;
      action: string;
    }
  | { type: "error"; run_id: string; message: string; recoverable: boolean }
  | { type: "mcp_server_changed"; name: string; status: McpStatus }
  | {
      type: "subagent_started";
      run_id: string;
      parent_run_id: string;
      task: string;
      call_id?: string;
    }
  | {
      type: "subagent_finished";
      run_id: string;
      parent_run_id: string;
      state: "succeeded" | "failed" | "cancelled" | "paused";
      summary: string;
      call_id?: string;
    };

export interface Envelope {
  v: number;
  seq: number;
  ts_ms: number;
  /** Present on every event emitted by a child (subagent) run. */
  parent_run_id?: string;
  event: DaemonEvent;
}

/** An envelope with the daemon-wide cursor (`gseq`) it was delivered under. */
export interface GlobalEnvelope extends Envelope {
  gseq: number;
}

// ---- MCP management API (`/v1/mcp/*`, `/v1/secrets/*`) ---------------------

export type McpTransport = S["McpTransportKind"];
export type McpStatus = S["McpStatus"];
export type McpServer = S["McpServerDto"];
/** Body of `POST /v1/mcp/servers`. */
export type McpServerConfig = S["McpServerInput"] & { name: string };
/** Body of `PUT /v1/mcp/servers/{name}` (every field optional). */
export type McpServerPatch = S["McpServerPatch"];
export type McpTool = S["McpToolDto"];
export type McpRequiredSecret = S["McpRequiredSecretDto"];
export type McpCatalogEntry = S["McpCatalogEntryDto"];

// ---- E-mail accounts (`/v1/email/*`) ---------------------------------------

export type EmailAccount = S["EmailAccountDto"];
/** Body of `POST /v1/email/accounts` and `POST /v1/email/test`. */
export type EmailAccountInput = S["EmailAccountInput"];
/** Body of `PUT /v1/email/accounts/{name}` (every field optional). */
export type EmailAccountPatch = S["EmailAccountPatch"];
export type EmailSecurity = S["EmailSecurity"];
export type EmailProbeStatus = S["EmailProbeStatus"];
export type EmailTestReport = S["EmailTestReport"];
