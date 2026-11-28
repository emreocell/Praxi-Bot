import type { components } from "./schema";

/** Types generated from `docs/api/openapi.json` (`pnpm --filter desktop-ui gen:api`). */
type S = components["schemas"];
export type BrowserProfileStatus = S["BrowserProfileStatus"];
export type RecentLogs = S["RecentLogs"];
export type AuditPage = S["AuditPage"];
export type AuditEvent = S["AuditEventDto"];
export type AuditVerify = S["AuditVerify"];
export type SearchPage = S["SearchPage"];
export type WorkflowProposal = S["WorkflowProposal"];
export type WorkflowDefinition = S["WorkflowDto"];
export type SaveWorkflow = S["SaveWorkflow"];
export type RunWorkflow = S["RunWorkflow"];
export type WorkflowRun = S["WorkflowRun"];
export type EditWorkflow = S["EditWorkflow"];
export type DuplicateWorkflow = S["DuplicateWorkflow"];
export type ImportWorkflow = S["ImportWorkflow"];
export type WorkflowExport = S["WorkflowExport"];
export type WorkflowRunRecord = S["WorkflowRunRecord"];
export type KnowledgeSource = S["KnowledgeSourceDto"];
export type KnowledgeWatchStatus = S["KnowledgeWatchStatus"];
export type KnowledgeFolderListing = S["KnowledgeFolderListing"];
export type KnowledgeFolderPreview = S["KnowledgeFolderPreview"];
export type KnowledgeSkipped = S["KnowledgeSkipped"];
export type KnowledgeFileFailure = S["KnowledgeFileFailure"];
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
export type ForgottenItem = S["ForgottenItemDto"];
export type MemoryPreviewHit = S["MemoryPreviewHitDto"];
export type MemoryPreview = S["MemoryPreviewDto"];
export type BotMemorySettings = S["BotMemorySettingsDto"];
export type BotMemorySettingsInput = S["BotMemorySettingsInput"];
export type PageReader = S["PageReaderDto"];
/** A mission or background-run digest the daemon pushes to the owner (Telegram, Discord, the panel). */
export type OwnerNotice = S["OwnerNotice"];
/** What is pushed to the owner's channels, and the quiet hours. */
export type BotCursorSettings = S["BotCursorSettings"];
export type OwnerNotifySettings = S["OwnerNotifySettings"];
export type MemoryEmbedding = S["MemoryEmbeddingDto"];
export type MemoryEmbeddingInput = S["MemoryEmbeddingInput"];

export type Bot = S["BotDto"];
export type BotInput = S["BotInput"];
export type BotPatchInput = S["BotPatchInput"];

/** A webhook that starts this bot. The secret is never in this object. */
export interface IncomingWebhook {
  id: string;
  bot_id: string;
  name: string;
  instruction: string;
  enabled: boolean;
  path: string;
  created_at_ms: number;
  updated_at_ms: number;
  last_fired_at_ms: number | null;
}

export interface IncomingWebhookCreated {
  webhook: IncomingWebhook;
  secret: string;
}

export interface IncomingWebhookRotated {
  webhook: IncomingWebhook;
  secret: string;
}

export interface IncomingWebhookInput {
  name: string;
  instruction?: string;
}

export interface IncomingWebhookPatch {
  name?: string;
  instruction?: string;
  enabled?: boolean;
}

