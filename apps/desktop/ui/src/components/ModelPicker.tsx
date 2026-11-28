import { Check, ChevronDown, ChevronRight, Download, Plug, RefreshCw, Search, Sparkles } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { isMockMode } from "../api/mockMode";
import {
  filterModelGroups,
  isCliDefault,
  modelUse,
  pickerSections,
  recommendedOptions,
  type ModelGroup,
  type ModelOption,
  type RowHint,
  type SourceGroup
} from "../lib/modelPicker";
import type { EngineBlock } from "../lib/engineConsent";
import { productBrandKey, providerBrand, sourceLabel } from "../lib/modelBrand";
import { modelDisplayName } from "../lib/modelName";
import { loadRecentModels, recentOptions, saveRecentModels, withRecent } from "../lib/recentModels";
import { matchesQuery } from "../lib/searchText";
import { THINKING_LEVELS, useThinkingLevel } from "../lib/thinkingLevel";
import { openSettingsTarget } from "../lib/settingsLink";
import { useDaemon } from "../state/DaemonContext";
import { useCliEngineRows } from "../state/useCliEngineRows";
import { useProviders } from "../state/useProviders";
import { BrandMark } from "./BrandMark";
import styles from "./ModelPicker.module.css";
import { Popover, type PopoverPlacement } from "./Popover";
import { useToast } from "./Toast";
import { humanLine } from "../lib/humanError";
import { isPlaceholderModel } from "../lib/placeholderModel";

export interface ModelPickerAutoOption {
  label: string;
  sub?: string;
  selected: boolean;
  onPick: () => void;
}

/** CLIs enabled but not ready on the server: one short line and a link to Settings. */
function MissingEngineRows({ ids, server, testId }: { ids: string[]; server: string; testId: string }) {
  const { t } = useTranslation();
  return (
    <>
      {ids.map((id) => {
        const engineId = id.replace(/^cli:/, "");
        return (
          <div key={`missing-${id}`} className={styles.missingCliRow} data-testid={`${testId}-missing-${id}`}>
            <span className={styles.noModels}>
              {t("modelPicker.missingCliShort", { name: sourceLabel(id), server: server || t("computerPicker.host") })}
            </span>
            <button
              type="button"
              data-picker-row
              className={styles.setupLink}
              onClick={() =>
                openSettingsTarget({ page: "models", card: "detectedAccounts", field: `cli-${engineId}` })
              }
            >
              {t("modelPicker.setUpCli")}
            </button>
          </div>
        );
      })}
    </>
  );
}

/**
 * Only while the bots' home is a server: its missing CLIs as muted rows, and the note that the
 * subscription CLIs of this computer are used when a bot runs on "this computer". Nothing otherwise.
 */
function HostCliExtras({ ids, testId, label }: { ids: string[]; testId: string; label?: ReactNode }) {
  const { t } = useTranslation();
  const { home, workLocalNow } = useDaemon();
  if (home.kind !== "host" || workLocalNow) return null;
  return (
    <>
      {label}
      {ids.length > 0 ? <MissingEngineRows ids={ids} server={home.name} testId={testId} /> : null}
      <p className={styles.noModels}>{t("modelPicker.noteLocalClis")}</p>
    </>
  );
}

