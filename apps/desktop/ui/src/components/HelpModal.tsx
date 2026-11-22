import { ExternalLink, Search } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { DEFAULT_STOP_HOTKEY } from "../config/product";
import { DOCS_INDEX_URL, docUrl, faqFor, searchFaq } from "../lib/helpFaq";
import { openExternal } from "../lib/externalLink";
import { resolveKeys, SHORTCUT_GROUPS, SHORTCUTS, slashShortcuts } from "../lib/shortcuts";
import { Modal } from "./Modal";
import styles from "./HelpModal.module.css";

/**
 * Help: short answers to the questions a beginner asks first (searchable, in the app's
 * language), a link to the full guide, the keyboard shortcuts folded away below, and a way
 * back into the setup wizard.
 */
export function HelpModal({
  open,
  onClose,
  stopHotkey,
  sendWith,
  onSetup
}: {
  open: boolean;
  onClose: () => void;
  stopHotkey: string;
  sendWith: "enter" | "ctrl-enter";
  onSetup: () => void;
}) {
  const { t, i18n } = useTranslation();
  const [query, setQuery] = useState("");
  const ctx = { stopHotkey: stopHotkey || DEFAULT_STOP_HOTKEY, sendWith };
  const items = useMemo(() => searchFaq(faqFor(i18n.language), query), [i18n.language, query]);
  return (
    <Modal open={open} onClose={onClose} label={t("help.center.title")} size="md">
      <div className={styles.root} data-testid="help-center">
        <h2 className={styles.title}>{t("help.center.title")}</h2>
        <p className={styles.text}>{t("help.center.intro")}</p>
        <label className={styles.search}>
          <Search aria-hidden="true" />
          <input
            type="search"
            className={styles.searchInput}
            value={query}
            placeholder={t("help.center.searchPlaceholder")}
            aria-label={t("help.center.searchLabel")}
            data-autofocus
            data-testid="help-search"
            onChange={(e) => setQuery(e.target.value)}
          />
        </label>
        {items.length === 0 ? (
          <p className={styles.text} role="status" data-testid="help-no-match">
            {t("help.center.noMatch")}
          </p>
        ) : (
          <div className={styles.faq} role="list">
            {items.map((item) => (
              <details key={item.id} className={styles.answer} role="listitem" open={query.trim() !== "" && items.length <= 3}>
                <summary className={styles.question}>{item.q}</summary>
                <p className={styles.text}>{item.a}</p>
                <button type="button" className={styles.more} onClick={() => openExternal(docUrl(item))}>
                  {t("help.center.readMore")}
                  <ExternalLink aria-hidden="true" />
                </button>
              </details>
            ))}
          </div>
        )}
        <details className={styles.answer}>
          <summary className={styles.question}>{t("help.title")}</summary>
          <p className={styles.text}>{t("help.intro")}</p>
          {SHORTCUT_GROUPS.map((group) => (
            <section key={group} className={styles.group}>
              <h3 className={styles.groupTitle}>{t(`help.group.${group}`)}</h3>
              <ul className={styles.list}>
                {(group === "slash" ? slashShortcuts() : SHORTCUTS.filter((s) => s.group === group)).map((s) => (
                  <li key={s.id} className={styles.row}>
                    <span>{t(s.labelKey)}</span>
                    <kbd className={styles.kbd}>{resolveKeys(s.keys, ctx)}</kbd>
                  </li>
                ))}
              </ul>
            </section>
          ))}
        </details>
        <div className={styles.actions}>
          <Button variant="ghost" size="sm" onClick={() => openExternal(DOCS_INDEX_URL)}>
            {t("help.center.allDocs")}
            <ExternalLink aria-hidden="true" />
          </Button>
          <Button variant="secondary" size="sm" onClick={onSetup}>
            {t("main.runSetupWizard")}
          </Button>
          <Button variant="primary" size="sm" onClick={onClose}>
            {t("common.close")}
          </Button>
        </div>
      </div>
    </Modal>
  );
}
