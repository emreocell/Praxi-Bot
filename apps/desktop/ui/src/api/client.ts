import { createSseParser } from "./events.ts";
import { isLocalBaseUrl } from "../lib/liveViewState.ts";
import type { Connection } from "./connection";
import type {
  ExportOutput,
  ImportInput,
  ImportOutput,
  ImportPreview,
  SharedProfile
} from "../lib/profileShare";
import type {
  KnowledgeSource,
  KnowledgeSourceInput,
  KnowledgeSourceChange,
  KnowledgeMutation,
  KnowledgeSettings,
  KnowledgeSettingsInput,
  KnowledgeEmailCapture,
  KnowledgeEmbeddingProgress,
  KnowledgeEmbeddingApproval,
  KnowledgeEmbeddingGrantTarget,
  MemoryItem,
  MemoryKind,
  CreateMemoryInput,
  UpdateMemoryInput,
  MemoryPreview
} from "./types";
import type {
  AcceptedResponse,
  AdminCliEngine,
  AdminCliInstall,
  AdminCliLogin,
  AdminProvider,
  AdminProviderInput,
  AdminState,
  CopyPlan,
  ReclaimResult,
  CopyReport,
  CopyRequest,
  HomeInfo,
  Approval,
  Bot,
  BotEngineInput,
  BotInput,
  Chat,
  UpdateChatInput,
  ConfiguredProviderDto,
  CliEngineDto,
  LocalModelsDto,
  CliInstallApproval,
  ControlModeName,
  ControlStateDto,
  ControlVerdict,
  CreateSessionInput,
  Decision,
  DetectedProviderDto,
  DoctorReport,
  EffectiveProfile,
  ErrorBody,
  Grant,
  GrantTarget,
  Health,
  HostLinkConnection,
  HostLinkInfo,
  PcRequestList,
  HostSetupJob,
  SshKeyHint,
  StartHostSetupInput,
  HostLinkList,
  EmailAccount,
  EmailAccountInput,
  EmailAccountPatch,
  EmailMessage,
  EmailSendTestResponse,
  EmailTestReport,
  InputBatch,
  InputResult,
  ComposioApp,
  ComposioCatalog,
  ComposioConnect,
  ComposioLogin,
  ComposioLoginStatus,
  Component,
  ComponentId,
  ComposioDisconnect,
  ComposioStatus,
  McpCatalogEntry,
  McpAuthState,
  McpAuthStart,
  McpServer,
  McpServerPatch,
  McpServerConfig,
  McpTool,
  Message,
  ParsedSchedule,
  PolicyDto,
  PolicyInput,
  PreviewSchedule,
  ProvidersDto,
  ReleaseRequest,
  RememberResponse,
  WorkflowProposal,
  WorkflowDefinition,
  SaveWorkflow,
  RunWorkflow,
  WorkflowRun,
  RepairId,
  RepairResult,
  Run,
  Schedule,
  ScheduleHistoryEntry,
  SecretRequestAnswer,
  FillSecretInfo,
  ScheduleInput,
  Scope,
  ScreenPrivilegesInput,
  RunUsage,
  SendMode,
  ResendResponse,
  SendResponse,
  SessionInfo,
  SessionUpdate,
  TakeoverAccepted,
  TakeoverRequest,
  TargetInfo,
  TargetUnavailable,
  PeerLink,
  ScreenStatus,
  TerminalInfo,
  TerminalOutput,
  SpendLimit,
  UsageSummary,
  ViewHeartbeat,
  TranscribeInput,
  TranscribeOutput,
  VoiceConfig,
  VoiceConfigInput,
  WebSearchConfig,
  WebSearchConfigInput,
  WebSearchTestInput,
  WebSearchTestReport,
  RunChanges,
  RestorePreview,
  RestoreResponse,
  BotSkills,
  BotPatchInput,
  PutBotSkillsInput,
  SkillsCatalog,
  StopRunsInput,
  StopRunsResponse,
  Attachment
} from "./types";

export const LIVE_VIEW_MEDIA_TYPE = "application/vnd.praxi.live-view.v1";

/** An error response of the daemon (`{"error": {"code", "message"}}`) or a network failure. */

/** A request that gets no answer for this long fails instead of hanging its screen forever. */
const REQUEST_TIMEOUT_MS = 60_000;

export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  /** The parsed JSON body of the error response, when it had one (a `409` carries `current`). */
  readonly body?: unknown;
  /** Name of the paired host the request went to, when it was not this computer (see `humanError`). */
  readonly host?: string;
  /**
   * Only for `code === "network"`: why no answer came. `connect` = the connection itself failed
   * (refused / unreachable: the request never reached a service); `timeout` = the request ran into
   * its time limit; `aborted` = the client was disposed or the caller cancelled. Only `connect` is
   * proof that nothing received the request.
   */
  readonly netKind?: "connect" | "timeout" | "aborted";

  constructor(status: number, code: string, message: string, body?: unknown, host?: string) {
    super(message);
    this.host = host;
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.body = body;
  }
}

type Query = Record<string, string | number | null | undefined>;

/**
 * Small typed fetch wrapper over the daemon API (`/v1`).  Request / response
 * shapes come from the generated OpenAPI types (see `types.ts`).
 */
/** What the owner lets one server's bots do on THIS computer; every part is off until turned on. */
export interface ComputerSwitches {
  /** Look at the screen, click, type and start programs. */
  screen: boolean;
  /** Skip the first-look card each session (screen only). */
  alwaysRead: boolean;
  browser: boolean;
  commands: boolean;
  files: boolean;
}

/** The switches a paired server currently has, read from its link info. */
export function switchesOf(host: {
  computer_access?: boolean | null;
  computer_access_always?: boolean | null;
  computer_browser?: boolean | null;
  computer_commands?: boolean | null;
  computer_files?: boolean | null;
}): ComputerSwitches {
  return {
    screen: host.computer_access === true,
    alwaysRead: host.computer_access_always === true,
    browser: host.computer_browser === true,
    commands: host.computer_commands === true,
    files: host.computer_files === true
  };
}

export class ApiClient {
  listKnowledgeEmailCaptures(bot: string): Promise<KnowledgeEmailCapture[]> {
    return this.request("GET", `/bots/${encodeURIComponent(bot)}/knowledge/email-captures`);
  }
  listKnowledgeSources(bot: string): Promise<KnowledgeSource[]> {
    return this.request("GET", `/bots/${encodeURIComponent(bot)}/knowledge/sources`);
  }

  createKnowledgeSource(bot: string, body: KnowledgeSourceInput): Promise<KnowledgeMutation> {
    return this.request("POST", `/bots/${encodeURIComponent(bot)}/knowledge/sources`, { body });
  }

  updateKnowledgeSource(bot: string, source: string, body: KnowledgeSourceChange): Promise<KnowledgeSource> {
    return this.request("PUT", `/bots/${encodeURIComponent(bot)}/knowledge/sources/${encodeURIComponent(source)}`, { body });
  }

