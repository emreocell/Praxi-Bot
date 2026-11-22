import type { components } from "./schema";

/** Types generated from `docs/api/openapi.json` (`pnpm --filter desktop-ui gen:api`). */
type S = components["schemas"];
export type SearchPage = S["SearchPage"];
export type WorkflowProposal = S["WorkflowProposal"];
export type WorkflowDefinition = S["WorkflowDto"];
export type SaveWorkflow = S["SaveWorkflow"];
export type RunWorkflow = S["RunWorkflow"];
export type WorkflowRun = S["WorkflowRun"];
export type KnowledgeSource = S["KnowledgeSourceDto"];
export type KnowledgeSourceInput = S["KnowledgeSourceInput"];
export type KnowledgeSourceKind = S["KnowledgeSourceKind"];
export type KnowledgeSourceChange = S["KnowledgeSourceChange"];
export type KnowledgeMutation = S["KnowledgeMutation"];
export type KnowledgeSettings = S["KnowledgeSettingsDto"];
export type KnowledgeSettingsInput = S["KnowledgeSettingsInput"];
export type KnowledgeEmailCapture = S["KnowledgeEmailCapture"];
export type KnowledgeEmbeddingProgress = S["KnowledgeEmbeddingProgress"];
export type KnowledgeEmbeddingApproval = S["EmbeddingApprovalChallenge"];
export type KnowledgeEmbeddingGrantTarget = S["EmbeddingGrantTarget"];
export type MemoryKind = S["MemoryKindDto"];
export type MemoryTrust = S["MemoryTrustDto"];
export type MemoryItem = S["MemoryItemDto"];
export type CreateMemoryInput = S["CreateMemoryInput"];
export type UpdateMemoryInput = S["UpdateMemoryInput"];
export type MemoryPreviewHit = S["MemoryPreviewHitDto"];
export type MemoryPreview = S["MemoryPreviewDto"];

export type Bot = S["BotDto"];
export type BotInput = S["BotInput"];
export type BotPatchInput = S["BotPatchInput"];
export type Chat = S["ChatDto"];
export type UpdateChatInput = S["UpdateChat"];
export type Message = S["MessageDto"];
export type Attachment = S["AttachmentDto"];
export type RunUsage = S["RunUsageDto"];
/**
 * A run.  `schedule_id` is not in the generated `RunDto` yet (the daemon only
 * reports it on the `run_started` event); typed here so it works once added.
 */
