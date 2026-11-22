import { BrainCircuit, ChartNoAxesCombined, Mail, Monitor, Palette, Search, Settings2, RefreshCw, Stethoscope, X } from "lucide-react";
import { type KeyboardEvent, type ReactNode, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Modal } from "../components/Modal";
import { SettingGroup } from "../components/SettingsUi";
import { matchesQuery } from "../lib/searchText";
import { SETTINGS_NAV_GROUPS } from "../lib/settingsNav";
import { aiCards } from "./settings/AiCards";
import { appearanceCards } from "./settings/AppearanceCards";
import { computerCards } from "./settings/ComputerCards";
import { generalCards } from "./settings/GeneralCards";
import { mailCards } from "./settings/MailCards";
import { otherCards } from "./settings/OtherCards";
import {
  type CardDef,
  SETTINGS_SECTIONS,
  type SettingsBridge,
  type SettingsSectionId
} from "./settings/types";
import styles from "./SettingsModal.module.css";

const SECTION_ICONS = { general: Settings2, appearance: Palette, ai: BrainCircuit, computer: Monitor, mail: Mail, usage: ChartNoAxesCombined, diagnostics: Stethoscope, updates: RefreshCw };


/** Consecutive `row` cards share one rounded group; `block` cards stand alone. */
function renderCards(cards: CardDef[], groupLabel: (key: string) => string): ReactNode[] {
  const out: ReactNode[] = [];
  let run: CardDef[] = [];
  const flush = () => {
    if (run.length > 0) {
      const first = run[0].id;
      const group = first === "account" ? "account" : first === "language" ? "behavior" : null;
      if (group) out.push(<h3 key={`label-${first}`} className={styles.sectionLabel}>{groupLabel(group)}</h3>);
      out.push(
        <SettingGroup key={`group-${first}`}>
          {run.map((c) => (
            <div key={c.id} className={styles.cardSlot}>
              {c.node}
            </div>
          ))}
        </SettingGroup>
      );
      run = [];
    }
  };
  for (const c of cards) {
    if (c.id === "language") flush();
    if (c.kind === "row") {
      run.push(c);
    } else {
      flush();
      out.push(
        <div key={c.id} className={styles.blockSlot}>
          {c.node}
        </div>
      );
    }
  }
  flush();
  return out;
}

/**
 * Settings as a modal: left navigation, a search box that filters every
 * setting, and the cards of the chosen section on the right.
 */
export function SettingsModal({
  open,
  initialSection = "general",
  onClose,
  bridge
}: {
  open: boolean;
  initialSection?: SettingsSectionId;
  onClose: () => void;
  bridge: SettingsBridge;
}) {
  const { t } = useTranslation();
  const [section, setSection] = useState<SettingsSectionId>(initialSection);
  const [query, setQuery] = useState("");

  useEffect(() => {
    if (open) {
      setSection(initialSection);
      setQuery("");
    }
  }, [open, initialSection]);

  const cards = useMemo(
    () => [
      ...generalCards(bridge, t),
      ...appearanceCards(bridge, t),
      ...aiCards(t),
      ...computerCards(bridge, t),
      ...mailCards(t),
      ...otherCards(t)
    ],
    // `bridge` changes on every settings update: cards must follow it.
    [bridge, t]
  );

  const matches = (c: CardDef) =>
    matchesQuery(
      query,
      c.title,
      c.description,
      c.keywords,
      t(`settingsModal.nav.${c.section}`)
    );
  const searching = query.trim() !== "";
  const visible = cards.filter((c) =>
    searching ? matches(c) : c.section === section
  );
  const sectionsWithHits = new Set(
    cards.filter(matches).map((c) => c.section)
  );

  const groups = SETTINGS_SECTIONS.filter((s) =>
    visible.some((c) => c.section === s)
  );
  const enabledSections = SETTINGS_NAV_GROUPS.flatMap((g) => g.sections).filter(
    (s) => !(searching && !sectionsWithHits.has(s))
  );
  const onNavKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (
      e.key !== "ArrowDown" &&
      e.key !== "ArrowUp" &&
      e.key !== "Home" &&
      e.key !== "End"
    ) {
      return;
    }
    if (enabledSections.length === 0) {
      return;
    }
    e.preventDefault();
    const cur = Math.max(0, enabledSections.indexOf(section));
    const nextIdx =
      e.key === "Home"
        ? 0
        : e.key === "End"
          ? enabledSections.length - 1
          : e.key === "ArrowDown"
            ? (cur + 1) % enabledSections.length
            : (cur - 1 + enabledSections.length) % enabledSections.length;
    const next = enabledSections[nextIdx];
    setQuery("");
    setSection(next);
    e.currentTarget
      .querySelector<HTMLElement>(`[data-testid="settings-nav-${next}"]`)
      ?.focus();
  };

  return (
    <Modal open={open} onClose={onClose} label={t("settingsModal.title")}>
      <nav className={styles.nav} aria-label={t("settingsModal.title")}>
        <div className={styles.navHeading}><Settings2 />{t("settingsModal.title")}</div>
        <div className={styles.searchWrap}><Search /><input
          className={styles.search}
          type="search"
          data-autofocus
          value={query}
          placeholder={t("settingsModal.search")}
          aria-label={t("settingsModal.search")}
          onChange={(e) => setQuery(e.target.value)}
        />
        </div>
        <div className={styles.navScroll} onKeyDown={onNavKeyDown}>
          {SETTINGS_NAV_GROUPS.map((g) => (
            <div key={g.id} className={styles.navList} role="group" aria-label={t(`settingsModal.navGroups.${g.id}`)}>
              <span className={styles.navGroupLabel} aria-hidden="true">{t(`settingsModal.navGroups.${g.id}`)}</span>
              {g.sections.map((s) => {
                const SectionIcon = SECTION_ICONS[s];
                const dim = searching && !sectionsWithHits.has(s);
                return (
                  <button
                    key={s}
                    type="button"
                    data-testid={`settings-nav-${s}`}
                    disabled={dim}
                    aria-current={!searching && s === section ? "page" : undefined}
                    className={[styles.navItem, !searching && s === section ? styles.navActive : ""].filter(Boolean).join(" ")}
                    onClick={() => {
                      setQuery("");
                      setSection(s);
                    }}
                  >
                    <SectionIcon />{t(`settingsModal.nav.${s}`)}
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
              {searching
                ? t("settingsModal.results")
                : t(`settingsModal.nav.${section}`)}
            </h2>
            {searching ? null : <p className={styles.subtitle}>{t(`settingsModal.sectionDesc.${section}`)}</p>}
          </div>
          <button
            type="button"
            className={styles.close}
            title={t("common.close")}
            aria-label={t("common.close")}
            onClick={onClose}
          >
            <X />
          </button>
        </header>
        <div className={styles.scroll}>
          {visible.length === 0 ? (
            <p className={styles.empty}><Search />{t("settingsModal.noResults")}</p>
          ) : (
            groups.map((s) => (
              <section key={s} className={styles.section}>
                {searching ? (
                  <h3 className={styles.sectionLabel}>
                    {t(`settingsModal.nav.${s}`)}
                  </h3>
                ) : null}
                {renderCards(visible.filter((c) => c.section === s), key => t(`settingsModal.groups.${key}`))}
              </section>
            ))
          )}
        </div>
      </div>
    </Modal>
  );
}
