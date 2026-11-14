import { ApiClient, ApiError } from "./client.ts";
import { MOCK_MCP_CATALOG } from "./mockCatalog.ts";
import type {
  Bot,
  Chat,
  ConfiguredProviderDto,
  DetectedProviderDto,
  EmailAccount,
  EmailAccountInput,
  McpServer,
  McpServerConfig,
  McpTool,
  Message,
  PolicyDto,
  ProvidersDto,
  UsageSummary,
  GlobalEnvelope
} from "./types.ts";

import { isMockMode, MOCK_BASE_URL } from "./mockMode.ts";
export { isMockMode, MOCK_BASE_URL };

const NOW = 1_750_000_000_000;

const ATLAS = "11111111-1111-4111-8111-111111111111";
const NOVA = "22222222-2222-4222-8222-222222222222";

function bot(id: string, name: string, color: string, model: string): Bot {
  return {
    id,
    name,
    created_at_ms: NOW,
    config: {
      bot: { id: name.toLowerCase(), name, color, persona: "" },
      models: { executor: model, planner: model, classifier: model }
    }
  };
}

const BOTS: Bot[] = [
  bot(ATLAS, "Atlas", "#9D9AE8", "anthropic/claude-sonnet-5-5"),
  bot(NOVA, "Nova", "#57BAA4", "gemini/gemini-2.5-flash"),
  bot("bot-milo", "Milo", "#E4B75C", "openai/gpt-4.1"),
  bot("bot-ember", "Ember", "#DC8D76", "ollama/qwen3:8b")
];

const previews = ["Haftalık özetin hazır.", "Kaynakları inceliyorum…", "Tasarım dosyalarını düzenledim.", "Devam etmek için onayını bekliyorum."];
BOTS.forEach((b, i) => { Object.assign((b.config as { bot: object }).bot, { preview: previews[i], state: ["idle", "thinking", "working", "waiting-approval"][i], unread: i === 2 }); });
const LONG_PROMPT = Array.from({ length: 9 }, (_, i) => `${i + 1}. Geçen haftanın GitHub kapanan sorunlarını, takvimdeki toplantıları ve gelen kutusundaki yanıt bekleyen iletileri tara; her biri için tek cümlelik bir özet çıkar ve önem sırasına göre listele.`).join(String.fromCharCode(10));
const SCHEDULES = [
  { id: "sch-1", origin: "user", bot_id: ATLAS, name: "Haftalık özet", enabled: true, has_webhook: false, created_at_ms: NOW, last_run_at_ms: NOW - 6 * 86400000, next_run_at_ms: NOW + 86400000, misfire_policy: "skip", spec: { cron: "0 9 * * 1 Europe/Istanbul" }, payload: { kind: "agent_prompt", text: "Geçen haftanın GitHub ve takvim hareketlerinden kısa bir özet hazırla." } },
  { id: "sch-2", origin: "bot", bot_id: NOVA, name: "Kaynak taraması", enabled: true, has_webhook: true, created_at_ms: NOW, last_run_at_ms: null, next_run_at_ms: NOW + 3600000, misfire_policy: "run_once", spec: { every: { seconds: 3600 } }, payload: { kind: "agent_prompt", text: "Takip listesindeki sitelerde yeni içerik var mı bak." } },
  { id: "sch-3", origin: "user", bot_id: "bot-milo", name: "Yedek kontrolü", enabled: false, has_webhook: false, created_at_ms: NOW, last_run_at_ms: NOW - 86400000, next_run_at_ms: null, misfire_policy: "skip", spec: { cron: "30 22 * * * Europe/Istanbul" }, payload: { kind: "workflow", name: "backup-check", params: {} } }
] as unknown as import("./types").Schedule[];
const CHAT_ID = "33333333-3333-4333-8333-333333333333";
const CHATS: Chat[] = [
  {
    id: CHAT_ID,
    chat_id: CHAT_ID,
    bot_id: ATLAS,
    title: "Haftalık çalışma özeti",
    created_at_ms: NOW
  }
];