export interface IncomingWebhookAccepted {
  status: string;
  webhook_id: string;
  run_id: string | null;
  chat_id: string | null;
}
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
export type HostPacks = S["HostPacks"];
export type HostPackInfo = S["HostPackInfo"];
export type PackAction = S["PackAction"];
export type PackPlan = S["PackPlan"];
export type PackPlanItem = S["PackPlanItem"];
export type BootstrapCommand = S["BootstrapCommand"];
export type HostStatus = S["HostStatus"];
export type HostVersion = S["HostVersion"];
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
export type AdminCliEnvironment = S["AdminCliEnvironmentDto"];
export type AdminCliInstall = S["AdminCliInstallDto"];
export type AdminCliLogin = S["AdminCliLoginDto"];
export type HomeInfo = S["HomeDto"];
export type StopRunsInput = S["StopRunsInput"];
export type StopRunsResponse = S["StopRunsResponse"];
export type UsageSummary = S["UsageSummary"];
export type ModelUsage = S["ModelUsage"];
export type ProviderUsage = S["ProviderUsage"];
export type DailyUsage = S["DailyUsage"];
export type TopRunUsage = S["TopRunUsage"];
export type BotUsage = S["BotUsage"];
export type SpendLimit = S["SpendLimitDto"];
export type EmbeddingsSetup = S["EmbeddingsDto"];
export type EmbeddingProviderRow = S["EmbeddingProviderDto"];
export type EmbeddingProviderInput = S["EmbeddingProviderInput"];
export type EmbeddingsInput = S["EmbeddingsInput"];
export type EmbeddingTest = S["EmbeddingTestDto"];
export type DecisionHelper = S["DecisionHelperDto"];
export type DataUsage = S["DataUsageDto"];
export type ClearCacheResult = S["ClearCacheDto"];
export type BotSpendLimits = S["BotSpendLimitsDto"];
export type Mission = S["MissionDto"];
export type MissionItem = S["MissionItemDto"];
export type MissionWait = S["MissionWaitDto"];
export type MissionDetail = S["MissionDetailDto"];
export type MissionDigest = S["MissionDigestDto"];
export type MissionProgress = S["MissionProgressDto"];
export type CreateMission = S["CreateMissionInput"];
export type MissionAnswer = S["MissionAnswerInput"];
export type MissionAnswersResult = S["MissionAnswersDto"];
export type UsageTotals = S["UsageTotals"];
export type EffectivePolicy = S["EffectivePolicyDto"];
export type Grant = S["GrantDto"];
export type SiteRule = S["SiteRuleDto"];
export type SiteRuleInput = S["SiteRuleInput"];
export type SiteRuleSuggestion = S["SiteRuleSuggestionDto"];
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
export type BotTerminal = S["BotTerminal"];
export type BotTerminalState = S["BotTerminalState"];
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
      /** What granted the call (`GrantedBy` in crates/protocol); absent on events recorded before it existed. */
      granted_by?: string;
      /** The bot's permission level when the call was decided: `ask`, `balanced` or `full`. */
      level?: string;
      /** The remembered rule that covered the call, `<scope>:<name>`; never text of the call. */
      rule_id?: string;
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
      /** A bot needs the owner for a CAPTCHA or a 2FA code. The card posts to /v1/handovers/{call_id}/...; never carries a code. */
      type: "handover_requested";
      run_id: string;
      call_id: string;
      kind: "captcha" | "code";
      /** Origin the browser reported for the page. */
      site: string;
      untrusted_content: boolean;
      expires_in_secs: number;
    }
  | {
      /**
       * An MCP server asks the owner for input through a form. `call_id` is the request's own id
       * (not a model tool call); the card posts to /v1/elicitations/{call_id}. `message` and the
       * field titles and descriptions are untrusted server text: render as plain text only.
       */
      type: "elicitation_requested";
      run_id: string;
      call_id: string;
      server: string;
      message: string;
      fields: ElicitationField[];
      /** 0 = no time limit. */
      expires_in_secs: number;
    }
  | {
      /** A bot asked the owner for a credential. The card posts to /v1/secret-requests/{call_id}; never carries a value. */
      type: "secret_requested";
      run_id: string;
      call_id: string;
      name: string;
      /** Untrusted model text: render as plain text only. */
      purpose: string;
      /** Absent in older journal events, which cannot reopen an input form. */
      origin?: string;
      fields: "secret" | "login";
      replaces_existing: boolean;
      untrusted_content: boolean;
      expires_in_secs: number;
      /** A mission asked: no run waits, the answer releases the parked item. */
      parked?: boolean;
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
      /** Set when the child is another bot asked through `ask_bot`. */
      helper?: SubagentHelper;
    }
  | {
      type: "subagent_finished";
      run_id: string;
      parent_run_id: string;
      state: "succeeded" | "failed" | "cancelled" | "paused";
      summary: string;
      call_id?: string;
    }
  | { type: "workspace_warning"; warning: WorkspaceWarningEventDto };