  deleteKnowledgeSource(bot: string, source: string, expectedRevision: number): Promise<void> {
    return this.request("DELETE", `/bots/${encodeURIComponent(bot)}/knowledge/sources/${encodeURIComponent(source)}`, { body: { expected_revision: expectedRevision } });
  }

  reindexKnowledgeSource(bot: string, source: string, expectedRevision: number, cursor?: string): Promise<KnowledgeMutation> {
    return this.request("POST", `/bots/${encodeURIComponent(bot)}/knowledge/sources/${encodeURIComponent(source)}/reindex`, { body: { expected_revision: expectedRevision, cursor: cursor ?? null } });
  }

  embedKnowledgeSource(bot: string, source: string, expectedRevision: number, resetRetry = false): Promise<KnowledgeEmbeddingProgress> {
    return this.request("POST", `/bots/${encodeURIComponent(bot)}/knowledge/sources/${encodeURIComponent(source)}/embed`, { body: { expected_revision: expectedRevision, reset_retry: resetRetry } });
  }

  getKnowledgeSettings(bot: string): Promise<KnowledgeSettings> {
    return this.request("GET", `/bots/${encodeURIComponent(bot)}/knowledge/settings`);
  }

  updateKnowledgeSettings(bot: string, body: KnowledgeSettingsInput): Promise<KnowledgeSettings> {
    return this.request("PUT", `/bots/${encodeURIComponent(bot)}/knowledge/settings`, { body });
  }
  requestKnowledgeEmbeddingApproval(bot: string, body: KnowledgeEmbeddingGrantTarget): Promise<KnowledgeEmbeddingApproval> {
    return this.request("POST", `/bots/${encodeURIComponent(bot)}/knowledge/embedding-approvals`, { body });
  }

  confirmKnowledgeEmbeddingApproval(bot: string, approval: string): Promise<void> {
    return this.request("POST", `/bots/${encodeURIComponent(bot)}/knowledge/embedding-approvals/${encodeURIComponent(approval)}/confirm`);
  }

  revokeKnowledgeEmbeddingGrant(bot: string, body: KnowledgeEmbeddingGrantTarget): Promise<void> {
    return this.request("DELETE", `/bots/${encodeURIComponent(bot)}/knowledge/embedding-grants`, { body });
  }

  resetKnowledgeEmbeddingCooldown(bot: string): Promise<void> {
    return this.request("POST", `/bots/${encodeURIComponent(bot)}/knowledge/embedding-cooldown/reset`);
  }

  listMemories(opts: {
    bot?: string | null;
    botOnly?: boolean;
    kind?: MemoryKind | null;
    q?: string | null;
    includeDeleted?: boolean;
  } = {}): Promise<MemoryItem[]> {
    return this.request("GET", "/memory", {
      query: {
        bot: opts.bot ?? undefined,
        bot_only: opts.botOnly ? "true" : undefined,
        kind: opts.kind ?? undefined,
        q: opts.q?.trim() ? opts.q.trim() : undefined,
        include_deleted: opts.includeDeleted ? "true" : undefined
      }
    });
  }

  createMemory(body: CreateMemoryInput): Promise<MemoryItem> {
    return this.request("POST", "/memory", { body });
  }

  updateMemory(id: string, body: UpdateMemoryInput): Promise<MemoryItem> {
    return this.request("PATCH", `/memory/${encodeURIComponent(id)}`, { body });
  }

  deleteMemory(id: string): Promise<MemoryItem> {
    return this.request("DELETE", `/memory/${encodeURIComponent(id)}`);
  }

  undoMemory(id: string): Promise<MemoryItem> {
    return this.request("POST", `/memory/${encodeURIComponent(id)}/undo`);
  }

  previewBotMemory(bot: string, query: string): Promise<MemoryPreview> {
    return this.request("GET", `/bots/${encodeURIComponent(bot)}/memory/preview`, {
      query: { query }
    });
  }
  readonly baseUrl: string;
  private readonly token: string;
  /** Name of the paired host this client talks to (null for this computer). */
  hostName: string | null = null;
  /**
   * Where the owner key comes from (the Tauri `owner_key` command, main window only), installed by
   * `main.tsx`. `null` in a plain browser: the daemon then refuses owner-only routes.
   */
  static ownerKeyProvider: (() => Promise<string | null>) | null = null;
  /** Aborts every request of this client when its computer is dropped (`dispose`). */
  private readonly life = new AbortController();

  constructor(conn: Pick<Connection, "base_url" | "token">) {
    this.baseUrl = conn.base_url.replace(/\/$/, "");
    this.token = conn.token;
  }

  /** The computer this client talked to is no longer selected: drop what is in flight. */
  dispose(): void {
    this.life.abort();
  }

  /** Headers for a request, also used by the SSE reader. */
  authHeaders(): Record<string, string> {
    return { Authorization: `Bearer ${this.token}` };
  }

  /**
   * Headers for the event stream: the owner key marks this app as the local manager, so the
   * daemon sends it approval/question/control events (a bearer alone only gets progress events).
   * Never sent through the proxy to a paired host.
   */
  async eventStreamHeaders(): Promise<Record<string, string>> {
    const headers = this.authHeaders();
    if (ApiClient.ownerKeyProvider && isLocalBaseUrl(this.baseUrl)) {
      const key = await ApiClient.ownerKeyProvider();
      if (key) {
        headers["x-praxi-owner-key"] = key;
      }
    }
    return headers;
  }

  protected async request<T>(
    method: string,
    path: string,
    opts: { body?: unknown; query?: Query; auth?: boolean; signal?: AbortSignal; headers?: Record<string, string>; raw?: Blob } = {}
  ): Promise<T> {
    const res = await this.send(method, path, opts);
    if (res.status === 204) {
      return undefined as T;
    }
    return (await res.json()) as T;
  }

  /** Like `request`, for routes that answer with binary data (an image). */
  protected async requestBlob(method: string, path: string): Promise<Blob> {
    const res = await this.send(method, path);
    return res.blob();
  }

  /** Like `request`, for routes that answer with text (Markdown, CSV). */
  protected async requestText(
    method: string,
    path: string,
    opts: { query?: Query; signal?: AbortSignal } = {}
  ): Promise<string> {
    const res = await this.send(method, path, opts);
    return res.text();
  }

