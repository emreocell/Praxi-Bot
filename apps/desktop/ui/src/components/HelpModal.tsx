import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { DEFAULT_STOP_HOTKEY } from "../config/product";
import { resolveKeys, SHORTCUT_GROUPS, SHORTCUTS } from "../lib/shortcuts";
import { Modal } from "./Modal";
import styles from "./HelpModal.module.css";

/** Quick help: every shortcut in `lib/shortcuts.ts`, grouped, and a way back into the setup wizard. */
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
  const { t } = useTranslation();
  const ctx = { stopHotkey: stopHotkey || DEFAULT_STOP_HOTKEY, sendWith };
  return (
    <Modal open={open} onClose={onClose} label={t("help.title")} size="md">
      <div className={styles.root}>
        <h2 className={styles.title}>{t("help.title")}</h2>
        <p className={styles.text}>{t("help.intro")}</p>
        {SHORTCUT_GROUPS.map((group) => (
          <section key={group} className={styles.group}>
            <h3 className={styles.groupTitle}>{t(`help.group.${group}`)}</h3>
            <ul className={styles.list}>
              {SHORTCUTS.filter((s) => s.group === group).map((s) => (
                <li key={s.id} className={styles.row}>
                  <span>{t(s.labelKey)}</span>
                  <kbd className={styles.kbd}>{resolveKeys(s.keys, ctx)}</kbd>
                </li>
              ))}
            </ul>
          </section>
        ))}
        <div className={styles.actions}>
          <Button variant="secondary" size="sm" onClick={onSetup}>
            {t("main.runSetupWizard")}
          </Button>
          <Button variant="primary" size="sm" data-autofocus onClick={onClose}>
            {t("common.close")}
          </Button>
        </div>
      </div>
    </Modal>
  );
}
