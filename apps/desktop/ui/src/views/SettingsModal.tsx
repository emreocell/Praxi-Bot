import { BrainCircuit, ChartNoAxesCombined, Monitor, Search, Settings2, RefreshCw, X } from "lucide-react";
import { type ReactNode, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Modal } from "../components/Modal";
import { SettingGroup } from "../components/SettingsUi";
import { matchesQuery } from "../lib/searchText";
import { aiCards } from "./settings/AiCards";
import { computerCards } from "./settings/ComputerCards";
import { generalCards } from "./settings/GeneralCards";
import { otherCards } from "./settings/OtherCards";
import {
  type CardDef,
  SETTINGS_SECTIONS,
  type SettingsBridge,
  type SettingsSectionId
} from "./settings/types";
import styles from "./SettingsModal.module.css";

const SECTION_ICONS = { general: Settings2, ai: BrainCircuit, computer: Monitor, usage: ChartNoAxesCombined, updates: RefreshCw };


/** Consecutive `row` cards share one rounded group; `block` cards stand alone. */
function renderCards(cards: CardDef[]): ReactNode[] {
  const out: ReactNode[] = [];
  let run: CardDef[] = [];
  const flush = () => {
    if (run.length > 0) {
      const first = run[0].id;
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
      ...aiCards(t),
      ...computerCards(bridge, t),
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
        </div><div className={styles.navList}>
          {SETTINGS_SECTIONS.map((s) => {
            const SectionIcon = SECTION_ICONS[s];
            const dim = searching && !sectionsWithHits.has(s);
            return (
              <button
                key={s}
                type="button"
                data-testid={`settings-nav-${s}`}
                disabled={dim}
                aria-current={!searching && s === section ? "page" : undefined}
                className={[
                  styles.navItem,
                  !searching && s === section ? styles.navActive : ""
                ]
                  .filter(Boolean)
                  .join(" ")}
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
      </nav>
      <div className={styles.content}>
        <header className={styles.header}>
          <h2 className={styles.title}>
            {searching
              ? t("settingsModal.results")
              : t(`settingsModal.nav.${section}`)}
          </h2>
          <button
            type="button"
            className={styles.close}
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
                {renderCards(visible.filter((c) => c.section === s))}
              </section>
            ))
          )}
        </div>
      </div>
    </Modal>
  );
}