  /** Perform the request; throws `ApiError` unless the status is 2xx. */
  private async send(
    method: string,
    path: string,
    opts: {
      body?: unknown;
      query?: Query;
      auth?: boolean;
      signal?: AbortSignal;
      /** Streaming answers (install output) must not be cut by the request timeout. */
      noTimeout?: boolean;
      headers?: Record<string, string>;
      /** A file body, sent as is (the attachment upload); `body` stays JSON. */
      raw?: Blob;
    } = {}
  ): Promise<Response> {
    const url = new URL(`${this.baseUrl}/v1${path}`);
    for (const [k, v] of Object.entries(opts.query ?? {})) {
      if (v !== null && v !== undefined) {
        url.searchParams.set(k, String(v));
      }
    }
    const headers: Record<string, string> = { ...(opts.headers ?? {}) };
    if (opts.auth !== false) {
      Object.assign(headers, this.authHeaders());
    }
    // The owner key (card 262) goes to THIS computer's daemon only, never through the proxy to a
    // paired host, and the daemon ignores it on routes that do not need it. One place for every call.
    if (!("x-praxi-owner-key" in headers) && ApiClient.ownerKeyProvider && isLocalBaseUrl(this.baseUrl)) {
      const key = await ApiClient.ownerKeyProvider();
      if (key) {
        headers["x-praxi-owner-key"] = key;
      }
    }
    if (opts.body !== undefined) {
      headers["Content-Type"] = "application/json";
    } else if (opts.raw !== undefined) {
      headers["Content-Type"] = "application/octet-stream";
    }
    let res: Response;
    try {
      res = await fetch(url, {
        method,
        headers,
        body: opts.raw ?? (opts.body === undefined ? undefined : JSON.stringify(opts.body)),
        signal: AbortSignal.any([
          this.life.signal,
          ...(opts.signal ? [opts.signal] : []),
          ...(opts.noTimeout ? [] : [AbortSignal.timeout(REQUEST_TIMEOUT_MS)])
        ])
      });
    } catch (e) {
      const name = e instanceof Error ? e.name : "";
      const netKind: "connect" | "timeout" | "aborted" =
        this.life.signal.aborted || name === "AbortError"
          ? "aborted"
          : name === "TimeoutError"
            ? "timeout"
            : "connect";
      const err = new ApiError(0, "network", e instanceof Error ? e.message : String(e));
      (err as { netKind?: string }).netKind = netKind;
      throw err;
    }
    if (!res.ok) {
      let code = "http_error";
      let message = `HTTP ${res.status}`;
      let parsed: unknown;
      try {
        const body = (await res.json()) as ErrorBody;
        parsed = body;
        code = body.error.code;
        message = body.error.message;
      } catch {
        // not JSON
      }
      throw new ApiError(res.status, code, message, parsed, this.hostName ?? undefined);
    }
    return res;
  }

  /** Hardware summary and local-model advice (`GET /v1/local-models`, owner only; best effort). */
  localModels(): Promise<LocalModelsDto> {
    return this.request("GET", "/local-models");
  }

  cliEngines(refresh = false): Promise<CliEngineDto[]> {
    return this.request("GET", "/providers/cli", { query: { refresh: refresh ? "1" : null } });
  }

  /** Ask a coding CLI for its model list again now (the picker otherwise refreshes it once a day). */
  refreshCliModels(id: string): Promise<void> {
    return this.request("POST", `/providers/cli/${encodeURIComponent(id)}/models/refresh`);
  }

  previewCliInstall(id: string): Promise<CliInstallApproval> {
    return this.request("POST", "/providers/cli/install", { body: { id } });
  }

  async decideCliInstall(id: string, decision: "approve" | "deny", onOutput: (text: string) => void): Promise<void> {
    const res = await this.send("POST", `/providers/cli/install/${encodeURIComponent(id)}/decision`, { body: { decision }, noTimeout: true });
    const reader = res.body?.getReader();
    if (!reader) { throw new Error("Missing install stream"); }
    let done = false;
    let failure: string | null = null;
    const parse = createSseParser((message) => {
      const data = JSON.parse(message.data) as { text?: string; ok?: boolean; error?: string };
      if (message.event === "output" && typeof data.text === "string") { onOutput(data.text); }
      if (message.event === "done") {
        done = true;
        if (!data.ok && data.error !== "denied") { failure = data.error ?? "install_failed"; }
      }
    });
    const decoder = new TextDecoder();
    try {
      for (;;) {
        const chunk = await reader.read();
        if (chunk.done) { break; }
        parse(decoder.decode(chunk.value, { stream: true }));
      }
      parse(decoder.decode());
      if (!done) { throw new Error("Install stream disconnected"); }
      if (failure) { throw new Error(failure); }
    } finally { await reader.cancel(); reader.releaseLock(); }
  }

  health(): Promise<Health> {
    return this.request("GET", "/health", { auth: false });
  }