/** "Connect" / "Install" rows for coding CLIs; mounted only where CLI models can be chosen. */
function EngineSection({
  query,
  testId,
  onInstall,
  onConnected
}: {
  query: string;
  testId: string;
  onInstall: () => void;
  onConnected: () => void;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const cli = useCliEngineRows(true);
  const typed = query.trim();
  const rows = cli.rows.filter((r) => !typed || matchesQuery(query, r.name, r.id));
  if (rows.length === 0) {
    return null;
  }
  const run = async (row: (typeof rows)[number]) => {
    if (row.action === "install") {
      onInstall();
    }
    try {
      await cli.run(row);
      if (row.action === "connect") {
        // The provider list reloads for every subscriber; show the new engine's models now.
        onConnected();
      }
    } catch (e) {
      toast.show(humanLine(e), "danger");
    }
  };
  return (
    <div role="group" aria-label={t("modelPicker.addEngines")} className={styles.group}>
      <div className={styles.groupLabel} aria-hidden="true">
        <span>{t("modelPicker.addEngines")}</span>
      </div>
      {rows.map((r) => (
        <button
          key={`engine-${r.id}`}
          type="button"
          data-picker-row
          className={styles.option}
          data-testid={`${testId}-${r.action}-${r.id}`}
          onClick={() => void run(r)}
        >
          {r.action === "install" ? <Download aria-hidden="true" /> : <Plug aria-hidden="true" />}
          <span className={styles.optionText}>
            {t(r.action === "install" ? "modelPicker.install" : "modelPicker.connect", { name: r.name })}
          </span>
        </button>
      ))}
    </div>
  );
}

/**
 * Model popover used by the composer, the bot editor and settings.  Layout: an optional
 * "automatic" row, up to four recommended models, a "Coding CLIs" section (one collapsible group
 * per connected engine) and an "API providers" section (one collapsible group per provider; an
 * aggregator lists a few curated models and "Show all N").  Every row names its source on the
 * right; the icon is the model's maker.  Search covers everything and groups hits by source.
 * Last come rows that connect or install coding CLIs.
 * `value` and the picked option use the `provider/model` form.
 */
/**
 * Under an open coding-CLI group: where its list came from and a Refresh button. The list is the
 * CLI's own (kept on disk, refreshed in the background); a built-in fallback is marked as such.
 */
/** Loads which CLI engines cannot answer right now (renders nothing); mounted only while the picker is open. */
function EngineBlocksProbe({ onBlocks }: { onBlocks: (blocks: Record<string, EngineBlock>) => void }) {
  const { blocks } = useCliEngineRows(true);
  const key = JSON.stringify(blocks);
  useEffect(() => {
    onBlocks(blocks);
    // `key` is the content of `blocks`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, onBlocks]);
  return null;
}

/** Why a CLI model row is disabled, in words (the engine is installed, enabled and still cannot answer). */
function blockReason(t: (key: string) => string, reason: string): string {
  switch (reason) {
    case "not_installed": return t("modelPicker.blocked.not_installed");
    case "outdated": return t("modelPicker.blocked.outdated");
    case "sign_in": return t("modelPicker.blocked.sign_in");
    default: return t("modelPicker.blocked.unsupported");
  }
}

function CliListNote({ provider, testId }: { provider: string; testId: string }) {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const { dto, reload } = useProviders();
  const [busy, setBusy] = useState(false);
  const row = dto?.configured.find((p) => p.id === provider);
  const source = row?.models_source ?? null;
  const fetched = row?.models_fetched_at_ms ?? null;
  // The daemon refreshes a stale list in the background when the picker reads it: look again shortly.
  useEffect(() => {
    if (source === null) return;
    const timer = window.setTimeout(() => void reload(), 6000);
    return () => window.clearTimeout(timer);
  }, [source, reload]);
  if (source === null) return null;
  const hours = fetched ? Math.max(0, Math.round((Date.now() - fetched) / 3_600_000)) : null;
  const text = source === "bundled"
    ? t("modelPicker.listBundled")
    : hours === null || hours < 1
      ? t("modelPicker.listFresh")
      : t("modelPicker.listAge", { hours });
  const refresh = async () => {
    if (!client || busy) return;
    setBusy(true);
    try {
      await client.refreshCliModels(provider.replace(/^cli:/, ""));
      await reload();
    } catch {
      // The list stays as it was; the note keeps saying where it came from.
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className={styles.listNote} data-source={source}>
      <span>{text}</span>
      <button
        type="button"
        data-picker-row
        className={styles.refresh}
        disabled={busy}
        aria-label={t("modelPicker.refreshList")}
        title={t("modelPicker.refreshList")}
        data-testid={`${testId}-refresh-${provider}`}
        onClick={() => void refresh()}
      >
        <RefreshCw aria-hidden="true" className={busy ? styles.spin : undefined} />
        {t("modelPicker.refresh")}
      </button>
    </div>
  );
}

export function ModelPicker({
  groups,
  value: rawValue,
  onPick,
  autoOption,
  disabled = false,
  placeholder,
  reviewOpen = true,
  thinking = false,
  placement = "bottom-end",
  testId = "model-picker",
  engines = false,
  cliNote,
  changeLink,
  recents = false
}: {
  groups: ModelGroup[];
  value: string;
  onPick: (ref: string) => void;
  autoOption?: ModelPickerAutoOption;
  disabled?: boolean;
  placeholder?: string;
  reviewOpen?: boolean;
  /** Show the thinking-level design preview in mock mode; the daemon cannot apply it yet. */
  thinking?: boolean;
  placement?: PopoverPlacement;
  testId?: string;
  /** Offer "Connect"/"Install" rows for coding CLIs (off where CLI models cannot be chosen). */
  engines?: boolean;
  /** Where CLI models cannot be chosen (planner, classifier, fallback): a greyed CLI section with this one line why. */
  cliNote?: string;
  /** Render the trigger as a quiet text link with this label (first run: "Change") instead of the model chip. */
  changeLink?: string;
  /** List the models picked last on top ("Recent") and remember each pick (the composer). */
  recents?: boolean;
}) {
  const { t } = useTranslation();
  // The internal validation-only model is never shown as a choice.
  const value = isPlaceholderModel(rawValue) ? "" : rawValue;
  const noModel = !groups.some((g) => g.models.length > 0);
  const valueRef = useMemo(() => {
    const [provider = "", ...rest] = value.split("/");
    return { provider, model: rest.join("/") };
  }, [value]);
  const [open, setOpen] = useState(
    () => reviewOpen && isMockMode() && new URLSearchParams(window.location.search).get("screen") === "models"
  );
  const mockParam = (name: string) => (isMockMode() ? new URLSearchParams(window.location.search).get(name) : null);
  const [query, setQuery] = useState(() => mockParam("modelq") ?? "");
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(
    () => new Set([valueRef.provider, ...(mockParam("expand")?.split(",") ?? [])])
  );
  const [full, setFull] = useState<ReadonlySet<string>>(() => new Set(mockParam("showall")?.split(",") ?? []));
  const [level, setLevel] = useThinkingLevel();
  const trigger = useRef<HTMLButtonElement | null>(null);
  // Engines that cannot answer right now (not installed, outdated, signed out): their rows are disabled with the reason.
  const [blocks, setBlocks] = useState<Record<string, EngineBlock>>({});
  const hasCli = groups.some((g) => g.provider.startsWith("cli:"));
  const blockOf = (provider: string) => (provider.startsWith("cli:") ? blocks[provider.slice(4)] : undefined);
  // Search results: every model, grouped by source, CLI engines first.
  const shown = useMemo(() => {
    const found = filterModelGroups(
      groups.map((g) => ({ ...g, models: g.models.filter((m) => !isCliDefault(m)) })),
      query
    );
    return [...found.filter((g) => g.provider.startsWith("cli:")), ...found.filter((g) => !g.provider.startsWith("cli:"))];
  }, [groups, query]);

  // "Other model id": a CLI accepts models its short list does not show.
  const typed = query.trim();
  const otherIds = shown.length === 0 && /^[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}$/.test(typed)
    ? groups.filter((g) => g.provider.startsWith("cli:") && !g.models.some((m) => m.model === typed))
    : [];

  const searching = typed !== "";
  // The models picked last, newest first; read again each time the picker opens (another chat may have picked).
  const [recentRefs, setRecentRefs] = useState<string[]>([]);
  const recent = useMemo(
    () => (recents && !searching ? recentOptions(groups, recentRefs) : []),
    [recents, searching, groups, recentRefs]
  );
  const recommended = useMemo(() => {
    if (searching) return [];
    const taken = new Set(recent.map((m) => m.ref));
    return recommendedOptions(groups).filter((r) => !taken.has(r.option.ref));
  }, [groups, searching, recent]);
  const sections = useMemo(() => pickerSections(groups, value), [groups, value]);

  // Design review only (mock mode): `?scrollto=<provider>` brings that group into view.
  const scrollTo = mockParam("scrollto");
  useEffect(() => {
    if (!open || !scrollTo) return;
    const id = window.setTimeout(
      () => document.querySelector(`[data-testid="${testId}-group-${scrollTo}"]`)?.scrollIntoView({ block: "start" }),
      150
    );
    return () => window.clearTimeout(id);
  }, [open, scrollTo, testId]);

  const toggle = (set: ReadonlySet<string>, id: string): Set<string> => {
    const next = new Set(set);
    if (!next.delete(id)) next.add(id);
    return next;
  };

  const close = useCallback(() => {
    setOpen(false);
    setQuery("");
  }, []);

  const openPicker = () => {
    // The selected model's source starts open so the current choice is visible.
    setExpanded(new Set([valueRef.provider]));
    setFull(new Set());
    if (recents) setRecentRefs(loadRecentModels());
    setOpen(true);
  };

  const pick = (ref: string) => {
    if (recents) saveRecentModels(withRecent(loadRecentModels(), ref));
    close();
    trigger.current?.focus();
    if (autoOption?.selected || ref !== value) {
      onPick(ref);
    }
  };

  const pickAuto = () => {
    if (!autoOption) return;
    close();
    trigger.current?.focus();
    autoOption.onPick();
  };

  const showAuto = Boolean(
    autoOption && (!query.trim() || matchesQuery(query, autoOption.label, autoOption.sub))
  );

  const label = (m: ModelOption) =>
    isCliDefault(m) ? providerBrand(m.provider).label : m.displayName;

  const valueProvider = valueRef.provider;
  const valueModel = valueRef.model;
  const current = groups.find((g) => g.provider === valueProvider)?.models.find((m) => m.ref === value);
  const valueLabel = current ? label(current) : valueModel ? modelDisplayName(valueModel) : "";

  // Arrow keys walk the rows (search box -> rows -> back up to the search box).
  const moveFocus = (e: KeyboardEvent<HTMLElement>) => {
    const dir = e.key === "ArrowDown" ? 1 : e.key === "ArrowUp" ? -1 : 0;
    if (!dir) return;
    const items = [...e.currentTarget.querySelectorAll<HTMLElement>('[role="option"], [data-picker-row]')];
    const next = items[items.indexOf(document.activeElement as HTMLElement) + dir];
    if (next) {
      e.preventDefault();
      next.focus();
    } else if (dir < 0) {
      e.preventDefault();
      e.currentTarget.closest("[role=dialog]")?.querySelector<HTMLInputElement>("input")?.focus();
    }
  };

  const optionRow = (m: ModelOption, opts: { hint?: RowHint; key?: string }) => {
    const active = !autoOption?.selected && m.ref === value;
    const primary = label(m);
    const block = blockOf(m.provider);
    // Second line: what the model is good at, in plain words (why it cannot answer comes first). The raw
    // id is the row's tooltip and is still found by the search.
    const secondary = [
      block ? blockReason(t, block) : null,
      opts.hint === "new" ? t("modelPicker.hint.new") : null,
      isCliDefault(m) ? null : t(`modelPicker.use.${modelUse(m, opts.hint)}`)
    ].filter(Boolean).join(" · ");
    return (
      <button
        key={`${opts.key ?? (opts.hint ? "rec" : "row")}-${m.ref}`}
        type="button"
        role="option"
        aria-selected={active}
        aria-disabled={block ? true : undefined}
        disabled={block ? true : undefined}
        data-blocked={block}
        className={[styles.option, active ? styles.optionActive : "", block ? styles.optionBlocked : ""].filter(Boolean).join(" ")}
        onClick={() => pick(m.ref)}
      >
        <BrandMark name={productBrandKey(m.provider, m.model)} size={16} />
        <span className={styles.optionText} title={m.model}>
          {primary}
          {secondary ? <span className={styles.useLine}>{secondary}</span> : null}
        </span>
        <span className={styles.source}>{sourceLabel(m.provider)}</span>
        {active ? <Check aria-hidden="true" /> : null}
      </button>
    );
  };

  const groupRow = (g: SourceGroup) => {
    const brand = providerBrand(g.provider);
    const isOpen = expanded.has(g.provider);
    const isFull = full.has(g.provider);
    const recRows = g.recommended;
    return (
      <div key={g.provider} className={styles.group} role="group" aria-label={sourceLabel(g.provider)}>
        <button
          type="button"
          data-picker-row
          className={styles.groupHead}
          aria-expanded={isOpen}
          data-testid={`${testId}-group-${g.provider}`}
          onClick={() => setExpanded(toggle(expanded, g.provider))}
        >
          <ChevronRight aria-hidden="true" className={isOpen ? styles.chevOpen : undefined} />
          <BrandMark name={brand.company} size={14} />
          <span className={styles.groupName}>{sourceLabel(g.provider)}</span>
          <span className={styles.groupCount}>{g.total}</span>
        </button>
        {isOpen ? (
          <>
            {recRows.map((r) => optionRow(r.option, { hint: r.hint }))}
            {recRows.length === 0 && g.rest.length === 0 ? (
              <p className={styles.noModels}>{t("modelPicker.noModels")}</p>
            ) : null}
            {isFull ? g.rest.map((m) => optionRow(m, {})) : null}
            {g.cli ? <CliListNote provider={g.provider} testId={testId} /> : null}
            {!isFull && g.rest.length > 0 ? (
              <button
                type="button"
                data-picker-row
                className={styles.showAll}
                data-testid={`${testId}-showall-${g.provider}`}
                onClick={() => setFull(toggle(full, g.provider))}
              >
                {t("modelPicker.showAll", { count: g.total })}
              </button>
            ) : null}
          </>
        ) : null}
      </div>
    );
  };

  const sectionLabel = (key: string) => (
    <div className={styles.sectionLabel} aria-hidden="true">
      <span>{t(key)}</span>
    </div>
  );

  return (
    <div className={styles.root}>
      <button
        ref={trigger}
        type="button"
        className={changeLink ? styles.changeLink : styles.trigger}
        disabled={disabled}
        aria-haspopup="dialog"
        aria-expanded={open}
        title={t("modelPicker.title")}
        data-testid={testId}
        onClick={() => (open ? close() : openPicker())}
      >
        {changeLink ? (
          changeLink
        ) : (
          <>
            {autoOption?.selected ? (
              <Sparkles aria-hidden="true" />
            ) : value ? (
              <BrandMark name={productBrandKey(valueProvider, valueModel)} size={15} />
            ) : null}
            <span className={styles.triggerText}>
              {autoOption?.selected
                ? autoOption.label
                : valueLabel ||
                  (noModel ? t("modelPicker.connectModel") : placeholder || t("modelPicker.none"))}
            </span>
            <ChevronDown />
          </>
        )}
      </button>
      <Popover
        open={open}
        anchor={trigger}
        onClose={close}
        placement={placement}
        role="dialog"
        label={t("modelPicker.title")}
        width={360}
        initialFocus="none"
      >
        {open && hasCli ? <EngineBlocksProbe onBlocks={setBlocks} /> : null}
        <label className={styles.search}>
          <Search aria-hidden="true" />
          <input
            value={query}
            placeholder={t("modelPicker.search")}
            aria-label={t("modelPicker.search")}
            autoFocus
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown") {
                e.preventDefault();
                e.currentTarget
                  .closest("[role=dialog]")
                  ?.querySelector<HTMLElement>('[role="option"], [data-picker-row]')
                  ?.focus();
              } else if (e.key === "Enter") {
                const first = shown[0]?.models[0];
                if (first && query.trim()) {
                  e.preventDefault();
                  pick(first.ref);
                } else if (showAuto && autoOption) {
                  e.preventDefault();
                  pickAuto();
                } else if (first) {
                  e.preventDefault();
                  pick(first.ref);
                }
              }
            }}
          />
        </label>
        <div className={styles.scroll} role="listbox" aria-label={t("modelPicker.title")} onKeyDown={moveFocus}>
          {noModel && !searching ? (
            <div className={styles.group} data-testid={`${testId}-connect`}>
              <p className={styles.noModels}>{t("modelPicker.connectBody")}</p>
              <button
                type="button"
                data-picker-row
                className={styles.option}
                data-testid={`${testId}-connect-button`}
                onClick={() => {
                  close();
                  openSettingsTarget({ page: "models", card: "keys", field: "api-key" });
                }}
              >
                <Plug aria-hidden="true" />
                <span className={styles.optionText}>{t("modelPicker.connectAction")}</span>
              </button>
            </div>
          ) : null}
          {shown.length === 0 && !showAuto && otherIds.length === 0 && !engines && !(noModel && !searching) ? (
            <p className={styles.empty}>{t("modelPicker.empty")}</p>
          ) : (
            <>
              {showAuto && autoOption ? (
                <button
                  type="button"
                  role="option"
                  aria-selected={autoOption.selected}
                  className={[styles.option, autoOption.selected ? styles.optionActive : ""].filter(Boolean).join(" ")}
                  onClick={pickAuto}
                >
                  <Sparkles aria-hidden="true" />
                  <span className={styles.optionText}>{autoOption.label}</span>
                  {autoOption.sub ? <span className={styles.optionSub}>{autoOption.sub}</span> : null}
                  {autoOption.selected ? <Check aria-hidden="true" /> : null}
                </button>
              ) : null}
              {otherIds.map((g) => (
                <button
                  key={`other-${g.provider}`}
                  type="button"
                  role="option"
                  aria-selected={false}
                  className={styles.option}
                  onClick={() => pick(`${g.provider}/${typed}`)}
                >
                  <span className={styles.optionText}>
                    {t("modelPicker.otherId", { id: typed, provider: providerBrand(g.provider).label })}
                  </span>
                </button>
              ))}
              {recent.length > 0 ? (
                <div className={styles.group} role="group" aria-label={t("modelPicker.recent")} data-testid={`${testId}-recent`}>
                  <div className={styles.groupLabel} aria-hidden="true">
                    <span>{t("modelPicker.recent")}</span>
                  </div>
                  {recent.map((m) => optionRow(m, { key: "recent" }))}
                </div>
              ) : null}
              {recommended.length > 0 ? (
                <div className={styles.group} role="group" aria-label={t("modelPicker.recommended")}>
                  <div className={styles.groupLabel} aria-hidden="true">
                    <span>{t("modelPicker.recommended")}</span>
                  </div>
                  {recommended.map((r) => optionRow(r.option, { hint: r.hint }))}
                </div>
              ) : null}
              {searching
                ? shown.map((g) => (
                    <div key={g.provider} className={styles.group} role="group" aria-label={sourceLabel(g.provider)}>
                      <div className={styles.groupLabel} aria-hidden="true">
                        <BrandMark name={providerBrand(g.provider).company} size={14} />
                        <span>{sourceLabel(g.provider)}</span>
                        <span className={styles.groupCount}>{g.models.length}</span>
                      </div>
                      {g.models.filter((m) => !isCliDefault(m)).map((m) => optionRow(m, {}))}
                    </div>
                  ))
                : (
                  <>
                    {sections.cli.length > 0 ? (
                      <>
                        {sectionLabel("modelPicker.sectionCli")}
                        {sections.cli.map(groupRow)}
                        <HostCliExtras ids={sections.emptyCli} testId={testId} />
                      </>
                    ) : sections.emptyCli.length > 0 ? (
                      <HostCliExtras ids={sections.emptyCli} testId={testId} label={sectionLabel("modelPicker.sectionCli")} />
                    ) : null}
                    {sections.api.length > 0 ? (
                      <>
                        {sectionLabel("modelPicker.sectionApi")}
                        {sections.api.map(groupRow)}
                      </>
                    ) : null}
                  </>
                )}
              {cliNote && !searching ? (
                <div className={styles.group} role="group" aria-label={t("modelPicker.sectionCli")} data-testid={`${testId}-cli-greyed`}>
                  {sectionLabel("modelPicker.sectionCli")}
                  <p className={styles.noModels}>{cliNote}</p>
                </div>
              ) : null}
              {engines ? (
                <EngineSection query={query} testId={testId} onInstall={close} onConnected={() => undefined} />
              ) : null}
            </>
          )}
        </div>
        {thinking && isMockMode() ? (
          <div className={styles.effort}>
            <span className={styles.effortLabel} id="thinking-level-label">
              {t("modelPicker.thinking")}
            </span>
            <div
              className={styles.seg}
              role="radiogroup"
              aria-labelledby="thinking-level-label"
              onKeyDown={(e) => {
                const step = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
                if (!step) return;
                e.preventDefault();
                const i = THINKING_LEVELS.indexOf(level);
                const next = THINKING_LEVELS[(i + step + THINKING_LEVELS.length) % THINKING_LEVELS.length];
                if (next) {
                  setLevel(next);
                  e.currentTarget.querySelectorAll<HTMLElement>('[role="radio"]')[THINKING_LEVELS.indexOf(next)]?.focus();
                }
              }}
            >
              {THINKING_LEVELS.map((l) => (
                <button
                  key={l}
                  type="button"
                  role="radio"
                  tabIndex={l === level ? 0 : -1}
                  aria-checked={l === level}
                  className={l === level ? styles.segOn : undefined}
                  onClick={() => setLevel(l)}
                >
                  {t(`modelPicker.levels.${l}`)}
                </button>
              ))}
            </div>
          </div>
        ) : null}
      </Popover>
    </div>
  );
}
