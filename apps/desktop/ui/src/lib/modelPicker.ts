import type { CliEngineDto, ProvidersDto } from "../api/types.ts";
import { modelDisplayName } from "./modelName.ts";
import { isPlaceholderModel, isPlaceholderProvider } from "./placeholderModel.ts";
import { isCliProvider } from "./providerView.ts";
import { matchesQuery } from "./searchText.ts";

export interface ModelOption {
  /** `provider/model`, the form a bot profile stores. */
  ref: string;
  provider: string;
  model: string;
  displayName: string;
  recommended: boolean;
  recommendedRank: number | null;
  /** Catalog capability (`chat`, `embedding`, ...); null when the catalog does not know the model. */
  kind: string | null;
  deprecated: boolean;
  note: string | null;
}

export interface ModelGroup {
  provider: string;
  models: ModelOption[];
}

/**
 * Models offered by the picker, grouped by provider.  Only enabled providers
 * are listed. Legacy responses preserve configured defaults; a metadata-aware
 * response is authoritative about filtered defaults. The `current` ref remains
 * selectable even when unlisted. Enabled CLI providers expose a
 * single `default` model unless `includeCli` is `false`.  The default provider comes first.
 */
export function modelGroups(
  dto: ProvidersDto | null,
  current?: string | null,
  options?: { includeCli?: boolean }
): ModelGroup[] {
  const includeCli = options?.includeCli ?? true;
  const groups = new Map<string, Set<string>>();
  for (const p of dto?.configured ?? []) {
    if (!p.enabled || isPlaceholderProvider(p.id)) {
      continue;
    }
    const cli = isCliProvider(p.id, dto);
    if (cli && !includeCli) {
      continue;
    }
    const set = groups.get(p.id) ?? new Set<string>();
    if (p.default_model && !isPlaceholderModel(p.default_model) && (p.model_details === undefined
      || p.model_details.some((m) => m.id === p.default_model))) {
      set.add(p.default_model);
    }
    for (const m of p.models) {
      if (!isPlaceholderModel(m)) set.add(m);
    }
    if (cli && set.size === 0) {
      set.add("default");
    }
    groups.set(p.id, set);
  }
  const cur = current?.trim();
  if (cur && !isPlaceholderModel(cur)) {
    const i = cur.indexOf("/");
    const provider = i < 0 ? "" : cur.slice(0, i);
    const model = i < 0 ? cur : cur.slice(i + 1);
    if (provider && (includeCli || !isCliProvider(provider, dto))) {
      const set = groups.get(provider) ?? new Set<string>();
      set.add(model);
      groups.set(provider, set);
    }
  }
  const defaultProvider = dto?.default_provider ?? null;
  return [...groups.entries()]
    .sort(([a], [b]) =>
      a === defaultProvider ? -1 : b === defaultProvider ? 1 : a.localeCompare(b)
    )
    .map(([provider, models]) => ({
      provider,
      models: [...models].map((model) => {
        const metadata = dto?.configured.find((p) => p.id === provider)
          ?.model_details?.find((m) => m.id === model);
        return {
          ref: `${provider}/${model}`,
          provider,
          model,
          displayName: metadata?.display_name ?? modelDisplayName(model),
          recommended: metadata?.recommended ?? false,
          recommendedRank: metadata?.recommended_rank ?? null,
          kind: metadata?.kind ?? null,
          deprecated: metadata?.deprecated ?? false,
          note: metadata?.note ?? null
        };
      }).sort((a, b) =>
        (a.recommendedRank ?? Number.MAX_SAFE_INTEGER) - (b.recommendedRank ?? Number.MAX_SAFE_INTEGER)
        || a.displayName.localeCompare(b.displayName)
        || a.model.localeCompare(b.model)
      )
    }));
}

/** True when at least one real model can be chosen: the placeholder alone means "nothing connected yet". */
export function hasConnectedModel(dto: ProvidersDto | null): boolean {
  return modelGroups(dto).some((g) => g.models.length > 0);
}