  /** Paired hosts and their channel state (this computer's own daemon). */
  listHosts(): Promise<HostLinkList> {
    return this.request("GET", "/hosts");
  }
  /** What "make this server my bots' home" could send (names only; local daemon). */
  copyPlan(hostId: string): Promise<CopyPlan> {
    return this.request("GET", `/hosts/${encodeURIComponent(hostId)}/copy-setup`);
  }
  /** Send the ticked items; the LOCAL daemon reads the keys, the app never sees them. */
  copySetup(hostId: string, input: CopyRequest, ownerKey: string): Promise<CopyReport> {
    // The owner key (from the app, main window only) authorises sending keys.
    return this.request("POST", `/hosts/${encodeURIComponent(hostId)}/copy-setup`, {
      body: input,
      headers: { "x-praxi-owner-key": ownerKey }
    });
  }
  /** The host adopted as the bots' home, if any. */
  homeInfo(): Promise<HomeInfo> {
    return this.request("GET", "/hosts/home");
  }
  // ---- host administration (call on a client connected to the HOST) -----
  adminState(): Promise<AdminState> {
    return this.request("GET", "/admin/state");
  }
  /** The key is write-only; the host stores it in its secret store. */
  adminPutProvider(id: string, input: AdminProviderInput): Promise<AdminProvider> {
    return this.request("PUT", `/admin/providers/${encodeURIComponent(id)}`, { body: input });
  }
  adminSetDefault(id: string, model?: string): Promise<AdminProvider> {
    return this.request("PUT", "/admin/default-provider", { body: { id, model } });
  }
  adminCliEngines(): Promise<AdminCliEngine[]> {
    return this.request("GET", "/admin/cli-engines");
  }
  /** Install one CLI engine ON THE HOST (admin desktop only; the host runs its registry command). */
  adminCliEngineInstall(id: string): Promise<AdminCliInstall> {
    return this.request("POST", `/admin/cli-engines/${encodeURIComponent(id)}/install`);
  }
  adminCliEngineInstallStatus(id: string): Promise<AdminCliInstall> {
    return this.request("GET", `/admin/cli-engines/${encodeURIComponent(id)}/install`);
  }
  /**
   * Start the engine's own sign-in ON THE HOST (admin desktop only). The answer says which page
   * to open and which code to type or paste back; 409 `login_manual` when it cannot be driven.
   */
  adminCliEngineLogin(id: string): Promise<AdminCliLogin> {
    return this.request("POST", `/admin/cli-engines/${encodeURIComponent(id)}/login`);
  }
  /** A sign-in belongs to the desktop that started it and is named by the `job_id` of the start answer. */
  adminCliEngineLoginStatus(id: string, job: string): Promise<AdminCliLogin> {
    return this.request("GET", `/admin/cli-engines/${encodeURIComponent(id)}/login?job=${encodeURIComponent(job)}`);
  }
  /** The code the vendor's page showed; goes to that sign-in's waiting server process only. */
  adminCliEngineLoginCode(id: string, job: string, code: string): Promise<AdminCliLogin> {
    return this.request("POST", `/admin/cli-engines/${encodeURIComponent(id)}/login/code`, { body: { job_id: job, code } });
  }
  adminCliEngineLoginCancel(id: string, job: string): Promise<AdminCliLogin> {
    return this.request("DELETE", `/admin/cli-engines/${encodeURIComponent(id)}/login?job=${encodeURIComponent(job)}`);
  }
  /** Pair with a host using the ticket from `bot host invite --role desktop`. */
  pairHost(ticket: string): Promise<HostLinkInfo> {
    return this.request("POST", "/hosts/pair", { body: { ticket } });
  }
  /** Open (or reuse) the guarded channel to a paired host. */
  connectHost(id: string): Promise<HostLinkConnection> {
    return this.request("POST", `/hosts/${encodeURIComponent(id)}/connect`);
  }
  /** Let (or stop letting) a paired server's bots use THIS computer (local owner only). */
  setComputerAccess(id: string, switches: ComputerSwitches, ownerKey: string): Promise<void> {
    return this.request("PUT", `/hosts/${encodeURIComponent(id)}/computer-access`, {
      body: {
        enabled: switches.screen,
        always_read: switches.screen && switches.alwaysRead,
        browser: switches.browser,
        commands: switches.commands,
        files: switches.files
      },
      headers: { "x-praxi-owner-key": ownerKey }
    });
  }
  /** Requests of a server to act on this computer, waiting for the owner. */
  computerRequests(ownerKey: string): Promise<PcRequestList> {
    return this.request("GET", "/hosts/computer-requests", { headers: { "x-praxi-owner-key": ownerKey } });
  }
  decideComputerRequest(id: string, allow: boolean, ownerKey: string): Promise<void> {
    return this.request("POST", `/hosts/computer-requests/${encodeURIComponent(id)}`, {
      body: { allow },
      headers: { "x-praxi-owner-key": ownerKey }
    });
  }
  disconnectHost(id: string): Promise<void> {
    return this.request("DELETE", `/hosts/${encodeURIComponent(id)}/connect`);
  }
  /** "Add server" wizard (this computer's own daemon, local owner only). */
  startHostSetup(input: StartHostSetupInput): Promise<HostSetupJob> {
    return this.request("POST", "/hosts/setup", { body: input });
  }
  getHostSetup(job: string): Promise<HostSetupJob> {
    return this.request("GET", `/hosts/setup/${encodeURIComponent(job)}`);
  }
  answerHostSetupKey(job: string, accept: boolean): Promise<HostSetupJob> {
    return this.request("POST", `/hosts/setup/${encodeURIComponent(job)}/host-key`, {
      body: { accept }
    });
  }
  cancelHostSetup(job: string): Promise<HostSetupJob> {
    return this.request("POST", `/hosts/setup/${encodeURIComponent(job)}/cancel`);
  }
  /** Private key files found in `~/.ssh` (paths only). */
  async listSshKeys(): Promise<SshKeyHint[]> {
    const r = await this.request<{ keys: SshKeyHint[] }>("GET", "/hosts/setup/ssh-keys");
    return r.keys;
  }
  listBots(): Promise<Bot[]> {
    return this.request("GET", "/bots");
  }
  getBot(id: string): Promise<Bot> {
    return this.request("GET", `/bots/${id}`);
  }
  createBot(input: BotInput): Promise<Bot> {
    return this.request("POST", "/bots", { body: input });
  }
  updateBot(id: string, input: BotInput): Promise<Bot> {
    return this.request("PUT", `/bots/${id}`, { body: input });
  }
  /**
   * Change only the listed fields of a bot, atomically, if `expected_version` is still current;
   * a stale version is a `409` whose body carries the current bot (`conflictBot`).
   */
  patchBot(id: string, input: BotPatchInput): Promise<Bot> {
    return this.request("PATCH", `/bots/${id}`, { body: input });
  }
  /** Owner-only: engine mode, approval level and workspace of a bot. */
  putBotEngine(id: string, input: BotEngineInput): Promise<Bot> {
    return this.request("PUT", `/bots/${id}/engine`, { body: input });
  }
  /** Owner kill switch: back to Praxi-governed and end the bot's CLI processes. */
  killBotEngine(id: string): Promise<Bot> {
    return this.request("POST", `/bots/${id}/engine/kill`);
  }
  /** The built-in bot template gallery (data files shipped with the daemon). */
  profileTemplates(): Promise<SharedProfile[]> {
    return this.request("GET", "/profile-share/templates");
  }
  /** Owner-only: one bot as a shareable profile file (no secrets, grants, memory or paths). */
  exportProfile(botId: string, includeSchedules: boolean): Promise<ExportOutput> {
    return this.request("POST", "/profile-share/export", {
      body: { bot_id: botId, include_schedules: includeSchedules }
    });
  }
  /** What a profile file would be allowed to do under the chosen ceiling; creates nothing. */
  previewProfile(input: ImportInput): Promise<ImportPreview> {
    return this.request("POST", "/profile-share/preview", { body: input });
  }
  /** Owner-only: create a bot from a profile file, never above `permission_level`. */
  importProfile(input: ImportInput): Promise<ImportOutput> {
    return this.request("POST", "/profile-share/import", { body: input });
  }
  deleteBot(id: string): Promise<void> {
    return this.request("DELETE", `/bots/${id}`);
  }
  listGrants(botId: string): Promise<Grant[]> {
    return this.request("GET", `/bots/${botId}/grants`);
  }
  revokeGrant(botId: string, target: GrantTarget): Promise<void> {
    return this.request("DELETE", `/bots/${botId}/grants`, {
      body: { target }
    });
  }
  listSkills(): Promise<SkillsCatalog> {
    return this.request("GET", "/skills");
  }
  getBotSkills(botId: string): Promise<BotSkills> {
    return this.request("GET", `/bots/${encodeURIComponent(botId)}/skills`);
  }
  putBotSkills(botId: string, body: PutBotSkillsInput): Promise<BotSkills> {
    return this.request("PUT", `/bots/${encodeURIComponent(botId)}/skills`, {
      body
    });
  }

  searchChatHistory(q: string, offset = 0, signal?: AbortSignal): Promise<import("./types").SearchPage> {
    return this.request("GET", "/chats/search", { query: { q, offset }, signal });
  }
  exportChat(chatId: string, format: import("../lib/chatHistory").ExportFormat): Promise<Blob> {
    return this.requestBlob("GET", `/chats/${encodeURIComponent(chatId)}/export?format=${format}`);
  }