const PROVIDERS: ProvidersDto = {
  active: ["anthropic", "openai", "gemini", "ollama"],
  default_provider: "anthropic",
  catalog: [
    {
      id: "anthropic",
      auth: "api_key_env",
      base_url: "https://api.anthropic.com",
      configured: true,
      key_env: "ANTHROPIC_API_KEY",
      kind: "anthropic",
      tools: true,
      vision: true
    },
    {
      id: "gemini",
      auth: "api_key_env",
      base_url: "https://generativelanguage.googleapis.com",
      configured: true,
      key_env: "GEMINI_API_KEY",
      kind: "gemini",
      tools: true,
      vision: true
    },
    {
      id: "openrouter",
      auth: "api_key_env",
      base_url: "https://openrouter.ai/api/v1",
      configured: false,
      key_env: "OPENROUTER_API_KEY",
      kind: "openai_compatible",
      tools: true,
      vision: false
    },
    {
      id: "ollama",
      auth: "none",
      base_url: "http://localhost:11434/v1",
      configured: true,
      key_env: null,
      kind: "openai_compatible",
      tools: true,
      vision: false
    }
  ],
  configured: [
    { id: "openai", active: true, custom: false, default_model: "gpt-4.1", enabled: true, has_key: true, is_default: false, models: ["gpt-4.1", "o3"] },
    {
      id: "anthropic",
      active: true,
      custom: false,
      default_model: "claude-sonnet-5-5",
      enabled: true,
      has_key: true,
      is_default: true,
      models: ["claude-sonnet-5-5", "claude-opus-5-5", "claude-haiku-5-5"]
    },
    {
      id: "gemini",
      active: true,
      custom: false,
      default_model: "gemini-2.5-flash",
      enabled: true,
      has_key: true,
      is_default: false,
      models: ["gemini-2.5-flash", "gemini-2.5-flash-lite", "gemini-3.1-pro-preview"]
    },
    {
      id: "ollama",
      active: true,
      custom: false,
      default_model: "qwen3:8b",
      enabled: true,
      has_key: false,
      is_default: false,
      models: ["qwen3:8b", "llama3.2"]
    }
  ]
};

const DETECTED_PROVIDERS: DetectedProviderDto[] = [
  {
    id: "claude",
    display_name: "Claude Code",
    installed: true,
    version: "2.1.42",
    auth: "signed_in",
    account_label: "emre@praxi.bot",
    runnable: true,
    detail: null,
    message: null
  },
  {
    id: "codex",
    display_name: "Codex",
    installed: true,
    version: "0.104.0",
    auth: "signed_in",
    account_label: "chatgpt@praxi.bot",
    runnable: false,
    detail: "codex_unconfinable",
    message:
      "The installed Codex cannot disable unified_exec, so Praxi will not run it."
  },
  {
    id: "cursor",
    display_name: "Cursor",
    installed: true,
    version: "0.49.2",
    auth: "signed_out",
    account_label: null,
    runnable: false,
    detail: null,
    message: null
  },
  {
    id: "opencode",
    display_name: "OpenCode",
    installed: false,
    version: null,
    auth: "unknown",
    account_label: null,
    runnable: false,
    detail: "not_on_path",
    message: null
  },
  {
    id: "gemini",
    display_name: "Gemini CLI",
    installed: false,
    version: null,
    auth: "unknown",
    account_label: null,
    runnable: false,
    detail: "not_on_path",
    message: null
  },
  {
    id: "chatgpt-plan",
    display_name: "ChatGPT plan",
    installed: true,
    version: "0.104.0",
    auth: "unknown",
    account_label: null,
    runnable: false,
    detail: "codex_unconfinable",
    message:
      "The installed Codex cannot disable unified_exec, so Praxi will not run it."
  }
];

const USAGE: UsageSummary = {
  runs: 42,
  total: {
    model_calls: 311,
    prompt_tokens: 1_204_500,
    completion_tokens: 188_300,
    cached_tokens: 640_000,
    cost_usd: 3.4127
  },
  by_bot: [
    {
      bot_id: ATLAS,
      model_calls: 220,
      prompt_tokens: 900_000,
      completion_tokens: 140_000,
      cached_tokens: 500_000,
      cost_usd: 2.9
    },
    {
      bot_id: NOVA,
      model_calls: 91,
      prompt_tokens: 304_500,
      completion_tokens: 48_300,
      cached_tokens: 140_000,
      cost_usd: 0.5127
    }
  ]
};

const MCP_TOOLS: McpTool[] = [
  { name: "search", description: "Search", effect: "read" },
  { name: "create", description: "Create an item", effect: "write" }
];

function transportKind(
  transport: McpServerConfig["transport"]
): McpServer["transport"] {
  return typeof transport === "string" ? transport : transport.type;
}