/** Keep the options whose provider or model matches `query`; drop empty groups. */
export function filterModelGroups(
  groups: ModelGroup[],
  query: string
): ModelGroup[] {
  return groups
    .map((g) => ({
      provider: g.provider,
      models: g.models.filter((m) => matchesQuery(query, m.provider, m.model, m.displayName))
    }))
    .filter((g) => g.models.length > 0);
}

/** CLI models that mean "let the CLI choose"; the picker words them as one default row. */
export function isCliDefault(option: Pick<ModelOption, "provider" | "model">): boolean {
  return option.provider.startsWith("cli:") && (option.model === "default" || option.model === "auto");
}

const CHAT_KINDS = new Set(["chat", "reasoning", "vision_capable"]);
const NOISE_ID =
  /(preview|experimental|(^|[-/_])exp([-_:.]|$)|embed|(^|[-/_])tts([-_:.]|$)|whisper|imagen|dall-e|moderation|rerank|-image|image-|-audio|-realtime|-transcribe|(^|[-/])veo|-(19|20)\d{2}-\d{2}-\d{2}$|-(19|20)\d{6}$|-(0[1-9]|1[0-2])[0-3]\d$)/i;

/**
 * Noise rule ("hide by default", shown only under "Show all" or by search): a model is noise when
 * the catalog says it is not a chat model (`kind` other than chat/reasoning/vision_capable), is
 * `deprecated`, or carries a `preview`/`experimental` note; and, for every model (also those the
 * catalog does not know), when its id looks like a preview/experimental build, an embedding,
 * image, tts, audio or moderation model, or a dated snapshot (`-20250514`, `-2025-05-14`, `-0613`).
 * Recommended models, CLI models and the currently selected model are never noise.
 */
export function isNoiseModel(m: ModelOption, current?: string | null): boolean {
  if (m.recommended || m.ref === current || m.provider.startsWith("cli:")) {
    return false;
  }
  if (m.kind !== null && !CHAT_KINDS.has(m.kind)) {
    return true;
  }
  if (m.deprecated || /preview|experimental|(^|[^a-z])exp([^a-z]|$)/i.test(m.note ?? "")) {
    return true;
  }
  return NOISE_ID.test(m.model);
}

/** Aggregators resell hundreds of models of other makers; the picker never lists them flat. */
const AGGREGATORS = new Set(["openrouter", "together", "litellm", "huggingface", "fireworks", "groq-router"]);
export function isAggregator(provider: string, modelCount: number): boolean {
  return AGGREGATORS.has(provider) || (!provider.startsWith("cli:") && modelCount > 40);
}

export type ModelHint = "fast" | "balanced" | "strongest";
/** Second line under a recommended model: why it is there. */
export type RowHint = ModelHint | "new" | "free" | "quota";

export interface Recommendation {
  option: ModelOption;
  hint: RowHint;
}

export interface ParsedModel {
  /** Name words without version numbers: `claude-fable-5-2` -> `claude-fable`. */
  family: string;
  /** Numeric parts in order: `claude-fable-5-2` -> [5, 2]; `claude-3-5-sonnet` -> [3, 5]. */
  version: number[];
}

/** Family and version of a model id (vendor prefix, `:tag`, date stamp and `-latest` ignored). */
export function parseModel(model: string): ParsedModel {
  const leaf = model.slice(model.lastIndexOf("/") + 1).toLowerCase().replace(/:.*$/, "");
  const raw = leaf.replace(/-latest$/, "").replace(/-(19|20)\d{2}-?\d{2}-?\d{2}$/, "");
  const words: string[] = [];
  const version: number[] = [];
  for (const tok of raw.split(/[-_]/).filter(Boolean)) {
    const split = /^([a-z]+)(\d+(\.\d+)*)$/.exec(tok); // llama3.2, qwen3
    if (/^\d+(\.\d+)*$/.test(tok)) {
      version.push(...tok.split(".").map(Number));
    } else if (split) {
      words.push(split[1] ?? "");
      version.push(...(split[2] ?? "").split(".").map(Number));
    } else {
      words.push(tok);
    }
  }
  return { family: words.join("-") || leaf, version };
}

