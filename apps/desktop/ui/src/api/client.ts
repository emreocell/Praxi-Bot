import type { Connection } from "./connection";
import type {
  AcceptedResponse,
  Approval,
  Bot,
  BotInput,
  Chat,
  ControlModeName,
  ControlStateDto,
  ControlVerdict,
  Decision,
  ErrorBody,
  Grant,
  GrantTarget,
  Health,
  EmailAccount,
  EmailAccountInput,
  EmailAccountPatch,
  EmailTestReport,
  McpCatalogEntry,
  McpServer,
  McpServerPatch,
  McpServerConfig,
  McpTool,
  Message,
  ParsedSchedule,
  PolicyDto,
  PolicyInput,
  ProvidersDto,
  RememberResponse,
  Run,
  Schedule,
  ScheduleInput,
  Scope,
  SendMode,
  SendResponse,
  UsageSummary
} from "./types";

/** An error response of the daemon (`{"error": {"code", "message"}}`) or a network failure. */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
  }
}

type Query = Record<string, string | number | null | undefined>;

/**
 * Small typed fetch wrapper over the daemon API (`/v1`).  Request / response
 * shapes come from the generated OpenAPI types (see `types.ts`).
 */
export class ApiClient {
  readonly baseUrl: string;
  private readonly token: string;

  constructor(conn: Pick<Connection, "base_url" | "token">) {
    this.baseUrl = conn.base_url.replace(/\/$/, "");
    this.token = conn.token;
  }

  /** Headers for a request, also used by the SSE reader. */
  authHeaders(): Record<string, string> {
    return { Authorization: `Bearer ${this.token}` };
  }

  protected async request<T>(
    method: string,
    path: string,
    opts: { body?: unknown; query?: Query; auth?: boolean } = {}
  ): Promise<T> {
    const url = new URL(`${this.baseUrl}/v1${path}`);
    for (const [k, v] of Object.entries(opts.query ?? {})) {
      if (v !== null && v !== undefined) {
        url.searchParams.set(k, String(v));
      }
    }
    const headers: Record<string, string> = {};
    if (opts.auth !== false) {
      Object.assign(headers, this.authHeaders());
    }
    if (opts.body !== undefined) {
      headers["Content-Type"] = "application/json";
    }
    let res: Response;
    try {
      res = await fetch(url, {
        method,
        headers,
        body: opts.body === undefined ? undefined : JSON.stringify(opts.body)
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
      try {
        const body = (await res.json()) as ErrorBody;
        code = body.error.code;
        message = body.error.message;
      } catch {
        // not JSON
      }
      throw new ApiError(res.status, code, message);
    }
    if (res.status === 204) {
      return undefined as T;
    }
    return (await res.json()) as T;
  }

  health(): Promise<Health> {
    return this.request("GET", "/health", { auth: false });
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

  listChats(): Promise<Chat[]> {
    return this.request("GET", "/chats");
  }
  createChat(botId: string, title?: string): Promise<Chat> {
    return this.request("POST", "/chats", { body: { bot_id: botId, title } });
  }
  listMessages(chatId: string): Promise<Message[]> {
    return this.request("GET", `/chats/${chatId}/messages`);
  }
  sendMessage(
    chatId: string,
    text: string,
    mode?: SendMode
  ): Promise<SendResponse> {
    return this.request("POST", `/chats/${chatId}/messages`, {
      body: { text, mode }
    });
  }

  listRuns(): Promise<Run[]> {
    return this.request("GET", "/runs");
  }
  cancelRun(runId: string): Promise<Run> {
    return this.request("POST", `/runs/${runId}/cancel`);
  }
  resumeRun(runId: string): Promise<Run> {
    return this.request("POST", `/runs/${runId}/resume`);
  }

  /** Cache the route of a succeeded run (409 when the run is not eligible). */
  rememberRun(runId: string): Promise<RememberResponse> {
    return this.request("POST", `/runs/${runId}/remember`);
  }

  listApprovals(runId?: string): Promise<Approval[]> {
    return this.request("GET", "/approvals", { query: { run_id: runId } });
  }
  decideApproval(
    callId: string,
    decision: Decision,
    scope?: Scope,
    reason?: string
  ): Promise<{ call_id: string; decision: Decision }> {
    return this.request("POST", `/approvals/${callId}`, {
      body: { decision, scope, reason }
    });
  }
  answerQuestion(callId: string, answer: string): Promise<{ call_id: string }> {
    return this.request("POST", `/questions/${callId}`, { body: { answer } });
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
  /** Turn an English / Turkish phrase into a schedule spec. */
  parseSchedule(text: string, tz?: string): Promise<ParsedSchedule> {
    return this.request("POST", "/schedules/parse", { body: { text, tz } });
  }

  usage(sinceMs?: number): Promise<UsageSummary> {
    return this.request("GET", "/usage", { query: { since_ms: sinceMs } });
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
  decideControl(
    requestId: string,
    decision: ControlVerdict,
    reason?: string
  ): Promise<{ request_id: string; decision: ControlVerdict }> {
    return this.request("POST", `/control/${requestId}`, {
      body: { decision, reason }
    });
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
  /** Store a secret (write-only: the daemon never returns the value). */
  setSecret(name: string, value: string): Promise<void> {
    return this.request("POST", `/secrets/${encodeURIComponent(name)}`, {
      body: { value }
    });
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
}
