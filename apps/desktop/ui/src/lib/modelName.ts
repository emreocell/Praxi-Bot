import { providerBrand } from "./modelBrand.ts";
import { isPlaceholderModel } from "./placeholderModel.ts";

/**
 * Human names for model ids (owner feedback 244, points 3 and 4): the UI shows
 * "Gemini 3.5 Flash Lite", never `gemini-3.5-flash-lite`.  The daemon reports ids only, so the
 * name is derived; an id that follows no known convention still comes out readable.
 */

const WORDS: Record<string, string> = {
  gpt: "GPT",
  chatgpt: "ChatGPT",
  claude: "Claude",
  gemini: "Gemini",
  gemma: "Gemma",
  llama: "Llama",
  codellama: "CodeLlama",
  mistral: "Mistral",
  mixtral: "Mixtral",
  deepseek: "DeepSeek",
  qwen: "Qwen",
  phi: "Phi",
  grok: "Grok",
  tts: "TTS",
  hd: "HD",
  oss: "OSS",
  exp: "Experimental"
};

const VERSION = /^\d+(\.\d+)*$/;
const SMALL_VERSION = /^\d{1,2}$/;
/** `20250929`, `2025-09-29` (already split into tokens: 2025, 09, 29) and `-latest`. */
const DATE_8 = /^(19|20)\d{6}$/;

function titleWord(token: string): string {
  const known = WORDS[token.toLowerCase()];
  if (known) return known;
  // `70b`, `32k`, `4o`, `o3`: size and series tags keep their shape, sizes go upper-case.
  if (/^\d+(\.\d+)?[bkm]$/i.test(token)) return token.toUpperCase();
  if (/\d/.test(token)) return token;
  return token.charAt(0).toUpperCase() + token.slice(1);
}

/** Drop a trailing date stamp (`-20250929`, `-2025-09-29`) and `-latest`. */
function withoutStamp(tokens: string[]): string[] {
  const out = [...tokens];
  if (out[out.length - 1]?.toLowerCase() === "latest" && out.length > 1) out.pop();
  if (DATE_8.test(out[out.length - 1] ?? "") && out.length > 1) out.pop();
  const n = out.length;
  if (
    n > 3 &&
    /^(19|20)\d{2}$/.test(out[n - 3] ?? "") &&
    /^\d{2}$/.test(out[n - 2] ?? "") &&
    /^\d{2}$/.test(out[n - 1] ?? "")
  ) {
    out.splice(n - 3, 3);
  }
  return out;
}

/** `claude-sonnet-5-5` -> `Claude Sonnet 5.5`; `gpt-5.1-mini` -> `GPT-5.1 Mini`. */
export function modelDisplayName(modelId: string): string {
  const raw = modelId.trim();
  if (!raw) return "";
  // `org/model` ids (OpenRouter, Hugging Face) are named after the model part.
  const leaf = raw.slice(raw.lastIndexOf("/") + 1);
  // Ollama tags: `llama3.2:3b`.
  const colon = leaf.indexOf(":");
  const base = colon < 0 ? leaf : leaf.slice(0, colon);
  const tag = colon < 0 ? "" : leaf.slice(colon + 1);
  const tokens = withoutStamp(base.split(/[-_\s]+/).filter(Boolean));
  if (tokens.length === 0) return raw;

  // Join split versions: `5`, `5` -> `5.5` (only short numbers, so `4-1106` stays apart).
  const joined: string[] = [];
  for (const tok of tokens) {
    const prev = joined[joined.length - 1];
    if (prev !== undefined && SMALL_VERSION.test(prev) && SMALL_VERSION.test(tok)) {
      joined[joined.length - 1] = `${prev}.${tok}`;
    } else {
      joined.push(tok);
    }
  }

  const words: string[] = [];
  for (let i = 0; i < joined.length; i += 1) {
    const tok = joined[i] ?? "";
    const word = titleWord(tok);
    const prev = words[words.length - 1];
    // OpenAI writes the version with a hyphen: GPT-5.1, GPT-4o.
    if (i === 1 && prev === "GPT" && /^\d/.test(tok)) {
      words[words.length - 1] = `GPT-${word}`;
    } else {
      words.push(word);
    }
  }
  // `llama3.2` -> `Llama 3.2`
  const spaced = words.map((w) => {
    const m = /^([a-z]+)(\d+(\.\d+)*)$/i.exec(w);
    if (m && WORDS[(m[1] ?? "").toLowerCase()] && VERSION.test(m[2] ?? "")) {
      return `${WORDS[(m[1] ?? "").toLowerCase()]} ${m[2]}`;
    }
    return w;
  });
  const name = spaced.join(" ");
  return tag ? `${name} (${tag.toUpperCase()})` : name;
}

export interface ModelLine {
  /** Human model name; empty when the ref carries no model. */
  name: string;
  /** Company / provider label ("Google", "Anthropic"); empty without a provider. */
  provider: string;
  /** `brandIcon` key of the provider's company logo. */
  brand: string;
  /** True for the CLI providers' single `default` model (named after the CLI). */
  isDefault: boolean;
}

/** What the right panel and the profile show for a stored `provider/model` ref. */
export function modelLine(ref: string | null | undefined): ModelLine {
  const text = (ref ?? "").trim();
  if (!text || isPlaceholderModel(text)) return { name: "", provider: "", brand: "", isDefault: false };
  const i = text.indexOf("/");
  const provider = i < 0 ? "" : text.slice(0, i);
  const model = i < 0 ? text : text.slice(i + 1);
  const brand = provider ? providerBrand(provider) : null;
  const providerLabel = brand ? brand.label : "";
  if (model === "default" && brand) {
    // A CLI account picks its own model: name the line after the CLI.
    return {
      name: brand.note ?? brand.label,
      provider: providerLabel,
      brand: brand.company,
      isDefault: true
    };
  }
  return {
    name: modelDisplayName(model),
    provider: providerLabel,
    brand: brand?.company ?? "",
    isDefault: false
  };
}
