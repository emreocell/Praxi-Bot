import type { components } from "./schema";

/** Types generated from `docs/api/openapi.json` (`pnpm --filter desktop-ui gen:api`). */
type S = components["schemas"];

export type Bot = S["BotDto"];
export type BotInput = S["BotInput"];
export type Chat = S["ChatDto"];
export type Message = S["MessageDto"];
export type Run = S["RunDto"];
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

export type DaemonEvent =
  | { type: "run_started"; run_id: string; chat_id: string; bot_id: string }
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
