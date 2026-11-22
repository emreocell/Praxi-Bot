import {
  AppWindow,
  Bell,
  BrainCircuit,
  Check,
  Database,
  LoaderCircle,
  Monitor,
  Mic,
  Palette,
  RotateCcw,
  Search,
  Settings2,
  ShieldCheck,
  SlidersHorizontal,
  TriangleAlert,
  X
} from "lucide-react";
import {
  type KeyboardEvent,
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState
} from "react";
import { useTranslation } from "react-i18next";
import { ConfirmDialog } from "../components/DialogParts";
import { LearnMore } from "../components/LearnMore";
import { Modal } from "../components/Modal";
import { Segmented, SettingGroup } from "../components/SettingsUi";
import { humanLine } from "../lib/humanError";
import { matchesQuery } from "../lib/searchText";
import {
  canReset,
  type DetailMode,
  highlightParts,
  isCardShown,
  loadDetailMode,
  metaOfCard,
  parsePage,
  queryWords,
  RECOMMENDED_SAFETY_PRESET,
  resetPatch,
  saveDetailMode,
  SETTINGS_PAGE_GROUPS,
  type SettingsPageId
} from "../lib/settingsPages";
import { useDaemon } from "../state/DaemonContext";
import { DEFAULT_SETTINGS } from "../store/settingsStore";
import { aiCards } from "./settings/AiCards";
import { appearanceCards } from "./settings/AppearanceCards";
import { computerCards } from "./settings/ComputerCards";
import { generalCards } from "./settings/GeneralCards";
import { mailCards } from "./settings/MailCards";
import { otherCards } from "./settings/OtherCards";
import { POLICY_CHANGED_EVENT } from "./settings/SafetyLevelsCard";
import type { CardDef, SettingsBridge, SettingsSectionId } from "./settings/types";
import styles from "./SettingsModal.module.css";

const PAGE_ICONS = {
  general: Settings2,
  appearance: Palette,
  models: BrainCircuit,
  where: Monitor,
  apps: AppWindow,
  safety: ShieldCheck,
  notifications: Bell,
  voice: Mic,
  data: Database,
  advanced: SlidersHorizontal
} as const;

/** A card with its page, plus the plain-words extras (consequence line, "what is this?"). */
interface PlacedCard {
  card: CardDef;
  page: SettingsPageId;
  advanced: boolean;
  risk?: string;
  learn?: ReturnType<typeof metaOfCard>["learn"];
}

/** Marks the words of `query` in `text` (used for card titles in search results). */
function Marked({ text, query }: { text: string; query: string }) {
  return (
    <>
      {highlightParts(text, query).map((p, i) =>
        p.hit ? <mark key={i} className={styles.mark}>{p.text}</mark> : <span key={i}>{p.text}</span>
      )}
    </>
  );
}

/** Consecutive `row` cards share one rounded group; `block` cards stand alone. */
function renderCards(
  cards: PlacedCard[],
  groupLabel: (key: string) => string,
  extras: (p: PlacedCard) => ReactNode
): ReactNode[] {
  const out: ReactNode[] = [];
  let run: PlacedCard[] = [];
  const flush = () => {
    if (run.length > 0) {
      const first = run[0].card.id;
      const group = first === "account" ? "account" : first === "language" ? "behavior" : null;
      if (group) out.push(<h3 key={`label-${first}`} className={styles.sectionLabel}>{groupLabel(group)}</h3>);
      out.push(
        <SettingGroup key={`group-${first}`}>
          {run.map((p) => (
            <div key={p.card.id} className={styles.cardSlot} data-card={p.card.id}>
              {p.card.node}
              {extras(p)}
            </div>
          ))}
        </SettingGroup>
      );
      run = [];
    }
  };
  for (const p of cards) {
    if (p.card.id === "language") flush();
    if (p.card.kind === "row") {
      run.push(p);
    } else {
      flush();
      out.push(
        <div key={p.card.id} className={styles.blockSlot} data-card={p.card.id}>
          {p.card.node}
          {extras(p)}
        </div>
      );
    }
  }
  flush();
  return out;
}

type SaveState = "idle" | "saving" | "saved";

/**
 * Settings as a modal: ten plainly named pages, a search over every setting, Simple / Advanced,
 * a clear saved indicator and a per-page "Reset to recommended".
 */