export type Run = S["RunDto"] & { schedule_id?: string | null };
export type Approval = S["ApprovalDto"];
export type StopRunResult = S["StopRunResult"];
export type StopOutcome = S["StopOutcome"];
export type StopScope = S["StopScope"];
export type Health = S["Health"];
export type HostLinkInfo = S["HostLinkInfo"];
export type PcRequestInfo = S["PcRequestInfo"];
export type PcRequestList = S["PcRequestList"];
export type HostLinkList = S["HostLinkList"];
export type StartHostSetupInput = S["StartHostSetupInput"];
export type HostSetupJob = S["HostSetupJob"];
export type HostSetupStep = S["HostSetupStep"];
export type SshKeyHint = S["SshKeyHint"];
export type HostLinkConnection = S["HostLinkConnection"];
export type CopyPlan = S["CopyPlanDto"];
export type CopyRequest = S["CopyRequest"];
export type CopyReport = S["CopyReport"];
export type CopyItemResult = S["CopyItemResult"];
export type AdminState = S["AdminStateDto"];
export type AdminProviderInput = S["AdminProviderInput"];
export type AdminProvider = S["AdminProviderDto"];
export type AdminCliEngine = S["AdminCliEngineDto"];
export type AdminCliInstall = S["AdminCliInstallDto"];
export type AdminCliLogin = S["AdminCliLoginDto"];
export type HomeInfo = S["HomeDto"];
export type StopRunsInput = S["StopRunsInput"];
export type StopRunsResponse = S["StopRunsResponse"];
export type UsageSummary = S["UsageSummary"];
export type ModelUsage = S["ModelUsage"];
export type ProviderUsage = S["ProviderUsage"];
export type DailyUsage = S["DailyUsage"];
export type SpendLimit = S["SpendLimitDto"];
export type BotSpendLimits = S["BotSpendLimitsDto"];
export type UsageTotals = S["UsageTotals"];
export type EffectivePolicy = S["EffectivePolicyDto"];
export type Grant = S["GrantDto"];
export type GrantTarget = S["GrantTargetDto"];
export type Scope = S["Scope"];
export type Decision = S["Decision"];
export type SendMode = S["Mode"];
export type SendResponse = S["SendResponse"];
export type ResendResponse = S["ResendResponse"];
export type DeliveryKind = S["DeliveryKind"];
export type RememberResponse = S["RememberResponse"];
export type Schedule = S["ScheduleDto"];
export type ScheduleInput = S["ScheduleInput"];
export type ParsedSchedule = S["ParsedSchedule"];
export type PreviewSchedule = S["PreviewSchedule"];
export type ScheduleHistoryEntry = S["ScheduleHistoryEntryDto"];
export type AcceptedResponse = S["AcceptedResponse"];
export type ReclaimResult = S["ReclaimDto"];
export type ErrorBody = S["ErrorBody"];
export type ControlStateDto = S["ControlStateDto"];
export type ControlModeName = NonNullable<ControlStateDto["mode"]>;
export type ControlVerdict = S["ControlVerdict"];
export type ControlStateChanged = S["ControlStateChanged"];
export type ReleaseResult = S["ReleaseResult"];
export type ScreenPrivilegesInput = S["ScreenPrivilegesInput"];
export type TargetInfo = S["TargetInfo"];
export type TargetUnavailable = S["TargetUnavailable"];
export type TargetKind = S["TargetKind"];
export type ViewCapabilities = S["Capabilities"];
export type Codec = S["Codec"];
export type ViewProfile = S["Profile"];
export type EffectiveProfile = S["EffectiveProfile"];
export type ViewState = S["ViewState"];
export type ViewReason = S["Reason"];
export type FrameCause = S["Cause"];
export type SourceUnits = S["Units"];
export type SourceRect = S["SourceRect"];
export type CreateSessionInput = S["CreateSession"];
export type SessionInfo = S["SessionInfo"];
export type SessionUpdate = S["SessionUpdate"];
export type ViewHeartbeat = S["Heartbeat"];
export type PeerLink = S["PeerLink"];
export type ScreenStatus = S["ScreenStatus"];
export type ScreenStreamState = S["ScreenStreamState"];
export type TerminalInfo = S["TerminalInfo"];
export type TerminalOutput = S["TerminalOutput"];
export type TakeoverRequest = S["TakeoverRequest"];
export type TakeoverAccepted = S["TakeoverAccepted"];
export type TakeoverInfo = S["TakeoverInfo"];
export type RecoveryReason = S["RecoveryReason"];
export type ReleaseRequest = S["ReleaseRequest"];
export type HumanAction = S["HumanAction"];
export type ReleaseOutcome = S["Outcome"];
export type InputBatch = S["InputBatch"];
export type InputEvent = S["InputEvent"];
export type InputResult = S["InputResult"];
export type PointerPhase = S["PointerPhase"];
export type KeyPhase = S["KeyPhase"];
export type MouseButton = S["Button"];
export type InputModifier = S["Modifier"];
export type RecordHeader = S["RecordHeader"];
export type FrameMetadata = S["FrameMetadata"];
export type PolicyDto = S["PolicyDto"];
export type PolicyInput = S["PolicyInput"];
export type ProvidersDto = S["ProvidersDto"];
export type ConfiguredProviderDto = S["ConfiguredProviderDto"];
export type CatalogProviderDto = S["CatalogProviderDto"];
export type DetectedProviderDto = S["DetectedProviderDto"];
export type LocalModelsDto = S["LocalModelsDto"];
export type LocalInstallDto = S["LocalInstallDto"];
export type InstallStateDto = S["InstallStateDto"];
export type DoctorCheck = S["Check"];
export type DoctorStatus = S["Status"];
export type RepairId = S["Repair"];
export type DoctorReport = S["DoctorReport"];
export type RepairResult = S["RepairResult"];
export type CliAuthState = S["CliAuthState"];
export type EnableCliProviderInput = S["EnableCliProviderInput"];

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
      /** Structured facts for the approval card (see `lib/approvalDetails`). */
      details?: unknown;
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
      /** A bot asked the owner for a credential. The card posts to /v1/secret-requests/{call_id}; never carries a value. */
      type: "secret_requested";
      run_id: string;
      call_id: string;
      name: string;
      /** Untrusted model text: render as plain text only. */
      purpose: string;
      fields: "secret" | "login";
      replaces_existing: boolean;
      untrusted_content: boolean;
      expires_in_secs: number;
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
      type: "checkpoint_restored";
      run_id: string;
      checkpoint_id: string;
      safety_checkpoint_id: string;
      partial?: boolean;
      restored: string[];
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
  | {
      type: "control_state_changed";
      state: string;
      owner: string;
      target_id: string;
      reason?: ViewReason | null;
    }
  | { type: "view_targets_changed" }
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