function serverFromConfig(c: McpServerConfig, category = "other"): McpServer {
  return {
    name: c.name,
    transport: transportKind(c.transport),
    command: c.command ?? null,
    args: c.args ?? null,
    url: c.url ?? null,
    enabled: c.enabled ?? true,
    status: "connected",
    error: null,
    tool_count: 8,
    category: c.category ?? category,
    trust_hints: false,
    public_content: c.public_content ?? false,
    local_only: c.local_only ?? false,
    bots: c.bots ?? null
  };
}

/**
 * An in-memory daemon: the typed client methods run unchanged against it, so
 * the UI and its tests exercise the real request paths.
 */
export class MockApiClient extends ApiClient {
  readonly servers: McpServer[];
  readonly providersDto: ProvidersDto = structuredClone(PROVIDERS);
  readonly detectedProvidersList: DetectedProviderDto[] = structuredClone(DETECTED_PROVIDERS);
  readonly bots = structuredClone(BOTS);
  readonly chats: Chat[] = structuredClone([...CHATS, ...BOTS.slice(1).map((b,i) => ({ id: `mock-chat-${i}`, chat_id: `mock-chat-${i}`, bot_id: b.id, title: previews[i+1], created_at_ms: NOW }))]);
  readonly messages = new Map<string, Message[]>();
  readonly secrets = new Map<string, number>();
  readonly emailAccounts: EmailAccount[] = [
    {
      name: "personal",
      provider: "imap_smtp",
      inbox_id: null,
      address: "alex@gmail.com",
      username: "",
      imap: { host: "imap.gmail.com", port: 993 },
      smtp: { host: "smtp.gmail.com", port: 465, security: "implicit" },
      drafts_folder: "Drafts",
      bot: null,
      allowed_senders: [],
      password_set: true,
      custom_ca: false
    },
    {
      name: "atlas-inbox",
      provider: "imap_smtp",
      inbox_id: null,
      address: "atlas@fastmail.example",
      username: "atlas",
      imap: { host: "imap.fastmail.example", port: 993 },
      smtp: { host: "smtp.fastmail.example", port: 587, security: "start_tls" },
      drafts_folder: "Drafts",
      bot: ATLAS,
      allowed_senders: ["alex@gmail.com"],
      password_set: true,
      custom_ca: false
    }
  ];

  constructor() {
    super({ base_url: MOCK_BASE_URL, token: "mock" });
    this.servers = [
      {
        ...serverFromConfig(
          {
            name: "github",
            transport: "stdio",
            command: "npx",
            args: ["-y", "@modelcontextprotocol/server-github"]
          },
          "developer"
        ),
        tool_count: 26,
        bots: null
      },
      {
        ...serverFromConfig(
          {
            name: "filesystem",
            transport: "stdio",
            command: "npx",
            args: ["-y", "@modelcontextprotocol/server-filesystem", "."]
          },
          "files"
        ),
        tool_count: 11,
        bots: [ATLAS]
      },
      serverFromConfig({ name: "gmail", transport: "http", url: "https://example.com/gmail" }, "communication"),
      serverFromConfig({ name: "google-calendar", transport: "http", url: "https://example.com/calendar", bots: [ATLAS, NOVA] }, "productivity"),
      serverFromConfig({ name: "slack", transport: "http", url: "https://example.com/slack", bots: [NOVA, "bot-milo"] }, "communication"),
      {
        ...serverFromConfig(
          { name: "linear", transport: "http", url: "https://mcp.linear.app/mcp" },
          "productivity"
        ),
        status: "error",
        error: "401 unauthorized: check the API key",
        tool_count: 0,
        bots: null
      }
    ];
  }