/** Model family for de-duplication across sources. */
export function modelFamily(model: string): string {
  return parseModel(model).family;
}

function compareVersion(a: number[], b: number[]): number {
  for (let i = 0; i < Math.max(a.length, b.length); i += 1) {
    const d = (a[i] ?? 0) - (b[i] ?? 0);
    if (d !== 0) return d;
  }
  return 0;
}

/** One-line hint for a recommended model, from its id (the catalog carries no hint text). */
export function modelHint(model: string): ModelHint {
  const id = model.toLowerCase();
  if (/(flash|haiku|lite|(^|[-/:])mini|nano|small|luna|:[1-8]b\b)/.test(id)) {
    return "fast";
  }
  if (/(opus|fable|-pro|carbon|sol\b|wonder|:(30|32|70)b)/.test(id)) {
    return "strongest";
  }
  return "balanced";
}

const TIER: Record<ModelHint, number> = { strongest: 2, balanced: 3, fast: 4 };

/** Tier order: the vendor's top line (fable, opus) above the strongest class, then balanced, then fast. */
function tierScore(model: string): number {
  const id = model.toLowerCase();
  return /fable/.test(id) ? 0 : /opus/.test(id) ? 1 : TIER[modelHint(model)];
}

/** Free by data: an OpenRouter-style `:free` id or a catalog note that says free. */
export function isFreeModel(m: Pick<ModelOption, "model" | "note">): boolean {
  return /:free$/i.test(m.model) || /(^|[^a-z])free([^a-z]|$)/i.test(m.note ?? "");
}

const usable = (m: ModelOption): boolean =>
  (m.kind === null || CHAT_KINDS.has(m.kind)) && !m.deprecated && !isNoiseModel(m, null);

/** Newest version of each family (one row per family), strongest tier first. */
function newestPerFamily(models: ModelOption[], preferFree = false): Array<{ m: ModelOption; older: boolean }> {
  const best = new Map<string, { m: ModelOption; v: number[]; count: number }>();
  for (const m of models) {
    const { family, version } = parseModel(m.model);
    const cur = best.get(family);
    if (!cur) {
      best.set(family, { m, v: version, count: 1 });
      continue;
    }
    cur.count += 1;
    const d = compareVersion(version, cur.v);
    if (d > 0 || (d === 0 && preferFree && isFreeModel(m) && !isFreeModel(cur.m))) {
      cur.m = m;
      cur.v = version;
    }
  }
  return [...best.entries()]
    .map(([family, e]) => ({ m: e.m, v: e.v, family, older: e.count > 1 }))
    .sort((a, b) =>
      (a.m.recommendedRank ?? 99) - (b.m.recommendedRank ?? 99)
      || tierScore(a.m.model) - tierScore(b.m.model)
      || compareVersion(b.v, a.v)
      || a.family.localeCompare(b.family)
    )
    .map(({ m, older }) => ({ m, older }));
}

/** Most models a source recommends. */
export const SOURCE_RECOMMENDED_MAX = 5;
const FREE_RECOMMENDED_MAX = 4;
const LOCAL_PROVIDERS = new Set(["ollama", "lmstudio"]);
const GEMINI_PROVIDERS = new Set(["gemini", "gemini-openai-compat"]);

/** (a) Vendor apps and APIs: the newest model of every family, strongest tier first. */
function ruleNewest(models: ModelOption[]): Recommendation[] {
  return newestPerFamily(models)
    .slice(0, SOURCE_RECOMMENDED_MAX)
    .map(({ m, older }) => ({ option: m, hint: older ? "new" : modelHint(m.model) }));
}

/**
 * (b) Aggregators and multi-vendor engines: free chat models first (newest per family, catalog rank
 * and tier decide the order); with no free model, rule (a).
 */