  listChats(): Promise<Chat[]> {
    return this.request("GET", "/chats");
  }
  createChat(botId: string, title?: string): Promise<Chat> {
    return this.request("POST", "/chats", { body: { bot_id: botId, title } });
  }
  updateChat(chatId: string, body: UpdateChatInput): Promise<Chat> {
    return this.request("PATCH", `/chats/${encodeURIComponent(chatId)}`, { body });
  }
  deleteChat(chatId: string): Promise<void> {
    return this.request("DELETE", `/chats/${encodeURIComponent(chatId)}`);
  }
  listMessages(chatId: string): Promise<Message[]> {
    return this.request("GET", `/chats/${chatId}/messages`);
  }
  trustedFolders(): Promise<{ folders: string[] }> {
    return this.request("GET", "/trusted-folders");
  }
  setFolderTrust(path: string, trusted: boolean, key: string): Promise<{ folders: string[] }> {
    return this.request(trusted ? "POST" : "DELETE", "/trusted-folders", {
      body: { path }, headers: { "x-praxi-owner-key": key }
    });
  }

  /** Upload one file for the next message of a chat; the daemon sniffs its type and size. */
  uploadAttachment(chatId: string, file: Blob, name: string): Promise<Attachment> {
    return this.request("POST", `/chats/${chatId}/attachments`, { raw: file, query: { name } });
  }
  /** The stored original of an attachment (an image to show in the message). */
  attachmentBlob(chatId: string, attachmentId: string): Promise<Blob> {
    return this.requestBlob("GET", `/chats/${chatId}/attachments/${attachmentId}`);
  }

  sendMessage(
    chatId: string,
    text: string,
    mode?: SendMode,
    ownerKey?: string,
    attachmentIds?: string[]
  ): Promise<SendResponse> {
    return this.request("POST", `/chats/${chatId}/messages`, {
      body: { text, mode, ...(attachmentIds?.length ? { attachment_ids: attachmentIds } : {}) },
      ...(ownerKey ? { headers: { "x-praxi-owner-key": ownerKey, "x-praxi-message-source": "local_ui" } } : {})
    });
  }

  /**
   * Regenerate the last answer (no `text`) or edit the last user message and resend it.
   * Owner only; the old turn is kept as superseded, a new run starts.
   */
  resendLastTurn(chatId: string, messageId: string, ownerKey: string | undefined, text?: string): Promise<ResendResponse> {
    return this.request("POST", `/chats/${chatId}/resend`, {
      body: { message_id: messageId, ...(text === undefined ? {} : { text }) },
      ...(ownerKey ? { headers: { "x-praxi-owner-key": ownerKey } } : {})
    });
  }

  listRuns(): Promise<Run[]> {
    return this.request("GET", "/runs");
  }
  proposeWorkflow(runId: string): Promise<WorkflowProposal> {
    return this.request("POST", `/runs/${encodeURIComponent(runId)}/workflow-proposal`);
  }
  listWorkflows(botId: string): Promise<WorkflowDefinition[]> {
    return this.request("GET", `/workflows?bot_id=${encodeURIComponent(botId)}`);
  }
  getWorkflow(name: string, botId: string): Promise<WorkflowDefinition> {
    return this.request("GET", `/workflows/${encodeURIComponent(name)}?bot_id=${encodeURIComponent(botId)}`);
  }
  saveWorkflow(input: SaveWorkflow): Promise<WorkflowDefinition> {
    return this.request("POST", "/workflows", { body: input });
  }
  deleteWorkflow(name: string, botId: string): Promise<void> {
    return this.request("DELETE", `/workflows/${encodeURIComponent(name)}?bot_id=${encodeURIComponent(botId)}`);
  }
  runWorkflow(name: string, input: RunWorkflow): Promise<WorkflowRun> {
    return this.request("POST", `/workflows/${encodeURIComponent(name)}/run`, { body: input });
  }
  cancelRun(runId: string): Promise<Run> {
    return this.request("POST", `/runs/${runId}/cancel`);
  }
  /**
   * One user Stop (`POST /v1/runs/stop`). Idempotent per `stop_id`: the daemon fixes the
   * target set on first receipt, so a retry must reuse the same id. `signal` bounds one attempt.
   */
  stopRuns(body: StopRunsInput, signal?: AbortSignal): Promise<StopRunsResponse> {
    return this.request("POST", "/runs/stop", { body, signal });
  }
  /** Model and token usage of one run (read-only; the message info popover). */
  getRunUsage(runId: string): Promise<RunUsage> {
    return this.request("GET", `/runs/${encodeURIComponent(runId)}/usage`);
  }
  resumeRun(runId: string): Promise<Run> {
    return this.request("POST", `/runs/${runId}/resume`);
  }

  /** Cache the route of a succeeded run (409 when the run is not eligible). */
  rememberRun(runId: string): Promise<RememberResponse> {
    return this.request("POST", `/runs/${runId}/remember`);
  }

  /** List collapsed file changes and capped inline diffs for a run. */
  getRunChanges(runId: string, offset = 0): Promise<RunChanges> {
    return this.request("GET", `/runs/${encodeURIComponent(runId)}/changes`, { query: { offset, limit: 50 } });
  }

  /** Preview restoring one checkpoint or `"all"` across a run. */
  previewRestore(runId: string, checkpointId: string): Promise<RestorePreview> {
    return this.request(
      "GET",
      `/runs/${encodeURIComponent(runId)}/changes/${encodeURIComponent(checkpointId)}/preview`
    );
  }

  /** Restore a checkpoint or `"all"` across a run (409 `modified_since` unless `force`). */
  restoreRunChanges(
    runId: string,
    checkpointId: string,
    force = false
  ): Promise<RestoreResponse> {
    return this.request(
      "POST",
      `/runs/${encodeURIComponent(runId)}/changes/${encodeURIComponent(checkpointId)}/restore`,
      { body: { force } }
    );
  }

  listApprovals(runId?: string): Promise<Approval[]> {
    return this.request("GET", "/approvals", { query: { run_id: runId } });
  }
  decideApproval(
    callId: string,
    decision: Decision,
    scope?: Scope,
    reason?: string,
    ownerKey?: string
  ): Promise<{ call_id: string; decision: Decision }> {
    return this.request("POST", `/approvals/${callId}`, {
      body: { decision, scope, reason },
      ...(ownerKey ? { headers: { "x-praxi-owner-key": ownerKey } } : {})
    });
  }
  answerQuestion(callId: string, answer: string): Promise<{ call_id: string }> {
    return this.request("POST", `/questions/${callId}`, { body: { answer } });
  }

  /**
   * Answer a bot's credential request: the value goes into the vault, the run only learns
   * `stored`. Owner-only (the owner key is added for this computer's daemon). The body is built
   * by the caller and never logged here.
   */
  submitSecretRequest(
    callId: string,
    body: { value?: string; username?: string; password?: string; origins: string[]; programs: string[] }
  ): Promise<SecretRequestAnswer> {
    return this.request("POST", `/secret-requests/${encodeURIComponent(callId)}`, { body });
  }