/** The two bots of an `ask_bot` hand-off (daemon-supplied). */
export interface SubagentHelper {
  bot_id: string;
  bot_name: string;
  caller_bot_id: string;
  caller_name: string;
}

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
export type HandoverAnswer = S["HandoverAnswer"];
/** `POST /v1/elicitations/{call_id}` (schema `AnswerElicitation`). */
export interface AnswerElicitation {
  action: "accept" | "decline";
  content: Record<string, string | number | boolean>;
}
export type ElicitationOutcome = "accepted" | "declined" | "cancelled";
export interface ElicitationAnswer {
  call_id: string;
  outcome: ElicitationOutcome;
}

/** One field of an MCP server's form (`elicitation_requested`). */
export interface ElicitationField {
  name: string;
  title: string | null;
  description: string | null;
  type: "string" | "number" | "integer" | "boolean";
  required: boolean;
  /** Present only for choice fields (type string). */
  options?: { value: string; label: string }[];
  minimum?: number;
  maximum?: number;
  /** In characters (type string). */
  min_length?: number;
  max_length?: number;
  format?: "email" | "uri" | "date" | "date-time";
  /** The only value a form may prefill. */
  default?: unknown;
  /**
   * A suspected credential field (password, token, one-time code, card number, API key): shown
   * as blocked and never submitted. The event JSON leaves it out when false.
   */
  blocked?: boolean;
}
export type FillSecretInfo = S["FillSecretDto"];
export type VaultKind = S["VaultKindDto"];
export type CreateFillSecretInput = S["CreateFillSecret"];
export type UpdateFillSecretInput = S["UpdateFillSecret"];
export type RevealFillSecretResponse = S["RevealFillSecretResponse"];
export type FillGrantInfo = S["FillGrantDto"];

// ---- MCP management API (`/v1/mcp/*`, `/v1/secrets/*`) ---------------------

export type McpTransport = S["McpTransportKind"];
export type McpStatus = S["McpStatus"];
export type McpAuthState = S["McpAuthState"];
export type McpAuthStart = S["McpAuthStart"];
export type McpTestResult = S["McpTestResult"];
export type McpServer = S["McpServerDto"];
export type McpBundleInspection = S["Inspection"];
export type McpRegistryResult = S["RegistryResult"];
export type McpRegistrySource = S["RegistryPreview"];
export type McpRegistryApproval = S["RegistryApproval"];
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
export type ComposioAction = S["ComposioActionDto"];
export type ComposioApp = S["ComposioAppDto"];
export type ComposioConnect = S["ComposioConnectDto"];
export type ComposioLogin = S["ComposioLoginDto"];
export type ComposioLoginStatus = S["ComposioLoginStatusDto"];
export type OpenApiApp = S["OpenApiAppDto"];
export type OpenApiOperation = S["OpenApiOperationDto"];
export type OpenApiAuthKind = S["OpenApiAuthKind"];
export type OpenApiAuthInput = S["OpenApiAuthInput"];
export type OpenApiAppInput = S["OpenApiAppInput"];
export type OpenApiAppUpdate = S["OpenApiAppUpdate"];
export type OpenApiEnabledOperation = S["OpenApiEnabledOperation"];
export type Component = S["ComponentDto"];
export type ComponentId = S["ComponentId"];
export type ComposioDisconnect = S["ComposioDisconnectDto"];
export type ComposioTest = S["ComposioTestDto"];

// ---- Calendar accounts (`/v1/calendar/*`) ----------------------------------

