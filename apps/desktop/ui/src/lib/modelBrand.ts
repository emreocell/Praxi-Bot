/**
 * Brand marks for the model picker: the company logo heads a provider group, the product
 * logo (Claude, ChatGPT, Gemini ...) sits on each model row.  Keys feed `brandIcon`.
 */

export interface ProviderBrand {
  /** `brandIcon` key of the company logo. */
  company: string;
  /** Company name shown on the group header. */
  label: string;
  /** Short qualifier after the name ("CLI"), if any. */
  note?: string;
}

const PROVIDERS: Record<string, ProviderBrand> = {
  anthropic: { company: "anthropic", label: "Anthropic" },
  "claude-code": { company: "claude", label: "Anthropic", note: "Claude Code CLI" },
  openai: { company: "openai", label: "OpenAI" },
  codex: { company: "openai", label: "OpenAI", note: "Codex CLI" },
  gemini: { company: "googlegemini", label: "Google" },
  "gemini-openai-compat": { company: "googlegemini", label: "Google" },
  "gemini-cli": { company: "googlegemini", label: "Google", note: "Gemini CLI" },
  ollama: { company: "ollama", label: "Ollama" }
};

const CLI_LABELS: Record<string, string> = {
  claude: "Claude Code",
  codex: "Codex",
  cursor: "Cursor",
  opencode: "OpenCode",
  gemini: "Gemini CLI",
  agy: "Antigravity",
  qwen: "Qwen Code",
  kiro: "Kiro",
  copilot: "GitHub Copilot",
  kimi: "Kimi Code",
  qoder: "Qoder"
};

function pretty(id: string): string {
  const bare = id.replace(/^cli:/, "");
  return bare ? bare.charAt(0).toUpperCase() + bare.slice(1) : id;
}

export function providerBrand(providerId: string): ProviderBrand {
  const known = PROVIDERS[providerId];
  if (known) return known;
  const bare = providerId.replace(/^cli:/, "");
  const cliLabel = providerId.startsWith("cli:") ? CLI_LABELS[bare] : undefined;
  if (cliLabel) {
    // Claude Code wears the Claude mark; the Anthropic API (`anthropic`) keeps the Anthropic "A".
    const company = { claude: "claude", codex: "openai", gemini: "googlegemini", agy: "antigravity", copilot: "githubcopilot" }[bare] ?? bare;
    return { company, label: cliLabel, note: "CLI" };
  }
  const base = PROVIDERS[bare];
  if (base) return { ...base, note: base.note ?? "CLI" };
  return {
    company: bare,
    label: pretty(providerId),
    ...(providerId.startsWith("cli:") ? { note: "CLI" } : {})
  };
}

const MAKER_PREFIX: Record<string, string> = {
  anthropic: "anthropic",
  openai: "openai",
  google: "googlegemini",
  deepseek: "deepseek",
  mistralai: "mistralai",
  mistral: "mistralai"
};

/** The model's maker (what the row icon shows), from the id; falls back to the provider's company. */
export function productBrandKey(providerId: string, model: string): string {
  const slash = model.lastIndexOf("/");
  const m = model.slice(slash + 1).toLowerCase();
  if (m.startsWith("claude")) return "claude";
  // Claude Code's own aliases (default, fable, opus, sonnet, haiku) are Claude models.
  if (/^(claude-code|cli:claude)$/.test(providerId) && slash < 0) return "claude";
  if (/^(gpt|chatgpt|codex)/.test(m) || /^o\d/.test(m)) return "openai";
  if (m.startsWith("gemini")) return "googlegemini";
  if (slash > 0) {
    const maker = MAKER_PREFIX[model.slice(0, model.indexOf("/")).toLowerCase()];
    if (maker) return maker;
  }
  return providerBrand(providerId).company;
}

const API_LABELS: Record<string, string> = {
  anthropic: "Anthropic API",
  openai: "OpenAI API",
  gemini: "Google API",
  "gemini-openai-compat": "Google API",
  openrouter: "OpenRouter",
  ollama: "Ollama (local)",
  lmstudio: "LM Studio (local)",
  deepseek: "DeepSeek API"
};

/** Where a model runs and who bills it: "Google API", "OpenRouter", "Claude Code CLI". */
export function sourceLabel(providerId: string): string {
  if (providerId.startsWith("cli:")) {
    const bare = providerId.slice(4);
    return `${CLI_LABELS[bare] ?? pretty(providerId)} CLI`;
  }
  return API_LABELS[providerId] ?? pretty(providerId);
}
