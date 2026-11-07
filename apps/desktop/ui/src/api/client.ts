import type { Connection } from "./connection";
import type {
  Approval,
  Bot,
  BotInput,
  Chat,
  Decision,
  ErrorBody,
  Grant,
  GrantTarget,
  Health,
  Message,
  Run,
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

  private async request<T>(
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

  usage(sinceMs?: number): Promise<UsageSummary> {
    return this.request("GET", "/usage", { query: { since_ms: sinceMs } });
  }
}