export type CalendarAccount = S["CalendarAccountDto"];
export type CalendarAccountInput = S["CalendarAccountInput"];
export type CalendarTestInput = S["CalendarTestInput"];
export type CalendarTestReport = S["CalendarTestReport"];
export type CalendarFound = S["CalendarFoundDto"];

// ---- Home Assistant (`/v1/homeassistant*`) ---------------------------------

export type HomeAssistantSettings = S["HomeAssistantDto"];
export type HomeAssistantInput = S["HomeAssistantInput"];
export type HomeAssistantSaved = S["HomeAssistantSaved"];
export type HomeAssistantTestInput = S["HomeAssistantTestInput"];
export type HomeAssistantTestReport = S["HomeAssistantTestReport"];

// ---- E-mail accounts (`/v1/email/*`) ---------------------------------------

export type EmailAccount = S["EmailAccountDto"];
export type EmailChannel = S["EmailChannelDto"];
export type EmailChannelSettings = S["EmailChannelSettingsDto"];
export type EmailPendingReply = S["EmailPendingReplyDto"];
/** Body of `POST /v1/email/accounts` and `POST /v1/email/test`. */
export type EmailAccountInput = S["EmailAccountInput"];
/** Body of `PUT /v1/email/accounts/{name}` (every field optional). */
export type EmailAccountPatch = S["EmailAccountPatch"];
export type EmailSecurity = S["EmailSecurity"];
/** `imap_smtp` (own mailbox) or `agentmail` (an inbox created with the user's key). */
export type EmailProviderKind = S["EmailProviderKind"];
export type EmailProbeStatus = S["EmailProbeStatus"];
/** `ask` (every send needs approval) or `draft_only`. */
export type EmailSendMode = S["EmailSendMode"];
export type EmailTestReport = S["EmailTestReport"];
/** One inbox row: headers only; every text field is untrusted, show it as plain text. */
export type EmailMessage = S["EmailMessageDto"];
export type EmailSendTestResponse = S["EmailSendTestResponse"];
export type EmailGuardSettings = S["EmailGuardSettingsDto"];
export type EmailOptOut = S["EmailOptOutDto"];
export type EmailSendLogEntry = S["EmailSendLogDto"];

/** Live voice settings (`/v1/voice`); intersected so the UI also builds against an older generated schema. */
export interface LiveVoiceSettings {
  live_model: string;
  live_voice: string;
  live_keep_audio: boolean;
}
export type VoiceConfig = S["VoiceConfigDto"] & LiveVoiceSettings;
export type VoiceConfigInput = S["VoiceConfigInput"] & Partial<LiveVoiceSettings>;
/** What the daemon sends on the live voice event stream (`POST /v1/voice/live`), one JSON object per event. */
export type LiveVoiceEvent =
  | { type: "ready"; session_id: string; model: string; input_rate: number; output_rate: number; max_seconds: number }
  | { type: "audio"; data: string }
  | { type: "you"; text: string }
  | { type: "bot"; text: string }
  | { type: "turn_complete" }
  | { type: "interrupted" }
  | { type: "tool_call"; call_id: string; request: string }
  | { type: "tool_cancelled"; call_ids: string[] }
  | { type: "reconnecting" }
  | { type: "ended"; reason: "limit" | "idle" | "error" | "closed"; message?: string };
export interface LiveVoiceOpenInput {
  language?: string;
  assistant_name?: string;
}
/** One line of a finished voice conversation, written into the chat as an ordinary message. */
export interface VoiceTurn {
  role: "user" | "assistant";
  text: string;
}
export type LocalVoiceStatus = S["LocalVoiceStatusDto"];
export type TranscribeInput = S["TranscribeInput"];
export type TranscribeOutput = S["TranscribeOutput"];