  protected override async request<T>(
    method: string,
    path: string,
    opts: { body?: unknown } = {}
  ): Promise<T> {
    await Promise.resolve();
    const ok = (v: unknown) => v as T;
    const seg = path.split("/").filter(Boolean).map(decodeURIComponent);
    switch (`${method} /${seg[0] ?? ""}`) {
      case "GET /health":
        return ok({ status: "ok", uptime_secs: 3600, version: "0.1.0-mock" });
      case "GET /bots":
        return ok(structuredClone(this.bots));
      case "GET /chats":
        if (seg[2] === "messages") {
          if (this.messages.has(seg[1])) return ok(this.messages.get(seg[1]));
          if ((typeof window !== "undefined" && new URLSearchParams(window.location.search).get("screen") === "empty") || seg[1] !== CHAT_ID) return ok([]);
          const tr = typeof window !== "undefined" && new URLSearchParams(window.location.search).get("lang") !== "en";
          const content = tr ? [
            "Bu haftaki çalışma özetini hazırlar mısın? Tamamlanan işleri ve sonraki adımları görmek istiyorum.",
            "## Bu hafta güzel ilerledik.\n\nGitHub ve çalışma klasöründeki son değişiklikleri inceledim. İşte haftanın kısa özeti:\n\n| Çalışma | Tamamlanan | Durum |\n| --- | --- | --- |\n| Masaüstü deneyimi | Ayarlar, sohbet ve model seçimi | Hazır |\n| Entegrasyonlar | 5 uygulama bağlantısı | Hazır |\n| Kalite kontrolleri | Tip denetimi ve arayüz testleri | Tamamlandı |\n\n### Sıradaki adım\n\nAçık görevleri gözden geçirdikten sonra sürüm kontrollerini çalıştırabiliriz.\n\n```sh\npnpm typecheck\npnpm --dir apps/desktop/ui test\n```\n\nİstersen bu özeti ekibinle paylaşabileceğin bir rapora dönüştürebilirim.",
          ] : [
            "Can you prepare this week's work summary? I'd like to see what shipped and what's next.",
            "## A good week of progress.\n\nI reviewed the latest changes in GitHub and your workspace. Here is your weekly summary:\n\n| Workstream | Delivered | Status |\n| --- | --- | --- |\n| Desktop experience | Settings, chat and model selection | Ready |\n| Integrations | 5 connected apps | Ready |\n| Quality checks | Types and interface tests | Complete |\n\n### Next up\n\nAfter reviewing open tasks, we can run the release checks.\n\n```sh\npnpm typecheck\npnpm --dir apps/desktop/ui test\n```\n\nI can turn this into a report to share with your team.",
          ];
          const seeded = content.map((text, index): Message => ({ id: `mock-message-${index}`, chat_id: CHAT_ID, content: text, role: index % 2 === 0 ? "user" : "assistant", created_at_ms: NOW + index * 1000 }));
          this.messages.set(seg[1], seeded);
          return ok(seeded);
        }
        return ok(structuredClone(this.chats));
      case "POST /chats": {
        const input = opts.body as { bot_id?: string; title?: string; text?: string };
        if (seg[2] === "messages") {
          const previous = await this.request<Message[]>("GET", `/chats/${seg[1]}/messages`);
          this.messages.set(seg[1], [...previous, { id: crypto.randomUUID(), chat_id: seg[1], role: "user", content: input.text ?? "", created_at_ms: Date.now() } as Message]);
          return ok({ delivery: "started", run_id: crypto.randomUUID() });
        }
        const id = crypto.randomUUID();
        const created = { id, chat_id: id, bot_id: input.bot_id ?? ATLAS, title: input.title ?? "Yeni sohbet", created_at_ms: Date.now() };
        this.chats.push(created);
        this.messages.set(id, []);
        return ok(created);
      }
      case "GET /schedules":
        return ok(typeof window !== "undefined" && new URLSearchParams(window.location.search).get("screen") === "empty" ? [] : structuredClone(SCHEDULES));
      case "GET /approvals":
        return ok(typeof window !== "undefined" && new URLSearchParams(window.location.search).get("screen") === "approval"
          ? [{ call_id: "mock-rm", run_id: "mock-live", requested_at_ms: Date.now() - 3000, decision: null, tool_name: "shell.run", summary: "drafts/old klasörünü ve içindeki 14 dosyayı sil" }]
          : typeof window !== "undefined" && new URLSearchParams(window.location.search).get("screen")?.startsWith("approval-schedule")
          ? [{ call_id: "mock-sch", run_id: "mock-live", requested_at_ms: Date.now() - 3000, decision: null, tool_name: "schedule_create", summary: "Create a schedule" }]
          : []);
      case "GET /runs":
        return ok([]);
      case "GET /usage":
        return ok(USAGE);
      case "GET /providers":
        if (seg[1] === "detected") {
          const screen =
            typeof window !== "undefined"
              ? new URLSearchParams(window.location.search).get("screen")
              : null;
          if (screen === "detected-empty") {
            return ok(
              this.detectedProvidersList.map((row) => ({
                ...row,
                installed: false,
                version: null,
                auth: "unknown",
                account_label: null,
                runnable: false,
                detail: "not_on_path",
                message: null
              }))
            );
          }
          return ok(structuredClone(this.detectedProvidersList));
        }
        return ok(structuredClone(this.providersDto));
      case "POST /providers": {
        if (seg[1] === "cli" && seg[2] === "enable") {
          const body = (opts.body ?? {}) as { id?: string };
          const id = body.id ?? "";
          if (id !== "claude") {
            throw new ApiError(
              400,
              "bad_request",
              id === "codex" || id === "chatgpt-plan"
                ? "The installed Codex cannot disable unified_exec, so Praxi will not run it."
                : "This coding CLI is detected only and cannot be run in this version of Praxi."
            );
          }
          const providerId = "cli:claude";
          const existing = this.providersDto.configured.find(
            (p) => p.id === providerId
          );
          const entry: ConfiguredProviderDto = {
            id: providerId,
            enabled: true,
            is_default: existing?.is_default ?? false,
            default_model: existing?.default_model ?? null,
            has_key: false,
            custom: false,
            active: true,
            models: existing?.models ?? [],
            models_error: null
          };
          if (existing) {
            Object.assign(existing, entry);
          } else {
            this.providersDto.configured.push(entry);
          }
          if (!this.providersDto.active.includes(providerId)) {
            this.providersDto.active.push(providerId);
          }
          if (!this.providersDto.catalog.some((c) => c.id === providerId)) {
            this.providersDto.catalog.push({
              id: providerId,
              kind: "cli",
              base_url: "",
              auth: "none",
              key_env: null,
              tools: true,
              vision: false,
              configured: true
            });
          }
          return ok(structuredClone(entry));
        }
        return ok({
          default_provider: this.providersDto.default_provider,
          active: [...this.providersDto.active]
        });
      }
      case "GET /policy":
        return ok({
          preset: "approve-destructive",
          categories: {}
        } satisfies PolicyDto);
      case "POST /secrets":
        this.secrets.set(seg[1] ?? "", Date.now());
        return ok(undefined);
      case "POST /bots": {
        const input = opts.body as { name: string; config: unknown };
        if (this.bots.some((b) => b.name.toLowerCase() === input.name.toLowerCase())) {
          throw new ApiError(409, "conflict", `a bot named "${input.name}" already exists`);
        }
        const created = { id: crypto.randomUUID(), name: input.name, config: input.config, created_at_ms: Date.now() } as Bot;
        this.bots.push(created);
        return ok(created);
      }
      case "PUT /bots":
        { const index = this.bots.findIndex(b => b.id === seg[1]);
          if (index >= 0) this.bots[index] = { ...this.bots[index], ...(opts.body as Partial<Bot>) };
          return ok(this.bots[index] ?? this.bots[0]); }
      case "GET /email":
      case "POST /email":
      case "PUT /email":
      case "DELETE /email":
        return ok(this.email(method, seg, opts.body));
      case "GET /mcp":
      case "POST /mcp":
      case "PUT /mcp":
      case "DELETE /mcp":
        return ok(this.mcp(method, seg, opts.body));
      default:
        throw new ApiError(404, "not_found", `mock: ${method} ${path}`);
    }
  }