// ---- Credential requests and the vault (`/v1/secret-requests/*`, `/v1/fill-secrets`) -----
export type SecretRequestAnswer = S["SecretRequestAnswer"];
export type FillSecretInfo = S["FillSecretDto"];

// ---- MCP management API (`/v1/mcp/*`, `/v1/secrets/*`) ---------------------

export type McpTransport = S["McpTransportKind"];
export type McpStatus = S["McpStatus"];
export type McpAuthState = S["McpAuthState"];
export type McpAuthStart = S["McpAuthStart"];
export type McpServer = S["McpServerDto"];
/** Body of `POST /v1/mcp/servers`. */
export type McpServerConfig = S["McpServerInput"] & { name: string };
/** Body of `PUT /v1/mcp/servers/{name}` (every field optional). */
export type McpServerPatch = S["McpServerPatch"];
export type McpTool = S["McpToolDto"];
export type McpRequiredSecret = S["McpRequiredSecretDto"];
export type McpCatalogEntry = S["McpCatalogEntryDto"];

export type ComposioStatus = S["ComposioStatusDto"];
export type ComposioConnState = S["ComposioConnState"];
export type ComposioToolkit = S["ComposioToolkitDto"];
export type ComposioCatalog = S["ComposioCatalogDto"];
export type ComposioApp = S["ComposioAppDto"];
export type ComposioConnect = S["ComposioConnectDto"];
export type ComposioLogin = S["ComposioLoginDto"];
export type ComposioLoginStatus = S["ComposioLoginStatusDto"];
export type Component = S["ComponentDto"];
export type ComponentId = S["ComponentId"];
export type ComposioDisconnect = S["ComposioDisconnectDto"];

// ---- E-mail accounts (`/v1/email/*`) ---------------------------------------

export type EmailAccount = S["EmailAccountDto"];
/** Body of `POST /v1/email/accounts` and `POST /v1/email/test`. */
export type EmailAccountInput = S["EmailAccountInput"];
/** Body of `PUT /v1/email/accounts/{name}` (every field optional). */
export type EmailAccountPatch = S["EmailAccountPatch"];
export type EmailSecurity = S["EmailSecurity"];
/** `imap_smtp` (own mailbox) or `agentmail` (an inbox created with the user's key). */
export type EmailProviderKind = S["EmailProviderKind"];
export type EmailProbeStatus = S["EmailProbeStatus"];
export type EmailTestReport = S["EmailTestReport"];
/** One inbox row: headers only; every text field is untrusted, show it as plain text. */
export type EmailMessage = S["EmailMessageDto"];
export type EmailSendTestResponse = S["EmailSendTestResponse"];

export type VoiceConfig = S["VoiceConfigDto"];
export type VoiceConfigInput = S["VoiceConfigInput"];
export type TranscribeInput = S["TranscribeInput"];
export type TranscribeOutput = S["TranscribeOutput"];

export type WebSearchConfig = S["WebSearchConfigDto"];
export type WebSearchConfigInput = S["WebSearchConfigInput"];
export type WebSearchTestInput = S["WebSearchTestInput"];
export type WebSearchTestReport = S["WebSearchTestReport"];

export type ChangeKind = S["ChangeKindDto"];
export type DiffOmittedReason = S["DiffOmittedReasonDto"];
export type UnifiedDiff = S["UnifiedDiffDto"];
export type FileChange = S["FileChangeDto"];
export type RunChanges = S["RunChangesDto"];
export type RestoreAction = S["RestoreActionDto"];
export type RestorePreviewEntry = S["RestorePreviewEntryDto"];
export type RestorePreview = S["RestorePreviewDto"];
export type RestoreRequest = S["RestoreRequestDto"];
export type RestoreResponse = S["RestoreResponseDto"];
export type SkillAssignmentMode = S["SkillAssignmentModeDto"];
export type SkillScope = S["SkillScope"];
export type SkillRequires = S["SkillRequiresDto"];
export type SkillHubInfo = S["SkillHubInfoDto"];
export type CatalogSkill = S["CatalogSkillDto"];
export type SkillIssueScope = S["SkillIssueScope"];
export type SkillIssue = S["SkillIssueDto"];
export type SkillsCatalog = S["SkillsCatalogDto"];
export type AssignedSkillState = S["AssignedSkillState"];
export type AssignedSkill = S["AssignedSkillDto"];
export type AvailableSkill = S["AvailableSkillDto"];
export type OwnSkill = S["OwnSkillDto"];
export type BotSkills = S["BotSkillsDto"];
export type PutBotSkillsInput = S["PutBotSkillsInput"];

export type CliEngineDto = S["CliEngineDto"];
export type BotEngineInput = S["BotEngineInput"];
export type CliInstallApproval = S["CliInstallApproval"];