export type WebSearchConfig = S["WebSearchConfigDto"];
export type WebSearchConfigInput = S["WebSearchConfigInput"];
export type WebSearchTestInput = S["WebSearchTestInput"];
export type WebSearchTestReport = S["WebSearchTestReport"];
export type ConnectionTestReport = S["ConnectionTestReport"];

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
export type MarketIndex = S["MarketIndexDto"];
export type MarketSkill = S["MarketSkillDto"];
export type MarketPreview = S["MarketPreviewDto"];
export type MarketPreviewResponse = S["MarketPreviewResponse"];
export type SkillDraft = S["SkillDraftDto"];
export type SkillDraftPreview = S["HubPreviewDto"];
export type MarketInstallInput = S["MarketInstallInput"];
export type MarketInstalled = S["MarketInstalledDto"];
export type DeleteSkillResult = S["DeleteSkillResult"];

export type CliEngineDto = S["CliEngineDto"];
export type BotEngineInput = S["BotEngineInput"];
export type CliInstallApproval = S["CliInstallApproval"];

// ---- Persistent Bot Workspaces (`/v1/workspaces*`, `/v1/bots/{id}/workspace*`) ----

export interface DiskStatsDto {
  total_bytes: number;
  available_bytes: number;
  used_bytes: number;
  used_percent: number;
}

export interface WorkspaceSettingsDto {
  warn_percent_1: number;
  warn_percent_2: number;
  warn_percent_3: number;
  min_free_bytes: number;
  downloads_max_age_days: number | null;
  logs_max_age_days: number | null;
}

export type WorkspaceSettingsInput = WorkspaceSettingsDto;

export interface BotWorkspaceSummaryDto {
  bot_id: string;
  bot_name: string;
  folder_name: string;
  workspace_path: string | null;
  exists: boolean;
  used_bytes: number;
  file_count: number;
  soft_limit_bytes: number | null;
  hard_limit_bytes: number | null;
  soft_limit_exceeded: boolean;
  hard_limit_exceeded: boolean;
}

export interface WorkspaceBigItemDto {
  bot_id: string | null;
  bot_name: string | null;
  folder_name: string;
  relative_path: string;
  size_bytes: number;
  modified_at_ms: number;
  category: string;
}

export type CleanupTarget = "old_downloads" | "old_logs" | "all_old" | "daemon_cache";

export interface CleanupSuggestionDto {
  target: CleanupTarget;
  reclaimable_bytes: number;
  file_count: number;
}

/**
 * Payload of the SSE `workspace_warning` event. The stream is readable with any
 * local token, so it carries numbers and bot ids only: names of bots, folders and
 * files come from the owner-gated `GET /v1/workspaces` (`WorkspaceWarningDto`).
 */
export interface WorkspaceWarningEventDto {
  severity: "warning" | "critical" | string;
  threshold_percent: number | null;
  below_min_free: boolean;
  disk_used_percent: number | null;
  available_bytes: number | null;
  min_free_bytes: number;
  soft_limit_bots: string[];
}

export interface WorkspaceWarningDto {
  severity: "warning" | "critical" | string;
  threshold_percent: number | null;
  below_min_free: boolean;
  disk_used_percent: number | null;
  available_bytes: number | null;
  min_free_bytes: number;
  soft_limit_bots: string[];
  largest_items: WorkspaceBigItemDto[];
  suggestions: CleanupSuggestionDto[];
}

export interface WorkspacesOverviewDto {
  configured: boolean;
  root_path: string | null;
  disk: DiskStatsDto | null;
  total_workspace_bytes: number;
  settings: WorkspaceSettingsDto;
  bots: BotWorkspaceSummaryDto[];
  largest_items: WorkspaceBigItemDto[];
  suggestions: CleanupSuggestionDto[];
  active_warning: WorkspaceWarningDto | null;
}

export interface WorkspaceCleanupInput {
  target: CleanupTarget;
  bot_id?: string | null;
}

export interface WorkspaceCleanupResultDto {
  target: CleanupTarget;
  deleted_files: number;
  freed_bytes: number;
}

