import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { DEFAULT_STOP_HOTKEY } from "../config/product";
import { Modal } from "./Modal";
import styles from "./HelpModal.module.css";

/** Quick help: the shortcuts that matter and a way back into the setup wizard. */
export function HelpModal({
  open,
  onClose,
  stopHotkey,
  sendKeys,
  onSetup
}: {
  open: boolean;
  onClose: () => void;
  stopHotkey: string;
  sendKeys: string;
  onSetup: () => void;
}) {
  const { t } = useTranslation();
  const rows: ReadonlyArray<[string, string]> = [
    [t("help.palette"), "Ctrl+K"],
    [t("help.send"), sendKeys],
    [t("help.stop"), stopHotkey || DEFAULT_STOP_HOTKEY]
  ];
  return (
    <Modal open={open} onClose={onClose} label={t("help.title")} size="md">
      <div className={styles.root}>
        <h2 className={styles.title}>{t("help.title")}</h2>
        <p className={styles.text}>{t("help.intro")}</p>
        <ul className={styles.list}>
          {rows.map(([label, keys]) => (
            <li key={label} className={styles.row}>
              <span>{label}</span>
              <kbd className={styles.kbd}>{keys}</kbd>
            </li>
          ))}
        </ul>
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