  private email(method: string, seg: string[], body: unknown): unknown {
    const [, sub, name, action] = seg;
    if (sub === "test") {
      const draft = (body ?? {}) as EmailAccountInput;
      return draft.provider === "agentmail"
        ? { imap: "ok", smtp: "ok", ok: true, api: "ok" }
        : { imap: "ok", smtp: "ok", ok: true };
    }
    if (sub !== "accounts") {
      throw new ApiError(404, "not_found", "mock email");
    }
    if (!name) {
      if (method === "GET") {
        return this.emailAccounts;
      }
      const input = body as EmailAccountInput;
      if (this.emailAccounts.some((a) => a.name === input.name)) {
        throw new ApiError(409, "conflict", `account ${input.name} exists`);
      }
      if (input.provider === "agentmail") {
        // The "provider" creates the inbox; the key is write-only.
        const local = (input.name_hint ?? input.name)
          .toLowerCase()
          .replace(/[^a-z0-9]+/g, "-")
          .replace(/^-+|-+$/g, "");
        const address = `${local || "bot"}@agentmail.to`;
        const made: EmailAccount = {
          name: input.name,
          provider: "agentmail",
          inbox_id: address,
          address,
          username: "",
          imap: { host: "", port: 993 },
          smtp: { host: "", port: 465, security: "implicit" },
          drafts_folder: "Drafts",
          bot: input.bot ?? null,
          allowed_senders: input.allowed_senders ?? [],
          password_set: Boolean(input.api_key),
          custom_ca: false
        };
        this.emailAccounts.push(made);
        return made;
      }
      const imap = input.imap ?? { host: "" };
      const smtp = input.smtp ?? { host: "" };
      const created: EmailAccount = {
        name: input.name,
        provider: "imap_smtp",
        inbox_id: null,
        address: input.address ?? "",
        username: input.username ?? "",
        imap: { host: imap.host, port: imap.port ?? 993 },
        smtp: {
          host: smtp.host,
          port: smtp.port ?? 465,
          security: smtp.security ?? "implicit"
        },
        drafts_folder: input.drafts_folder ?? "Drafts",
        bot: input.bot ?? null,
        allowed_senders: input.allowed_senders ?? [],
        // The password is write-only: only the fact that one is stored is kept.
        password_set: Boolean(input.password),
        custom_ca: false
      };
      this.emailAccounts.push(created);
      return created;
    }
    const idx = this.emailAccounts.findIndex((a) => a.name === name);
    if (idx < 0) {
      throw new ApiError(404, "not_found", `no account ${name}`);
    }
    if (action === "test") {
      return this.emailAccounts[idx].provider === "agentmail"
        ? { imap: "ok", smtp: "ok", ok: true, api: "ok" }
        : { imap: "ok", smtp: "ok", ok: true };
    }
    if (method === "DELETE") {
      this.emailAccounts.splice(idx, 1);
      return undefined;
    }
    // PUT is a patch: a field left out keeps its value, `bot: null` = every bot.
    const patch = (body ?? {}) as Record<string, unknown>;
    const { password, api_key: apiKey, ...rest } = patch;
    const merged = {
      ...this.emailAccounts[idx],
      ...rest,
      password_set:
        this.emailAccounts[idx].password_set || Boolean(password || apiKey)
    } as EmailAccount;
    this.emailAccounts[idx] = merged;
    return merged;
  }