export type WorkspaceEntryKind = "directory" | "file";
export type WorkspacePreviewKind = "none" | "text" | "image";

export interface WorkspaceEntryDto {
  name: string;
  path: string;
  kind: WorkspaceEntryKind;
  size_bytes: number;
  modified_at_ms: number;
  preview_kind: WorkspacePreviewKind;
}

export interface WorkspaceTrashEntryDto {
  undo_id: string;
  name: string;
  original_path: string;
  size_bytes: number;
  deleted_at_ms: number;
}

export interface BotWorkspaceListDto {
  bot_id: string;
  bot_name: string;
  folder_name: string;
  workspace_path: string | null;
  current_path: string;
  used_bytes: number;
  soft_limit_bytes: number | null;
  hard_limit_bytes: number | null;
  soft_limit_exceeded: boolean;
  hard_limit_exceeded: boolean;
  entries: WorkspaceEntryDto[];
  undo_items: WorkspaceTrashEntryDto[];
}

export interface WorkspaceRenameInput {
  path: string;
  new_name: string;
}

export interface WorkspaceDeleteInput {
  path: string;
}

export interface WorkspaceRestoreInput {
  undo_id: string;
}

export interface BotWorkspaceLimitsInput {
  soft_limit_bytes: number | null;
  hard_limit_bytes: number | null;
}

// --- Import from other assistants (`POST /v1/import/scan` and `/v1/import/run`, owner-only) ------
// Hand-written until the daemon routes are in the generated schema.

/** The other assistants the daemon knows where to look for (`folder` = a folder the owner typed). */
export type ImportApp = "claude" | "cursor" | "vscode" | "codex" | "openclaw" | "hermes" | "folder";
export type ImportKind = "mcp_server" | "skill" | "instructions" | "other";
/** Stable code for why an item is (not) importable or was skipped; the UI words it per language. */
export type ImportReason =
  | "ready"
  | "ready_with_secrets"
  | "already_have"
  | "duplicate"
  | "unpinned"
  | "never_list"
  | "unsupported"
  | "invalid"
  | "unreadable"
  | "secret_in_command"
  | "variables"
  | "review_first"
  | "too_long"
  | "never_copied"
  | "not_imported"
  // Only in run results: why an item that should be imported was not.
  | "not_found"
  | "failed"
  | "secret_failed";

/** A value that would go to the secret store. Only its name is ever shown: the daemon holds the value. */
export interface ImportSecretSlot {
  /** Name in the secret store; what `confirmed_secrets` carries. */
  name: string;
  /** The environment variable or header name. */
  key: string;
  place: "env" | "header";
}

export interface ImportItem {
  id: string;
  app: ImportApp;
  kind: ImportKind;
  name: string;
  /** Where it was found, e.g. `~/.cursor/mcp.json`. */
  source: string;
  import: boolean;
  default_on: boolean;
  reason: ImportReason;
  /** English fallback for `reason`. */
  note: string;
  /** Safe one-line summary, never a value. */
  detail?: string | null;
  secrets: ImportSecretSlot[];
  /** Names whose value was only a variable reference and was not copied. */
  dropped: string[];
}

export interface ImportResult {
  id: string;
  status: "imported" | "skipped" | "failed";
  reason?: ImportReason | null;
  /** English fallback. */
  message: string;
  /** Secret-store names that were stored. */
  secrets_stored: string[];
  /** Keys (not names) that were left out because the owner did not confirm them. */
  secrets_left_out: string[];
  /** Instructions only: the text to append to a bot's persona. */
  text?: string | null;
}

export interface ImportScanInput {
  only?: ImportApp;
  /** A folder of prompt / skill Markdown on the machine the daemon runs on. */
  folder?: string;
}
export interface ImportScanOutput {
  items: ImportItem[];
}
export interface ImportRunInput extends ImportScanInput {
  ids: string[];
  confirmed_secrets: string[];
}
export interface ImportRunOutput {
  results: ImportResult[];
}