  cancelSecretRequest(callId: string): Promise<SecretRequestAnswer> {
    return this.request("POST", `/secret-requests/${encodeURIComponent(callId)}/cancel`);
  }

  /** Store a value the owner typed or pasted in the vault (`POST /v1/fill-secrets`, write-only). */
  createFillSecret(input: { name: string; value: string; origins?: string[]; programs?: string[] }): Promise<FillSecretInfo> {
    return this.request("POST", "/fill-secrets", { body: input });
  }

  listSchedules(): Promise<Schedule[]> {
    return this.request("GET", "/schedules");
  }
  getSchedule(id: string): Promise<Schedule> {
    return this.request("GET", `/schedules/${id}`);
  }
  createSchedule(input: ScheduleInput): Promise<Schedule> {
    return this.request("POST", "/schedules", { body: input });
  }
  updateSchedule(id: string, input: ScheduleInput): Promise<Schedule> {
    return this.request("PUT", `/schedules/${id}`, { body: input });
  }
  deleteSchedule(id: string): Promise<void> {
    return this.request("DELETE", `/schedules/${id}`);
  }
  /** Fire a schedule now (409 while a run of it is still in flight). */
  runScheduleNow(id: string): Promise<AcceptedResponse> {
    return this.request("POST", `/schedules/${id}/run-now`);
  }
  /**
   * Bring a schedule that moved to a server back to this computer. The server's copy is
   * switched off first; when it cannot be reached the call answers 409 `host_unreachable` until
   * `confirmHostUnreachable` is set (its copy may then still be active).
   */
  reclaimSchedule(id: string, confirmHostUnreachable = false): Promise<ReclaimResult> {
    return this.request("POST", `/schedules/${id}/reclaim`, {
      body: { confirm_host_unreachable: confirmHostUnreachable }
    });
  }
  /** Turn an English / Turkish phrase into a schedule spec. */
  parseSchedule(text: string, tz?: string): Promise<ParsedSchedule> {
    return this.request("POST", "/schedules/parse", { body: { text, tz } });
  }
  /** Preview upcoming firing timestamps for a schedule spec or phrase. */
  previewSchedule(input: PreviewSchedule): Promise<ParsedSchedule> {
    return this.request("POST", "/schedules/preview", { body: input });
  }
  /** Occurrence and run history for a schedule (newest first). */
  getScheduleHistory(id: string): Promise<ScheduleHistoryEntry[]> {
    return this.request("GET", `/schedules/${id}/history`);
  }
  /** Explicitly retry a failed, missed, or interrupted occurrence (owner only, idempotent). */
  retryScheduleOccurrence(id: string, dueMs: number): Promise<AcceptedResponse> {
    return this.request("POST", `/schedules/${id}/occurrences/${dueMs}/retry`);
  }

  /** Monthly spending limit and month-to-date spend. */
  getSpendLimit(): Promise<SpendLimit> {
    return this.request("GET", "/spend-limit");
  }
  /** Set (USD, at least 1) or clear (`null`) the monthly limit; owner only. */
  setSpendLimit(limitUsd: number | null): Promise<SpendLimit> {
    return this.request("PUT", "/spend-limit", { body: { limit_usd: limitUsd } });
  }

  usage(
    opts?: number | { since_ms?: number; until_ms?: number; days?: number; tz?: string }
  ): Promise<UsageSummary> {
    const query: Query =
      typeof opts === "number" ? { since_ms: opts } : (opts ?? {});
    return this.request("GET", "/usage", { query });
  }

  /** Formula-safe CSV export of usage over the same range (owner only). */
  usageExport(
    opts?: { since_ms?: number; until_ms?: number; days?: number; tz?: string }
  ): Promise<string> {
    return this.requestText("GET", "/usage/export", { query: opts });
  }

  /** Daemon's current controller registration & owned takeover status. */
  getControlState(): Promise<ControlStateDto> {
    return this.request("GET", "/control/state");
  }
  /** Heartbeat of the desktop app as controller (see `controlBridge.ts`). */
  putControlState(
    mode: ControlModeName,
    release?: boolean
  ): Promise<ControlStateDto> {
    return this.request("PUT", "/control/state", {
      body: release ? { mode, release: true } : { mode }
    });
  }
  /** Release an owned screen takeover or clear a legacy latch. */
  releaseControl(req?: ReleaseRequest, signal?: AbortSignal): Promise<ControlStateDto> {
    return this.request("POST", "/control/release", req ? { body: req, signal } : { signal });
  }
  /** Local Stop override: interrupts any active takeover and keeps the pause latch. */
  overrideControl(signal?: AbortSignal): Promise<ControlStateDto> {
    return this.request("POST", "/control/override", { signal });
  }
  /** Local user recovery: clears an interrupted/recovered latch after key reset. */
  recoverControl(): Promise<ControlStateDto> {
    return this.request("POST", "/control/recover");
  }
  requestTakeover(input: TakeoverRequest): Promise<TakeoverAccepted> {
    return this.request("POST", "/control/takeover", { body: input });
  }
  decideControl(
    requestId: string,
    decision: ControlVerdict,
    reason?: string
  ): Promise<{ request_id: string; decision: ControlVerdict }> {
    return this.request("POST", `/control/${requestId}`, {
      body: { decision, reason }
    });
  }

  listViewTargets(): Promise<TargetInfo[]> {
    return this.request("GET", "/view/targets");
  }
  /** Which kinds of screen this host cannot offer, and why. */
  listViewAvailability(): Promise<TargetUnavailable[]> {
    return this.request("GET", "/view/availability");
  }
  /** Idle / starting / live / paused-idle / unavailable, and the host's idle limit. */
  peerLink(signal?: AbortSignal): Promise<PeerLink | null> {
    return this.request("GET", "/peer/link", { signal });
  }

  viewStatus(): Promise<ScreenStatus> {
    return this.request("GET", "/view/status");
  }
  /** One still image of a screen (the host captures a single frame, then stops capturing). */
  async viewThumbnail(targetId: string, signal?: AbortSignal): Promise<Blob> {
    const res = await this.send("GET", `/view/targets/${encodeURIComponent(targetId)}/thumbnail`, { signal });
    return res.blob();
  }
  // ---- Server terminal (this computer's daemon forwards to the host; owner key is added by `send`)
  terminalOpen(hostId: string, cols: number, rows: number): Promise<TerminalInfo> {
    return this.request("POST", `/hosts/${encodeURIComponent(hostId)}/terminal`, { body: { cols, rows } });
  }
  terminalOutput(hostId: string, id: string, after: number, waitMs: number, signal?: AbortSignal): Promise<TerminalOutput> {
    return this.request("GET", `/hosts/${encodeURIComponent(hostId)}/terminal/${encodeURIComponent(id)}/output`, {
      query: { after, wait_ms: waitMs },
      signal
    });
  }
  terminalInput(hostId: string, id: string, data: string): Promise<void> {
    return this.request("POST", `/hosts/${encodeURIComponent(hostId)}/terminal/${encodeURIComponent(id)}/input`, { body: { data } });
  }
  terminalResize(hostId: string, id: string, cols: number, rows: number): Promise<void> {
    return this.request("POST", `/hosts/${encodeURIComponent(hostId)}/terminal/${encodeURIComponent(id)}/resize`, { body: { cols, rows } });
  }
  terminalClose(hostId: string, id: string): Promise<void> {
    return this.request("DELETE", `/hosts/${encodeURIComponent(hostId)}/terminal/${encodeURIComponent(id)}`);
  }
  private readonly staleViewSessions = new Set<string>();

