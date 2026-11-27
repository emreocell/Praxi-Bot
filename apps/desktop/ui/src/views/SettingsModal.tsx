import {
  BrainCircuit,
  Check,
  CloudDownload,
  KeyRound,
  LoaderCircle,
  Palette,
  Plug,
  RotateCcw,
  Search,
  Settings2,
  ShieldCheck,
  SlidersHorizontal,
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
import {
  CardHelpContext,
  SettingGroup,
  SettingsCardFooter,
  SettingsDisclosure
} from "../components/SettingsUi";
import { humanLine } from "../lib/humanError";
import { peekPendingTarget } from "../lib/settingsLink";
import {
  CARD_META,
  canReset,
  type DetailMode,
  groupCards,
  groupOfCard,
  groupTextKey,
  highlightParts,
  initialDetailMode,
  metaOfCard,
  pageTextKey,
  parsePage,
  queryWords,
  RECOMMENDED_SAFETY_PRESET,
  resetPatch,
  SETTINGS_GROUPS,
  SETTINGS_PAGE_GROUPS,
  type SettingsGroupId,
  type SettingsPageId,
  settingsSearchMatch,
  settingsSearchRank
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

const PAGE_ICONS: Record<SettingsPageId, typeof Settings2> = {
  general: Settings2,
  appearance: Palette,
  models: BrainCircuit,
  connections: Plug,
  safety: ShieldCheck,
  vault: KeyRound,
  system: SlidersHorizontal,
  updates: CloudDownload
};

const CARD_ORDER = new Map(Object.keys(CARD_META).map((id, i) => [id, i]));
const FLASH_MS = 220;

/** A card with its page, plus the plain-words extras (consequence line, "what is this?"). */
interface PlacedCard {
  card: CardDef;
  page: SettingsPageId;
  group: SettingsGroupId;
  advanced: boolean;
  risk?: string;
  learn?: ReturnType<typeof metaOfCard>["learn"];
}

export interface SettingsTargetRequest {
  page: SettingsSectionId | SettingsPageId;
  card?: string;
  field?: string;
  seq?: number;
}

/** Marks the words of `query` in `text` (used for card titles and page tags in search results). */
function Marked({ text, query }: { text: string; query: string }) {
  return (
    <>
      {highlightParts(text, query).map((p, i) =>
        p.hit ? <mark key={i} className={styles.mark}>{p.text}</mark> : <span key={i}>{p.text}</span>
      )}
    </>
  );
}

/** The card, with its "?" (if it has one) handed to its own header. */
function withHelp(p: PlacedCard): ReactNode {
  return p.learn ? (
    <CardHelpContext.Provider value={<LearnMore term={p.learn} />}>{p.card.node}</CardHelpContext.Provider>
  ) : (
    p.card.node
  );
}

/**
 * Consecutive one-line `row` cards share one rounded group; a `standalone` row card (its own
 * topic, e.g. Theme or Backdrop) and every `block` card get a frame of their own, so unrelated
 * settings are never stacked into one long card.
 */
function renderCards(
  cards: PlacedCard[],
  extras: (p: PlacedCard) => ReactNode,
  flashCard: string | null
): ReactNode[] {
  const out: ReactNode[] = [];
  let run: PlacedCard[] = [];
  const slot = (p: PlacedCard) => (
    <div
      key={p.card.id}
      className={styles.cardSlot}
      data-card={p.card.id}
      data-flash={flashCard === p.card.id ? "true" : undefined}
    >
      {withHelp(p)}
      {extras(p)}
    </div>
  );
  const flush = () => {
    if (run.length > 0) {
      const first = run[0].card.id;
      out.push(<SettingGroup key={`group-${first}`}>{run.map(slot)}</SettingGroup>);
      run = [];
    }
  };
  for (const p of cards) {
    if (p.card.kind === "row" && p.card.standalone) {
      flush();
      out.push(<SettingGroup key={`group-${p.card.id}`}>{slot(p)}</SettingGroup>);
    } else if (p.card.kind === "row") {
      run.push(p);
    } else {
      flush();
      out.push(
        <div
          key={p.card.id}
          className={styles.blockSlot}
          data-card={p.card.id}
          data-flash={flashCard === p.card.id ? "true" : undefined}
        >
          {withHelp(p)}
          {extras(p)}
        </div>
      );
    }
  }
  flush();
  return out;
}

function isEditableElement(el: EventTarget | null): boolean {
  if (!(el instanceof HTMLElement)) return false;
  if (el.isContentEditable) return true;
  const tag = el.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
}

function prefersReducedMotion(): boolean {
  if (typeof window === "undefined") return false;
  return (
    document.documentElement.hasAttribute("data-reduce-motion") ||
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

type SaveState = "idle" | "saving" | "saved";

/**
 * Settings modal: 6 plainly named pages in titled groups, forgiving search across every setting
 * with keyboard navigation, one per-page "Advanced settings" disclosure (closed on open), real deep links
 * (scroll + flash + focus), saved status and per-page "Reset to recommended".
 */
export function SettingsModal({
  open,
  initialSection = "general",
  initialTarget,
  onClose,
  bridge
}: {
  open: boolean;
  /** A page id, or an old section id (resolved via alias table). */
  initialSection?: SettingsSectionId | SettingsPageId;
  /** Optional card/field deep link to scroll to, flash and focus when opening. */
  initialTarget?: SettingsTargetRequest | null;
  onClose: () => void;
  bridge: SettingsBridge;
}) {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const [page, setPage] = useState<SettingsPageId>(() => parsePage(initialSection));
  const [query, setQuery] = useState("");
  // "Advanced settings" starts closed on every open (initialDetailMode); not remembered across opens.
  const [mode, setMode] = useState<DetailMode>("simple");
  const [save, setSave] = useState<SaveState>("idle");
  const [confirmReset, setConfirmReset] = useState(false);
  const [resetBusy, setResetBusy] = useState(false);
  const [resetError, setResetError] = useState<string | null>(null);
  const [resetDone, setResetDone] = useState(false);
  const [flashCard, setFlashCard] = useState<string | null>(null);
  const [pendingReveal, setPendingReveal] = useState<{ card: string; field?: string; seq: number } | null>(null);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const searchRef = useRef<HTMLInputElement | null>(null);
  const timer = useRef<number | undefined>(undefined);
  const flashTimer = useRef<number | undefined>(undefined);
  const revealSeq = useRef(0);

  const changeMode = useCallback((m: DetailMode) => {
    setMode(m);
  }, []);

  useEffect(() => {
    if (!open) return;
    const target = initialTarget ?? (peekPendingTarget() ? { ...peekPendingTarget()! } : null);
    const cardId = target?.card;
    const cardMeta = cardId ? CARD_META[cardId] : undefined;
    setPage(cardMeta ? parsePage(cardMeta.page) : parsePage(target?.page ?? initialSection));
    setQuery("");
    setSave("idle");
    setResetDone(false);
    changeMode(initialDetailMode(cardMeta?.advanced === true));
    if (cardId) {
      setPendingReveal({ card: cardId, field: target?.field, seq: ++revealSeq.current });
    } else {
      setPendingReveal(null);
    }
  }, [open, initialSection, initialTarget, changeMode]);

  useEffect(
    () => () => {
      window.clearTimeout(timer.current);
      window.clearTimeout(flashTimer.current);
    },
    []
  );

  // Scroll target card into view, flash it briefly (respecting reduced motion), and focus target field.
  useEffect(() => {
    if (!open || !pendingReveal) return undefined;
    const { card, field } = pendingReveal;
    let cancelled = false;
    const raf = window.requestAnimationFrame(() => {
      if (cancelled) return;
      const root = scrollRef.current;
      const slot = root?.querySelector<HTMLElement>(`[data-card="${card}"]`);
      if (!slot) return;
      const instant = prefersReducedMotion();
      slot.scrollIntoView?.({ block: "center", behavior: instant ? "instant" : "smooth" });
      if (!instant) {
        window.clearTimeout(flashTimer.current);
        setFlashCard(card);
        flashTimer.current = window.setTimeout(() => setFlashCard(null), FLASH_MS);
      }
      const fieldEl = field
        ? slot.querySelector<HTMLElement>(`[data-settings-field="${field}"]`)
        : null;
      const fallbackEl =
        fieldEl ??
        slot.querySelector<HTMLElement>(
          'input:not([disabled]), select:not([disabled]), textarea:not([disabled]), button:not([disabled]), [tabindex]:not([tabindex="-1"])'
        );
      fallbackEl?.focus({ preventScroll: true });
      setPendingReveal(null);
    });
    return () => {
      cancelled = true;
      window.cancelAnimationFrame(raf);
    };
  }, [open, pendingReveal, page, mode]);

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
      ]
        .map((card, index) => {
          const m = metaOfCard(card);
          return {
            card,
            page: m.page,
            group: groupOfCard(m),
            advanced: m.advanced === true,
            risk: m.risk,
            learn: m.learn,
            order: CARD_ORDER.get(card.id) ?? 1000 + index
          };
        })
        .sort((a, b) => a.order - b.order),
    // `bridge` changes on every settings update: cards must follow it.
    [wrapped, t]
  );

  const searching = query.trim() !== "";
  const matches = useCallback(
    (p: PlacedCard) =>
      settingsSearchMatch(
        query,
        p.card.title,
        p.card.description,
        p.card.keywords,
        t(pageTextKey(p.page, "name")),
        t(groupTextKey(p.group))
      ),
    [query, t]
  );

  const searchHits = useMemo(
    () =>
      searching
        ? placed
            .filter(matches)
            .map((p, i) => ({ p, i, rank: settingsSearchRank(query, p.card.title, p.card.description) }))
            .sort((a, b) => a.rank - b.rank || a.i - b.i)
            .map((x) => x.p)
        : [],
    [searching, placed, matches, query]
  );
  const pagesWithHits = useMemo(
    () => new Set(searchHits.map((p) => p.page)),
    [searchHits]
  );
  const onPage = useMemo(
    () => placed.filter((p) => p.page === page),
    [placed, page]
  );
  const basicOnPage = useMemo(() => onPage.filter((p) => !p.advanced), [onPage]);
  const advancedOnPage = useMemo(() => onPage.filter((p) => p.advanced), [onPage]);
  const moreOpen = mode === "advanced";

  const enabledPages = useMemo(
    () =>
      SETTINGS_PAGE_GROUPS.flatMap((g) => g.pages).filter(
        (s) => !(searching && !pagesWithHits.has(s))
      ),
    [searching, pagesWithHits]
  );

  const jumpToCard = useCallback(
    (cardId: string) => {
      const hit = placed.find((p) => p.card.id === cardId);
      if (!hit) return;
      setQuery("");
      setPage(hit.page);
      if (hit.advanced) {
        changeMode("advanced");
      }
      setPendingReveal({ card: cardId, seq: ++revealSeq.current });
    },
    [placed, changeMode]
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
  }, [query, open, searchHits.length, page, mode]);

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

  const onShellKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "/" && !e.ctrlKey && !e.metaKey && !e.altKey && !isEditableElement(e.target)) {
      e.preventDefault();
      searchRef.current?.focus();
      searchRef.current?.select();
    }
  };

  const onSearchKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown" && searching && searchHits.length > 0) {
      e.preventDefault();
      scrollRef.current?.querySelector<HTMLElement>("[data-settings-result]")?.focus();
    } else if (e.key === "Enter" && searching && searchHits.length > 0) {
      e.preventDefault();
      jumpToCard(searchHits[0].card.id);
    }
  };

  const onResultKeyDown = (e: KeyboardEvent<HTMLButtonElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const buttons = [...(scrollRef.current?.querySelectorAll<HTMLElement>("[data-settings-result]") ?? [])];
    const idx = buttons.indexOf(e.currentTarget);
    if (idx === -1) return;
    e.preventDefault();
    if (e.key === "ArrowUp" && idx === 0) {
      searchRef.current?.focus();
      return;
    }
    const nextIdx = e.key === "ArrowDown" ? Math.min(buttons.length - 1, idx + 1) : Math.max(0, idx - 1);
    buttons[nextIdx]?.focus();
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

  // The "?" sits in each card's header (withHelp); the footer keeps only the consequence line.
  const extras = (p: PlacedCard): ReactNode =>
    p.risk ? <SettingsCardFooter risk={t(`settingsModal.risk.${p.risk}`)} /> : null;

  const pageName = (id: SettingsPageId) => t(pageTextKey(id, "name"));
  const groupName = (id: SettingsGroupId) => t(groupTextKey(id));
  // A page of one group (Vault, Updates) needs no group heading under the page title.
  const singleGroup = SETTINGS_GROUPS[page].length === 1;
  // A whole tool on its own page (the Vault) fills the pane and scrolls inside itself.
  const fullPageCard = !searching && onPage.length === 1 && onPage[0].card.fullPage ? onPage[0] : null;
  // A page reads as a few titled groups; the cards of a group keep the usual row / block framing.
  const renderGroups = (cards: PlacedCard[], scope: "basic" | "advanced") =>
    groupCards(page, cards, (p) => ({ page: p.page, group: p.group })).map((g) => (
      <section
        key={`${scope}-${g.group}`}
        className={styles.group}
        aria-labelledby={singleGroup ? undefined : `settings-group-${scope}-${g.group}`}
        aria-label={singleGroup ? groupName(g.group) : undefined}
        data-testid={`settings-group-${g.group}`}
      >
        {singleGroup ? null : (
          <h3 id={`settings-group-${scope}-${g.group}`} className={styles.groupTitle}>
            {groupName(g.group)}
          </h3>
        )}
        {renderCards(g.cards, extras, flashCard)}
      </section>
    ));
  const resettable = !searching && canReset(page);

  return (
    <Modal
      open={open}
      onClose={onClose}
      onEscape={() => {
        if (query !== "") {
          setQuery("");
          searchRef.current?.focus();
          return true;
        }
        return false;
      }}
      label={t("settingsModal.title")}
    >
      <div className={styles.shell} onKeyDown={onShellKeyDown}>
        <nav className={styles.nav} aria-label={t("settingsModal.title")}>
          <div className={styles.navHeading}>
            <Settings2 aria-hidden="true" />
            <span>{t("settingsModal.title")}</span>
          </div>
          <div className={styles.searchWrap}>
            <Search aria-hidden="true" />
            <input
              ref={searchRef}
              className={styles.search}
              type="search"
              data-autofocus
              data-testid="settings-search"
              value={query}
              placeholder={t("settingsModal.search")}
              aria-label={t("settingsModal.search")}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={onSearchKeyDown}
            />
            {!query ? <kbd className={styles.searchHint} aria-hidden="true">/</kbd> : null}
          </div>
          <div className={styles.navScroll} onKeyDown={onNavKeyDown}>
            {SETTINGS_PAGE_GROUPS.map((g) => (
              <div key={g.id} className={styles.navList} role="group" aria-label={t(`settingsModal.navGroups.${g.id}`)}>
                <span className={styles.navGroupLabel} aria-hidden="true">
                  {t(`settingsModal.navGroups.${g.id}`)}
                </span>
                {g.pages.map((s) => {
                  const PageIcon = PAGE_ICONS[s];
                  const dim = searching && !pagesWithHits.has(s);
                  return (
                    <button
                      key={s}
                      type="button"
                      data-testid={`settings-nav-${s}`}
                      title={t(pageTextKey(s, "description"))}
                      disabled={dim}
                      aria-current={!searching && s === page ? "page" : undefined}
                      className={[styles.navItem, !searching && s === page ? styles.navActive : ""].filter(Boolean).join(" ")}
                      onClick={() => {
                        setQuery("");
                        setPage(s);
                      }}
                    >
                      <PageIcon aria-hidden="true" />
                      <span>{pageName(s)}</span>
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
                  ? t("settingsModal.resultsCount", { count: searchHits.length })
                  : t(pageTextKey(page, "description"))}
              </p>
            </div>
            <div className={styles.headerTools}>
              {resetDone ? (
                <span className={styles.resetDone} role="status">{t("settingsModal.reset.done")}</span>
              ) : null}
              {resettable ? (
                <button
                  type="button"
                  className={styles.resetButton}
                  data-testid="settings-reset"
                  title={t("settingsModal.reset.button")}
                  onClick={() => {
                    setResetError(null);
                    setConfirmReset(true);
                  }}
                >
                  <RotateCcw aria-hidden="true" />
                  <span>{t("settingsModal.reset.button")}</span>
                </button>
              ) : null}
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

          <div
            className={[styles.scroll, fullPageCard ? styles.scrollFull : ""].filter(Boolean).join(" ")}
            ref={scrollRef}
          >
            {searching ? (
              searchHits.length === 0 ? (
                <p className={styles.empty}>
                  <Search aria-hidden="true" />
                  <span>{t("settingsModal.noResults")}</span>
                </p>
              ) : (
                <div className={styles.resultsList}>
                  {searchHits.map((p) => (
                    <div key={p.card.id} className={styles.resultCard}>
                      <button
                        type="button"
                        className={styles.resultHead}
                        data-settings-result
                        data-testid={`settings-result-${p.card.id}`}
                        onClick={() => jumpToCard(p.card.id)}
                        onKeyDown={onResultKeyDown}
                      >
                        <span className={styles.resultTitle}>
                          <Marked text={p.card.title} query={query} />
                        </span>
                        <span className={styles.resultMeta}>
                          <span className={styles.pageTag}>
                            <Marked text={pageName(p.page)} query={query} />
                            <span aria-hidden="true"> › </span>
                            <Marked text={groupName(p.group)} query={query} />
                          </span>
                          {p.advanced ? (
                            <span className={styles.advTag}>{t("settingsModal.mode.moreOptions")}</span>
                          ) : null}
                        </span>
                      </button>
                      {/* A whole tool (the Vault) is not drawn inside the results: its heading jumps to it. */}
                      {p.card.fullPage ? null : renderCards([p], extras, flashCard)}
                    </div>
                  ))}
                </div>
              )
            ) : fullPageCard ? (
              <div
                className={styles.fullPage}
                data-card={fullPageCard.card.id}
                data-testid={`settings-group-${fullPageCard.group}`}
              >
                {withHelp(fullPageCard)}
              </div>
            ) : (
              <div className={styles.section}>
                {renderGroups(basicOnPage, "basic")}
                {advancedOnPage.length > 0 ? (
                  <SettingsDisclosure
                    open={moreOpen}
                    count={advancedOnPage.length}
                    label={t("settingsModal.mode.moreOptions")}
                    hint={t("settingsModal.mode.moreHint")}
                    testId="settings-show-advanced"
                    onToggle={() => changeMode(moreOpen ? "simple" : "advanced")}
                  >
                    {renderGroups(advancedOnPage, "advanced")}
                  </SettingsDisclosure>
                ) : null}
              </div>
            )}
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
