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
  "claude-code": { company: "anthropic", label: "Anthropic", note: "Claude Code CLI" },
  openai: { company: "openai", label: "OpenAI" },
  codex: { company: "openai", label: "OpenAI", note: "Codex CLI" },
  gemini: { company: "googlegemini", label: "Google" },
  "gemini-openai-compat": { company: "googlegemini", label: "Google" },
  "gemini-cli": { company: "googlegemini", label: "Google", note: "Gemini CLI" },
  ollama: { company: "ollama", label: "Ollama" }
};

function pretty(id: string): string {
  const bare = id.replace(/^cli:/, "");
  return bare ? bare.charAt(0).toUpperCase() + bare.slice(1) : id;
}

export function providerBrand(providerId: string): ProviderBrand {
  const known = PROVIDERS[providerId];
  if (known) return known;
  const bare = providerId.replace(/^cli:/, "");
  const base = PROVIDERS[bare];
  if (base) return { ...base, note: base.note ?? "CLI" };
  return {
    company: bare,
    label: pretty(providerId),
    ...(providerId.startsWith("cli:") ? { note: "CLI" } : {})
  };
}

/** The product a model belongs to; falls back to the company logo. */
export function productBrandKey(providerId: string, model: string): string {
  const m = model.toLowerCase();
  if (m.startsWith("claude")) return "claude";
  if (/^(gpt|chatgpt|codex)/.test(m) || /^o\d/.test(m)) return "openai";
  if (m.startsWith("gemini")) return "googlegemini";
  return providerBrand(providerId).company;
}