  private mcp(method: string, seg: string[], body: unknown): unknown {
    const [, sub, name, action] = seg;
    if (sub === "catalog") {
      return MOCK_MCP_CATALOG;
    }
    if (sub !== "servers") {
      throw new ApiError(404, "not_found", "mock mcp");
    }
    if (!name) {
      if (method === "GET") {
        return structuredClone(this.servers);
      }
      const cfg = body as McpServerConfig;
      if (this.servers.some((s) => s.name === cfg.name)) {
        throw new ApiError(409, "conflict", `server ${cfg.name} exists`);
      }
      const s = serverFromConfig(cfg);
      this.servers.push(s);
      return s;
    }
    const idx = this.servers.findIndex((s) => s.name === name);
    if (idx < 0) {
      throw new ApiError(404, "not_found", `no server ${name}`);
    }
    if (action === "tools") {
      return MCP_TOOLS;
    }
    if (action === "reconnect") {
      this.servers[idx] = { ...this.servers[idx], status: "connected", error: null };
      return this.servers[idx];
    }
    if (method === "DELETE") {
      this.servers.splice(idx, 1);
      return undefined;
    }
    // PUT is a patch: a field left out keeps its value, `bots: null` = all bots.
    const patch = (body ?? {}) as Record<string, unknown>;
    const prev = this.servers[idx];
    const merged: McpServer = {
      ...prev,
      ...(typeof patch.enabled === "boolean" ? { enabled: patch.enabled } : {}),
      ...(typeof patch.category === "string" ? { category: patch.category } : {}),
      ...(typeof patch.command === "string" ? { command: patch.command } : {}),
      ...(typeof patch.url === "string" ? { url: patch.url } : {}),
      ...("bots" in patch ? { bots: (patch.bots as string[] | null) ?? null } : {})
    };
    // Atomic id operations; an open server (null) stays open.
    if (merged.bots != null) {
      let list = [...merged.bots];
      for (const id of (patch.bots_add as string[] | undefined) ?? []) {
        if (!list.includes(id)) {
          list.push(id);
        }
      }
      const rm = (patch.bots_remove as string[] | undefined) ?? [];
      list = list.filter((id) => !rm.includes(id));
      merged.bots = list;
    }
    this.servers[idx] = merged;
    return merged;
  }
}