function ruleFreeFirst(models: ModelOption[]): Recommendation[] {
  const free = newestPerFamily(models.filter(isFreeModel), true)
    .slice(0, FREE_RECOMMENDED_MAX)
    .map(({ m }) => ({ option: m, hint: "free" as RowHint }));
  if (free.length >= 3) return free;
  const taken = new Set(free.map((r) => r.option.ref));
  const filler = ruleNewest(models.filter((m) => !taken.has(m.ref))).slice(0, 3 - free.length);
  return [...free, ...filler];
}

const geminiClass = (id: string): number =>
  /flash-lite|flash_lite/.test(id) ? 0 : /flash/.test(id) ? 1 : /(^|[-/])pro/.test(id) ? 3 : 2;

/**
 * (c) Gemini API (free tier): the high-quota class first - Flash-Lite, then Flash, other models,
 * Pro last (the catalog carries no per-model quota) - newest version first within a class.
 */
function ruleGemini(models: ModelOption[]): Recommendation[] {
  return newestPerFamily(models)
    .map(({ m }) => ({ m, cls: geminiClass(m.model.toLowerCase()), v: parseModel(m.model).version }))
    .sort((a, b) => a.cls - b.cls || compareVersion(b.v, a.v))
    .slice(0, SOURCE_RECOMMENDED_MAX)
    .map(({ m, cls }) => ({ option: m, hint: cls <= 1 ? "quota" : modelHint(m.model) }));
}

const sizeOf = (id: string): number => {
  const m = /(\d+(?:\.\d+)?)b\b/i.exec(id);
  return m ? Number(m[1]) : -1;
};

/** (d) Local providers: installed models, largest first. */
function ruleLocal(models: ModelOption[]): Recommendation[] {
  return [...models]
    .sort((a, b) => sizeOf(b.model) - sizeOf(a.model) || a.displayName.localeCompare(b.displayName))
    .slice(0, SOURCE_RECOMMENDED_MAX)
    .map((m) => ({ option: m, hint: modelHint(m.model) }));
}

/** Recommended models of one source (never the "CLI decides" row), computed from the data. */
export function sourceRecommendations(g: ModelGroup): Recommendation[] {
  const models = g.models.filter((m) => !isCliDefault(m));
  const pool = models.filter(usable);
  const list = pool.length > 0 ? pool : models;
  if (list.length === 0) return [];
  const cli = g.provider.startsWith("cli:");
  const multiVendor = cli && list.filter((m) => m.model.includes("/")).length * 2 >= list.length;
  if (LOCAL_PROVIDERS.has(g.provider)) return ruleLocal(list);
  if (GEMINI_PROVIDERS.has(g.provider)) return ruleGemini(list);
  if (multiVendor || (!cli && isAggregator(g.provider, g.models.length))) return ruleFreeFirst(list);
  return ruleNewest(list);
}

export interface SourceGroup {
  provider: string;
  cli: boolean;
  aggregator: boolean;
  /** Models of the source (without the "CLI decides" row). */
  total: number;
  /** Rows shown when the group is open (the selected model is pinned when not recommended). */
  recommended: Recommendation[];
  /** Everything else, behind "All models (N)"; noise last. */
  rest: ModelOption[];
}

export interface PickerSections {
  /** One group per connected coding CLI (subscription). */
  cli: SourceGroup[];
  /** One group per API provider (billed per use). */
  api: SourceGroup[];
  /** Coding CLIs that are switched on but have no model to offer (not installed or not signed in
   *  where this list comes from). Never rendered as an empty group; the picker words them as one
   *  muted "not on <server> yet" row or leaves them out. */
  emptyCli: string[];
}

