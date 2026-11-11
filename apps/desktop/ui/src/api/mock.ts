import { ApiClient, ApiError } from "./client.ts";
import { MOCK_MCP_CATALOG } from "./mockCatalog.ts";
import type {
  Bot,
  Chat,
  McpServer,
  McpServerConfig,
  McpTool,
  PolicyDto,
  ProvidersDto,
  UsageSummary
} from "./types.ts";

/** `base_url` of the connection the mock mode hands out. */
export const MOCK_BASE_URL = "mock://praxi";

/**
 * Mock mode: design review, screenshots and demos without a daemon.  Opt-in
 * with `?mock=1` in dev builds or `VITE_MOCK=1` at build time.
 */
export function isMockMode(): boolean {
  if (typeof window === "undefined") {
    return false;
  }
  // Use direct `import.meta.env` access so Vite can replace `VITE_MOCK` at build time.
  if (import.meta.env.VITE_MOCK === "1") {
    return true;
  }
  return (
    import.meta.env.DEV &&
    new URLSearchParams(window.location.search).get("mock") === "1"
  );
}

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
  bot(ATLAS, "Atlas", "#3b82f6", "anthropic/claude-sonnet-5-5"),
  bot(NOVA, "Nova", "#10b981", "gemini/gemini-2.5-flash")
];

const CHAT_ID = "33333333-3333-4333-8333-333333333333";
const CHATS: Chat[] = [
  {
    id: CHAT_ID,
    chat_id: CHAT_ID,
    bot_id: ATLAS,
    title: "Weekly report",
    created_at_ms: NOW
  }
];

const PROVIDERS: ProvidersDto = {
  active: ["anthropic", "gemini", "ollama"],
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
    bots: c.bots ?? null
  };
}

/**
 * An in-memory daemon: the typed client methods run unchanged against it, so
 * the UI and its tests exercise the real request paths.
 */
export class MockApiClient extends ApiClient {
  readonly servers: McpServer[];
  readonly secrets = new Map<string, number>();

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
        return ok(BOTS);
      case "GET /chats":
        if (seg[2] === "messages") {
          const tr = typeof window !== "undefined" && new URLSearchParams(window.location.search).get("lang") !== "en";
          const content = tr ? [
            "Bu haftaki çalışma özetini hazırlar mısın? Tamamlanan işleri ve sonraki adımları görmek istiyorum.",
            "## Haftalık çalışma özeti\n\nGitHub ve çalışma klasöründeki son değişiklikleri inceledim. Bu hafta üç başlık öne çıkıyor.\n\n- **Masaüstü deneyimi:** Ayarlar, uygulamalar ve model seçimi tek bir akışta toplandı.\n- **Entegrasyonlar:** GitHub ve dosya sistemi bağlantıları kullanıma hazır.\n- **Kalite:** Tip denetimi ve arayüz testleri tamamlandı.\n\n### Sonraki adımlar\n\nÖnce açık görevleri gözden geçirmeni, ardından sürüm kontrollerini çalıştırmanı öneriyorum:\n\n```sh\npnpm typecheck\npnpm --dir apps/desktop/ui test\n```\n\nİstersen bu özeti paylaşılabilir bir rapora dönüştürebilirim.",
          ] : [
            "Can you prepare this week's work summary? I'd like to see what shipped and what's next.",
            "## Your weekly work summary\n\nI reviewed the latest changes in GitHub and your workspace. Three areas stood out this week.\n\n- **Desktop experience:** Settings, apps and model selection now share one flow.\n- **Integrations:** GitHub and filesystem connections are ready.\n- **Quality:** Type checks and UI tests are complete.\n\n### Next steps\n\nReview open tasks, then run the release checks:\n\n```sh\npnpm typecheck\npnpm --dir apps/desktop/ui test\n```\n\nI can turn this summary into a report for your team.",
          ];
          return ok(content.map((text, index) => ({ id: `mock-message-${index}`, chat_id: CHAT_ID, content: text, role: index % 2 === 0 ? "user" : "assistant", created_at_ms: NOW + index * 1000 })));
        }
        return ok(CHATS);
      case "GET /runs":
      case "GET /approvals":
      case "GET /schedules":
        return ok([]);
      case "GET /usage":
        return ok(USAGE);
      case "GET /providers":
        return ok(PROVIDERS);
      case "POST /providers":
        return ok({ default_provider: "anthropic", active: PROVIDERS.active });
      case "GET /policy":
        return ok({
          preset: "approve-destructive",
          categories: {}
        } satisfies PolicyDto);
      case "POST /secrets":
        this.secrets.set(seg[1] ?? "", Date.now());
        return ok(undefined);
      case "PUT /bots":
        return ok(BOTS.find((b) => b.id === seg[1]) ?? BOTS[0]);
      case "GET /mcp":
      case "POST /mcp":
      case "PUT /mcp":
      case "DELETE /mcp":
        return ok(this.mcp(method, seg, opts.body));
      default:
        throw new ApiError(404, "not_found", `mock: ${method} ${path}`);
    }
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
        return this.servers;
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