export function SettingsModal({
  open,
  initialSection = "general",
  onClose,
  bridge
}: {
  open: boolean;
  /** A page id, or an old section id (those still open the right page). */
  initialSection?: SettingsSectionId | SettingsPageId;
  onClose: () => void;
  bridge: SettingsBridge;
}) {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const [page, setPage] = useState<SettingsPageId>(() => parsePage(initialSection));
  const [query, setQuery] = useState("");
  const [mode, setMode] = useState<DetailMode>(() => loadDetailMode());
  const [save, setSave] = useState<SaveState>("idle");
  const [confirmReset, setConfirmReset] = useState(false);
  const [resetBusy, setResetBusy] = useState(false);
  const [resetError, setResetError] = useState<string | null>(null);
  const [resetDone, setResetDone] = useState(false);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const timer = useRef<number | undefined>(undefined);

  useEffect(() => {
    if (open) {
      setPage(parsePage(initialSection));
      setQuery("");
      setSave("idle");
      setResetDone(false);
    }
  }, [open, initialSection]);

  useEffect(() => () => window.clearTimeout(timer.current), []);

  const markSaved = useCallback((deferred: boolean) => {
    window.clearTimeout(timer.current);
    setSave(deferred ? "saving" : "saved");
    if (deferred) timer.current = window.setTimeout(() => setSave("saved"), 900);
  }, []);

  // Every change goes through here, so the "Saved" pill is truthful for all cards.
  const wrapped = useMemo<SettingsBridge>(
    () => ({
      ...bridge,
      update: (patch, opts) => {
        bridge.update(patch, opts);
        markSaved(opts?.deferSave === true);
      }
    }),
    [bridge, markSaved]
  );

  const placed = useMemo<PlacedCard[]>(
    () =>
      [
        ...generalCards(wrapped, t),
        ...appearanceCards(wrapped, t),
        ...aiCards(t),
        ...computerCards(wrapped, t),
        ...mailCards(t),
        ...otherCards(t)
      ].map((card) => {
        const m = metaOfCard(card);
        return { card, page: m.page, advanced: m.advanced === true, risk: m.risk, learn: m.learn };
      }),
    // `bridge` changes on every settings update: cards must follow it.
    [wrapped, t]
  );

  const searching = query.trim() !== "";
  const matches = (p: PlacedCard) =>
    matchesQuery(
      query,
      p.card.title,
      p.card.description,
      p.card.keywords,
      t(`settingsModal.pages.${p.page}.name`)
    );
  const pagesWithHits = new Set(placed.filter(matches).map((p) => p.page));
  const onPage = placed.filter((p) => p.page === page);
  const visible = searching
    ? placed.filter(matches)
    : onPage.filter((p) => isCardShown({ page: p.page, advanced: p.advanced }, mode, false));
  const hiddenCount = searching ? 0 : onPage.length - visible.length;
  const hasAdvancedHere = onPage.some((p) => p.advanced) && page !== "advanced";
  const enabledPages = SETTINGS_PAGE_GROUPS.flatMap((g) => g.pages).filter(
    (s) => !(searching && !pagesWithHits.has(s))
  );

  // Highlight the matched words inside the cards without touching React's DOM.
  useEffect(() => {
    const css = (globalThis as { CSS?: { highlights?: Map<string, unknown> } }).CSS;
    const root = scrollRef.current;
    const HighlightCtor = (globalThis as { Highlight?: new (...r: Range[]) => unknown }).Highlight;
    if (!css?.highlights || !HighlightCtor || !root) return undefined;
    const words = queryWords(query);
    css.highlights.delete("settings-search");
    if (!open || words.length === 0) return undefined;
    const ranges: Range[] = [];
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      const text = n.textContent ?? "";
      if (n.parentElement?.closest("mark,button,input,select,textarea")) continue;
      const parts = highlightParts(text, query);
      let at = 0;
      for (const p of parts) {
        if (p.hit) {
          const r = document.createRange();
          r.setStart(n, at);
          r.setEnd(n, at + p.text.length);
          ranges.push(r);
        }
        at += p.text.length;
      }
    }
    if (ranges.length > 0) css.highlights.set("settings-search", new HighlightCtor(...ranges));
    return () => {
      css.highlights?.delete("settings-search");
    };
  }, [query, open, visible.length, page, mode]);

  const onNavKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp" && e.key !== "Home" && e.key !== "End") return;
    if (enabledPages.length === 0) return;
    e.preventDefault();
    const cur = Math.max(0, enabledPages.indexOf(page));
    const nextIdx =
      e.key === "Home"
        ? 0
        : e.key === "End"
          ? enabledPages.length - 1
          : e.key === "ArrowDown"
            ? (cur + 1) % enabledPages.length
            : (cur - 1 + enabledPages.length) % enabledPages.length;
    const next = enabledPages[nextIdx];
    setQuery("");
    setPage(next);
    e.currentTarget.querySelector<HTMLElement>(`[data-testid="settings-nav-${next}"]`)?.focus();
  };

  const changeMode = (m: DetailMode) => {
    setMode(m);
    saveDetailMode(m);
  };

  const doReset = async () => {
    setResetBusy(true);
    setResetError(null);
    try {
      const patch = resetPatch(page, DEFAULT_SETTINGS);
      if (patch) wrapped.update(patch);
      if (page === "safety") {
        if (!client) throw new Error("offline");
        await client.putPolicy({ preset: RECOMMENDED_SAFETY_PRESET });
        window.dispatchEvent(new Event(POLICY_CHANGED_EVENT));
        markSaved(false);
      }
      setConfirmReset(false);
      setResetDone(true);
    } catch (e) {
      setResetError(
        e instanceof Error && e.message === "offline" ? t("settingsModal.reset.offline") : humanLine(e)
      );
    } finally {
      setResetBusy(false);
    }
  };

  const extras = (p: PlacedCard): ReactNode => {
    const showRisk = p.risk !== undefined;
    if (!showRisk && !p.learn) return null;
    return (
      <div className={styles.cardNote}>
        {showRisk ? (
          <p className={styles.risk}>
            <TriangleAlert aria-hidden="true" />
            <span>{t(`settingsModal.risk.${p.risk}`)}</span>
          </p>
        ) : null}
        {p.learn ? <LearnMore term={p.learn} /> : null}
      </div>
    );
  };

  const pageName = (id: SettingsPageId) => t(`settingsModal.pages.${id}.name`);
  const resettable = !searching && canReset(page);
  const grouped = searching
    ? enabledPages.filter((s) => visible.some((p) => p.page === s))
    : [page];

  return (
    <Modal open={open} onClose={onClose} label={t("settingsModal.title")}>
      <div className={styles.shell}>
        <nav className={styles.nav} aria-label={t("settingsModal.title")}>
          <div className={styles.navHeading}><Settings2 aria-hidden="true" />{t("settingsModal.title")}</div>
          <div className={styles.searchWrap}>
            <Search aria-hidden="true" />
            <input
              className={styles.search}
              type="search"
              data-autofocus
              data-testid="settings-search"
              value={query}
              placeholder={t("settingsModal.search")}
              aria-label={t("settingsModal.search")}
              onChange={(e) => setQuery(e.target.value)}
            />
          </div>
          <div className={styles.navScroll} onKeyDown={onNavKeyDown}>
            {SETTINGS_PAGE_GROUPS.map((g) => (
              <div key={g.id} className={styles.navList} role="group" aria-label={t(`settingsModal.navGroups.${g.id}`)}>
                <span className={styles.navGroupLabel} aria-hidden="true">{t(`settingsModal.navGroups.${g.id}`)}</span>
                {g.pages.map((s) => {
                  const PageIcon = PAGE_ICONS[s];
                  const dim = searching && !pagesWithHits.has(s);
                  return (
                    <button
                      key={s}
                      type="button"
                      data-testid={`settings-nav-${s}`}
                      disabled={dim}
                      aria-current={!searching && s === page ? "page" : undefined}
                      className={[styles.navItem, !searching && s === page ? styles.navActive : ""].filter(Boolean).join(" ")}
                      onClick={() => {
                        setQuery("");
                        setPage(s);
                      }}
                    >
                      <PageIcon aria-hidden="true" />{pageName(s)}
                    </button>
                  );
                })}
              </div>
            ))}
          </div>
        </nav>
        <div className={styles.content}>
          <header className={styles.header}>
            <div className={styles.headerText}>
              <h2 className={styles.title}>
                {searching ? t("settingsModal.results") : pageName(page)}
              </h2>
              <p className={styles.subtitle}>
                {searching
                  ? t("settingsModal.resultsCount", { count: visible.length })
                  : t(`settingsModal.pages.${page}.description`)}
              </p>
            </div>
            <div className={styles.headerTools}>
              <span
                className={[styles.saveState, save === "saved" ? styles.saveOk : ""].filter(Boolean).join(" ")}
                role="status"
                aria-live="polite"
                data-testid="settings-save-state"
              >
                {save === "saving" ? <LoaderCircle className={styles.spin} aria-hidden="true" /> : <Check aria-hidden="true" />}
                <span>{t(`settingsModal.save.${save}`)}</span>
              </span>
              <button
                type="button"
                className={styles.close}
                title={t("common.close")}
                aria-label={t("common.close")}
                onClick={onClose}
              >
                <X aria-hidden="true" />
              </button>
            </div>
          </header>
          {!searching ? (
            <div className={styles.toolbar}>
              <Segmented<DetailMode>
                label={t("settingsModal.mode.label")}
                value={mode}
                onChange={changeMode}
                options={[
                  { value: "simple", label: t("settingsModal.mode.simple"), testId: "settings-mode-simple" },
                  { value: "advanced", label: t("settingsModal.mode.advanced"), testId: "settings-mode-advanced" }
                ]}
              />
              {resettable ? (
                <button
                  type="button"
                  className={styles.resetButton}
                  data-testid="settings-reset"
                  onClick={() => {
                    setResetError(null);
                    setConfirmReset(true);
                  }}
                >
                  <RotateCcw aria-hidden="true" />
                  {t("settingsModal.reset.button")}
                </button>
              ) : null}
              {resetDone ? (
                <span className={styles.resetDone} role="status">{t("settingsModal.reset.done")}</span>
              ) : null}
            </div>
          ) : null}
          <div className={styles.scroll} ref={scrollRef}>
            {visible.length === 0 ? (
              <p className={styles.empty}><Search aria-hidden="true" />{t("settingsModal.noResults")}</p>
            ) : (
              grouped.map((s) => (
                <section key={s} className={styles.section}>
                  {searching ? (
                    <h3 className={styles.sectionLabel}>
                      <Marked text={pageName(s)} query={query} />
                    </h3>
                  ) : null}
                  {searching
                    ? visible
                        .filter((p) => p.page === s)
                        .map((p) => (
                          <div key={p.card.id} className={styles.resultCard}>
                            <h4 className={styles.resultTitle}>
                              <Marked text={p.card.title} query={query} />
                              {p.advanced ? <span className={styles.advTag}>{t("settingsModal.mode.advanced")}</span> : null}
                            </h4>
                            {renderCards([p], (k) => t(`settingsModal.groups.${k}`), extras)}
                          </div>
                        ))
                    : renderCards(
                        visible.filter((p) => p.page === s),
                        (k) => t(`settingsModal.groups.${k}`),
                        extras
                      )}
                </section>
              ))
            )}
            {hiddenCount > 0 ? (
              <button
                type="button"
                className={styles.moreAdvanced}
                data-testid="settings-show-advanced"
                onClick={() => changeMode("advanced")}
              >
                <SlidersHorizontal aria-hidden="true" />
                {t("settingsModal.mode.hidden", { count: hiddenCount })}
              </button>
            ) : null}
            {!searching && mode === "advanced" && hasAdvancedHere ? (
              <p className={styles.advancedNote}>{t("settingsModal.mode.advancedOn")}</p>
            ) : null}
          </div>
        </div>
      </div>
      <ConfirmDialog
        open={confirmReset}
        title={t("settingsModal.reset.title", { page: pageName(page) })}
        body={t(`settingsModal.reset.body.${page === "safety" ? "safety" : "app"}`)}
        confirmLabel={t("settingsModal.reset.confirm")}
        busy={resetBusy}
        error={resetError}
        onConfirm={() => void doReset()}
        onCancel={() => setConfirmReset(false)}
      />
    </Modal>
  );
}