/** Split provider groups into the CLI and API sections with their recommended models. */
export function pickerSections(groups: ModelGroup[], current?: string | null): PickerSections {
  const out: PickerSections = { cli: [], api: [], emptyCli: [] };
  for (const g of groups) {
    const cli = g.provider.startsWith("cli:");
    const models = g.models.filter((m) => !isCliDefault(m));
    // A CLI with no listed model is not a group: "Codex 0" tells the owner nothing. A group that
    // holds the current choice stays so the choice is visible.
    if (cli && models.length === 0 && !g.models.some((m) => m.ref === current)) {
      out.emptyCli.push(g.provider);
      continue;
    }
    const recommended = sourceRecommendations(g);
    const cur = models.find((m) => m.ref === current);
    if (cur && !recommended.some((r) => r.option.ref === cur.ref)) {
      recommended.unshift({ option: cur, hint: modelHint(cur.model) });
    }
    const shown = new Set(recommended.map((r) => r.option.ref));
    const rest = models
      .filter((m) => !shown.has(m.ref))
      .sort((a, b) => Number(isNoiseModel(a, current)) - Number(isNoiseModel(b, current)));
    (cli ? out.cli : out.api).push({
      provider: g.provider,
      cli,
      aggregator: isAggregator(g.provider, g.models.length),
      total: models.length,
      recommended,
      rest
    });
  }
  return out;
}

/** Most models in the top "Recommended" strip. */
export const RECOMMENDED_MAX = 4;

/**
 * The top strip: the first recommendation of up to four different sources, CLI / subscription
 * sources first, then direct providers, aggregators last.  A model whose family the strip already
 * has is skipped in favour of the source's next recommendation.
 */
export function recommendedOptions(groups: ModelGroup[], max = RECOMMENDED_MAX): Recommendation[] {
  const rank = (g: ModelGroup) =>
    g.provider.startsWith("cli:") ? 0 : isAggregator(g.provider, g.models.length) ? 2 : 1;
  const ordered = groups.map((g, i) => ({ g, i })).sort((a, b) => rank(a.g) - rank(b.g) || a.i - b.i);
  const picked: Recommendation[] = [];
  const families = new Set<string>();
  for (const { g } of ordered) {
    if (picked.length >= max) break;
    const rec = sourceRecommendations(g).find((r) => !families.has(parseModel(r.option.model).family));
    if (rec) {
      families.add(parseModel(rec.option.model).family);
      picked.push(rec);
    }
  }
  return picked;
}

export interface EngineRow {
  id: string;
  name: string;
  /** `connect`: installed, not switched on yet. `install`: not on this computer. */
  action: "connect" | "install";
}

/**
 * Coding CLIs the picker offers below the model list: installed ones that are not enabled yet,
 * then supported ones that are not installed (they open the install flow).
 */
export function engineRows(engines: CliEngineDto[], enabledCliIds: ReadonlySet<string>): EngineRow[] {
  const rows: EngineRow[] = [];
  for (const e of engines) {
    const d = e.detected;
    if (enabledCliIds.has(d.id)) {
      continue;
    }
    if (d.installed) {
      // Only engines Praxi can run (confined, or with the CLI's own tools) can be connected.
      if (!d.runnable && !e.native) {
        continue;
      }
      rows.push({ id: d.id, name: d.display_name, action: "connect" });
    } else if (e.install) {
      rows.push({ id: d.id, name: d.display_name, action: "install" });
    }
  }
  return rows.sort((a, b) => (a.action === b.action ? 0 : a.action === "connect" ? -1 : 1));
}

/** What a model is good at, in one line under its name (every row, not only the recommended ones). */
export type ModelUse = "hard" | "everyday" | "quick" | "code" | "free" | "quota" | "local";

const CODE_MODEL = /(codex|coder|codestral|devstral|(^|[-/_])code([-/_.:]|$))/i;

/**
 * The one-line "good at" of a model, from its source and id (the catalog carries no such text):
 * free and high-quota recommendations say so, a model on this machine (Ollama, LM Studio) says it
 * runs locally, a coding model says code, everything else follows its tier (hard / everyday / quick).
 */
export function modelUse(m: Pick<ModelOption, "provider" | "model" | "note">, hint?: RowHint): ModelUse {
  if (hint === "free" || isFreeModel(m)) return "free";
  if (hint === "quota") return "quota";
  if (LOCAL_PROVIDERS.has(m.provider)) return "local";
  if (CODE_MODEL.test(m.model)) return "code";
  const tier = hint === "fast" || hint === "balanced" || hint === "strongest" ? hint : modelHint(m.model);
  return tier === "strongest" ? "hard" : tier === "fast" ? "quick" : "everyday";
}