/** Review-state events for `?mock=1&screen=...` (approval, streaming, default run). */
export function mockReviewEvents(): { ts_ms: number; event: GlobalEnvelope["event"] }[] {
  const out: { ts_ms: number; event: GlobalEnvelope["event"] }[] = [];
  const screen = new URLSearchParams(window.location.search).get("screen");
  if (screen === "approval" || screen?.startsWith("approval-schedule") || screen === "streaming" || screen === "working") {
    const bot_id = "11111111-1111-4111-8111-111111111111";
    const chat_id = "33333333-3333-4333-8333-333333333333";
    const events: GlobalEnvelope["event"][] = [
      { type: "run_started", run_id: "mock-live", bot_id, chat_id },
      { type: "message_delta", run_id: "mock-live", text: screen === "approval" ? "Raporu hazırladım. Paylaşmadan önce eski taslakları temizlemem gerekiyor." : "Taslağı oluşturuyorum, önce kaynakları karşılaştırıyorum…" },
      ...(screen === "working" ? [
        { type: "tool_call_requested", run_id: "mock-live", call_id: "mock-w1", tool_name: "web_search", input: { query: "cpu llm benchmark 2026" } },
        { type: "tool_call_completed", run_id: "mock-live", call_id: "mock-w1", error: null },
        { type: "message_delta", run_id: "mock-live", text: "Kaynakları okuyup ölçümleri karşılaştırıyorum." },
        { type: "tool_call_requested", run_id: "mock-live", call_id: "mock-w2", tool_name: "web_extract", input: { url: "https://huggingface.co/blog/cpu-inference" } },
        { type: "tool_call_completed", run_id: "mock-live", call_id: "mock-w2", error: null },
        { type: "tool_call_requested", run_id: "mock-live", call_id: "mock-w3", tool_name: "github.search_issues", input: { query: "gguf arm" } }
      ] as GlobalEnvelope["event"][] : []),
      ...(screen?.startsWith("approval-schedule") ? [
        { type: "tool_call_requested", run_id: "mock-live", call_id: "mock-sch", tool_name: "schedule_create", input: {} },
        { type: "approval_needed", run_id: "mock-live", call_id: "mock-sch", tool_name: "schedule_create", input: {}, summary: "Create a schedule", details: { kind: "schedule", action: "create", name: "Pazartesi özeti", prompt: screen === "approval-schedule-long" ? LONG_PROMPT : "Geçen haftanın GitHub ve takvim hareketlerinden kısa bir özet hazırla ve bana gönder.", prompt_chars: screen === "approval-schedule-long" ? LONG_PROMPT.length : 76, previous: null, schedule: "Her pazartesi 09:00", timezone: "Europe/Istanbul", next_runs: ["2026-11-16T09:00:00+03:00", "2026-11-23T09:00:00+03:00"], permission: { level: "mixed", allow: ["github.*"], ask: ["email_send"], deny: [] } } }
      ] as GlobalEnvelope["event"][] : []),
      ...(screen === "approval" ? [
        { type: "tool_call_requested", run_id: "mock-live", call_id: "mock-rm", tool_name: "shell.run", input: { command: "rm -rf ./drafts/old" } },
        { type: "approval_needed", run_id: "mock-live", call_id: "mock-rm", tool_name: "shell.run", input: { command: "rm -rf ./drafts/old" }, summary: "drafts/old klasörünü ve içindeki 14 dosyayı sil" }
      ] as GlobalEnvelope["event"][] : [])
    ];
    events.forEach((event, i) => out.push({ ts_ms: Date.now() - 4000 + i * 300, event }));
  } else if (screen !== "empty") {
    const runId = "mock-review-run";
    const events: GlobalEnvelope["event"][] = [
      { type: "run_started", run_id: runId, bot_id: "11111111-1111-4111-8111-111111111111", chat_id: "33333333-3333-4333-8333-333333333333" },
      { type: "tool_call_requested", run_id: runId, call_id: "mock-tool", tool_name: "github.search", input: { query: "is:closed updated:this-week" } },
      { type: "tool_call_completed", run_id: runId, call_id: "mock-tool", error: null },
      { type: "run_finished", run_id: runId, error: null }
    ];
    events.forEach((event, i) => out.push({ ts_ms: 1750000000000 + (i === 3 ? 2000 : 200 + i * 100), event }));
  }
  return out;
}