  async createViewSession(input: CreateSessionInput): Promise<SessionInfo> {
    const create = () => this.request<SessionInfo>("POST", "/view/sessions", { body: input });
    try {
      return await create();
    } catch (err) {
      if (!(err instanceof ApiError) || err.code !== "view_quota" || this.staleViewSessions.size === 0) {
        throw err;
      }
      // Retry only DELETEs for sessions this client has already abandoned.
      await Promise.allSettled([...this.staleViewSessions].map((id) => this.deleteViewSession(id)));
      return create();
    }
  }
  updateViewSession(
    id: string,
    patch: SessionUpdate
  ): Promise<EffectiveProfile> {
    return this.request("PATCH", `/view/sessions/${encodeURIComponent(id)}`, {
      body: patch
    });
  }
  /** The newest frame on screen; the host paces its frames by it. Cheap: no refresh. */
  ackViewFrame(id: string, lastRenderedFrameSeq: number): Promise<void> {
    return this.request(
      "POST",
      `/view/sessions/${encodeURIComponent(id)}/ack`,
      { body: { last_rendered_frame_seq: lastRenderedFrameSeq } }
    );
  }
  heartbeatViewSession(id: string, input: ViewHeartbeat): Promise<void> {
    return this.request(
      "PUT",
      `/view/sessions/${encodeURIComponent(id)}/heartbeat`,
      { body: input }
    );
  }
  async deleteViewSession(id: string): Promise<void> {
    this.staleViewSessions.add(id);
    // The daemon reaps dormant sessions after their lease. Bound local tracking as well.
    if (this.staleViewSessions.size > 8) {
      const oldest = this.staleViewSessions.values().next().value;
      if (oldest) this.staleViewSessions.delete(oldest);
    }
    try {
      await this.request("DELETE", `/view/sessions/${encodeURIComponent(id)}`);
      this.staleViewSessions.delete(id);
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 410)) {
        this.staleViewSessions.delete(id);
        return;
      }
      throw err;
    }
  }
  sendViewInput(id: string, batch: InputBatch): Promise<InputResult> {
    return this.request(
      "POST",
      `/view/sessions/${encodeURIComponent(id)}/input`,
      { body: batch }
    );
  }
  setScreenPrivileges(
    peerKey: string,
    input: ScreenPrivilegesInput
  ): Promise<void> {
    return this.request(
      "PUT",
      `/view/privileges/${encodeURIComponent(peerKey)}`,
      { body: input }
    );
  }

  /**
   * Open the chunked binary frame stream with Bearer auth header and `no-store`.
   * Throws `ApiError` on non-2xx responses; never sends credentials in URLs.
   */
  async openViewFrameStream(
    sessionId: string,
    signal?: AbortSignal
  ): Promise<Response> {
    const url = new URL(
      `${this.baseUrl}/v1/view/sessions/${encodeURIComponent(sessionId)}/frames`
    );
    let res: Response;
    try {
      res = await fetch(url, {
        method: "GET",
        headers: {
          ...this.authHeaders(),
          Accept: LIVE_VIEW_MEDIA_TYPE
        },
        cache: "no-store",
        signal
      });
    } catch (e) {
      throw new ApiError(
        0,
        "network",
        e instanceof Error ? e.message : String(e)
      );
    }
    if (!res.ok) {
      let code = "http_error";
      let message = `HTTP ${res.status}`;
      let parsed: unknown;
      try {
        const body = (await res.json()) as ErrorBody;
        parsed = body;
        code = body.error.code;
        message = body.error.message;
      } catch {
        // not JSON
      }
      throw new ApiError(res.status, code, message, parsed);
    }
    return res;
  }

  getPolicy(): Promise<PolicyDto> {
    return this.request("GET", "/policy");
  }
  putPolicy(input: PolicyInput): Promise<PolicyDto> {
    return this.request("PUT", "/policy", { body: input });
  }

  providers(): Promise<ProvidersDto> {
    return this.request("GET", "/providers");
  }
  detectedProviders(refresh = false): Promise<DetectedProviderDto[]> {
    return this.request("GET", "/providers/detected", {
      query: refresh ? { refresh: "1" } : undefined
    });
  }
  enableCliProvider(id: string): Promise<ConfiguredProviderDto> {
    return this.request("POST", "/providers/cli/enable", {
      body: { id }
    });
  }
  /** Switch a coding CLI off; the daemon remembers it and never turns it on again by itself. */
  disableCliProvider(id: string): Promise<ConfiguredProviderDto> {
    return this.request("POST", "/providers/cli/disable", {
      body: { id }
    });
  }
  // ---- MCP servers & secrets -------------------------------------------

  listMcpServers(): Promise<McpServer[]> {
    return this.request("GET", "/mcp/servers");
  }
  createMcpServer(config: McpServerConfig): Promise<McpServer> {
    return this.request("POST", "/mcp/servers", { body: config });
  }
  updateMcpServer(name: string, patch: McpServerPatch): Promise<McpServer> {
    return this.request("PUT", `/mcp/servers/${encodeURIComponent(name)}`, {
      body: patch
    });
  }
  deleteMcpServer(name: string): Promise<void> {
    return this.request("DELETE", `/mcp/servers/${encodeURIComponent(name)}`);
  }
  reconnectMcpServer(name: string): Promise<McpServer> {
    return this.request(
      "POST",
      `/mcp/servers/${encodeURIComponent(name)}/reconnect`
    );
  }
  listMcpTools(name: string): Promise<McpTool[]> {
    return this.request(
      "GET",
      `/mcp/servers/${encodeURIComponent(name)}/tools`
    );
  }
  mcpCatalog(): Promise<McpCatalogEntry[]> {
    return this.request("GET", "/mcp/catalog");
  }
  startMcpAuth(name: string): Promise<McpAuthStart> {
    return this.request("POST", `/mcp/servers/${encodeURIComponent(name)}/auth/start`);
  }
  /** Finish a sign-in with the redirect address pasted from the browser (headless daemon). */
  completeMcpAuth(name: string, redirectUrl: string): Promise<McpAuthState> {
    return this.request("POST", `/mcp/servers/${encodeURIComponent(name)}/auth/callback`, {
      body: { redirect_url: redirectUrl }
    });
  }
  cancelMcpAuth(name: string): Promise<McpAuthState> {
    return this.request("POST", `/mcp/servers/${encodeURIComponent(name)}/auth/cancel`);
  }
  mcpAuthStatus(name: string): Promise<McpAuthState> {
    return this.request("GET", `/mcp/servers/${encodeURIComponent(name)}/auth/status`);
  }
  logoutMcpAuth(name: string): Promise<McpAuthState> {
    return this.request("POST", `/mcp/servers/${encodeURIComponent(name)}/auth/logout`);
  }
  /** Store a secret (write-only: the daemon never returns the value). */
  setSecret(name: string, value: string): Promise<void> {
    return this.request("POST", `/secrets/${encodeURIComponent(name)}`, {
      body: { value }
    });
  }

  // ---- Composio app connections (owner-only routes) ---------------------

  composioStatus(): Promise<ComposioStatus> {
    return this.request("GET", "/composio");
  }
  /** One-time browser login: returns the Composio page to open; no key is typed or copied. */
  composioLoginStart(): Promise<ComposioLogin> {
    return this.request("POST", "/composio/login");
  }
  composioLoginStatus(): Promise<ComposioLoginStatus> {
    return this.request("GET", "/composio/login");
  }
  /** Advanced: the owner's own project key; write-only, never returned. */
  setComposioKey(value: string): Promise<ComposioStatus> {
    return this.request("PUT", "/composio/key", { body: { value } });
  }
  removeComposioKey(): Promise<ComposioStatus> {
    return this.request("DELETE", "/composio/key");
  }
  composioToolkits(q: {
    search?: string;
    group?: string;
    cursor?: string;
    limit?: number;
  }): Promise<ComposioCatalog> {
    return this.request("GET", "/composio/toolkits", {
      query: {
        search: q.search || null,
        group: q.group ?? null,
        cursor: q.cursor ?? null,
        limit: q.limit ?? null
      }
    });
  }
  // ---- Missing local components (uv, Node.js): detect and install ----------
  component(id: ComponentId): Promise<Component> {
    return this.request("GET", `/components/${id}`);
  }
  installComponent(id: ComponentId): Promise<Component> {
    return this.request("POST", `/components/${id}/install`);
  }
  composioApps(): Promise<ComposioApp[]> {
    return this.request("GET", "/composio/apps");
  }
  /** The app's logo as the daemon fetched, validated and cached it (404 when none is known). */
  composioLogo(slug: string): Promise<Blob> {
    return this.requestBlob("GET", `/composio/logos/${encodeURIComponent(slug)}`);
  }
  composioApp(slug: string): Promise<ComposioApp> {
    return this.request("GET", `/composio/apps/${encodeURIComponent(slug)}`);
  }
  composioConnect(slug: string): Promise<ComposioConnect> {
    return this.request("POST", `/composio/apps/${encodeURIComponent(slug)}/connect`);
  }
  composioSetEnabled(slug: string, enabled: boolean): Promise<ComposioApp> {
    return this.request("PUT", `/composio/apps/${encodeURIComponent(slug)}/enabled`, {
      body: { enabled }
    });
  }
  composioDisconnect(slug: string): Promise<ComposioDisconnect> {
    return this.request("DELETE", `/composio/apps/${encodeURIComponent(slug)}`);
  }

  // ---- E-mail accounts -------------------------------------------------

  listEmailAccounts(): Promise<EmailAccount[]> {
    return this.request("GET", "/email/accounts");
  }
  /** The `password` is write-only: the daemon stores it and never returns it. */
  createEmailAccount(input: EmailAccountInput): Promise<EmailAccount> {
    return this.request("POST", "/email/accounts", { body: input });
  }
  updateEmailAccount(
    name: string,
    patch: EmailAccountPatch
  ): Promise<EmailAccount> {
    return this.request("PUT", `/email/accounts/${encodeURIComponent(name)}`, {
      body: patch
    });
  }
  /**
   * Forget an account. With `deleteInbox` the inbox is first deleted at the
   * provider too (irreversible; AgentMail accounts only).
   */
  deleteEmailAccount(
    name: string,
    opts: { deleteInbox?: boolean } = {}
  ): Promise<void> {
    return this.request(
      "DELETE",
      `/email/accounts/${encodeURIComponent(name)}`,
      opts.deleteInbox ? { query: { delete_inbox: "true" } } : {}
    );
  }
  /** IMAP login + SMTP authentication of a saved account; nothing is sent. */
  testEmailAccount(name: string): Promise<EmailTestReport> {
    return this.request(
      "POST",
      `/email/accounts/${encodeURIComponent(name)}/test`
    );
  }
  /** Newest messages of the inbox, headers only (owner-only; nothing is marked read). */
  listEmailMessages(name: string, limit = 20): Promise<EmailMessage[]> {
    return this.request("GET", `/email/accounts/${encodeURIComponent(name)}/messages`, {
      query: { limit: String(limit) }
    });
  }
  /** One fixed test message from the mailbox to its own address (owner-only). */
  sendEmailTest(name: string): Promise<EmailSendTestResponse> {
    return this.request("POST", `/email/accounts/${encodeURIComponent(name)}/send-test`);
  }
  /** The same for an account that is not saved (nothing is stored). */
  testEmailDraft(input: EmailAccountInput): Promise<EmailTestReport> {
    return this.request("POST", "/email/test", { body: input });
  }

  reloadProviders(): Promise<{
    default_provider?: string | null;
    active: string[];
  }> {
    return this.request("POST", "/providers/reload");
  }

  // ---- Diagnostics -----------------------------------------------------

  /** Read-only checks (`GET /v1/doctor`, at most ~10 s). */
  doctor(): Promise<DoctorReport> {
    return this.request("GET", "/doctor");
  }
  /** Run one allow-listed repair; only ever called from a user click. */
  doctorRepair(id: RepairId): Promise<RepairResult> {
    return this.request("POST", `/doctor/repair/${encodeURIComponent(id)}`);
  }
  /** Redacted Markdown support report. */
  doctorReport(): Promise<string> {
    return this.requestText("GET", "/doctor/report");
  }

  getVoice(): Promise<VoiceConfig> {
    return this.request("GET", "/voice");
  }

  updateVoice(input: VoiceConfigInput): Promise<VoiceConfig> {
    return this.request("PUT", "/voice", { body: input });
  }

  /** Dictation: the recording goes to the service the owner configured. */
  transcribe(input: TranscribeInput): Promise<TranscribeOutput> {
    return this.request("POST", "/voice/transcribe", { body: input });
  }

  getWebSearch(): Promise<WebSearchConfig> {
    return this.request("GET", "/web/search");
  }

  updateWebSearch(input: WebSearchConfigInput): Promise<WebSearchConfig> {
    return this.request("PUT", "/web/search", { body: input });
  }

  testWebSearch(input: WebSearchTestInput = {}): Promise<WebSearchTestReport> {
    return this.request("POST", "/web/search/test", { body: input });
  }
}
