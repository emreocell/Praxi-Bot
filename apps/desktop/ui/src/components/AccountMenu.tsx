import { CalendarClock, CircleHelp, Download, Gauge, Palette, Settings2, Sparkles, type LucideIcon } from "lucide-react";
import { useCallback, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ComputerTarget } from "../lib/settingsPrefs";
import styles from "./AccountMenu.module.css";
import { Popover } from "./Popover";

export type AccountAction =
  | "appearance"
  | "ai"
  | "usage"
  | "settings"
  | "schedules"
  | "updates"
  | "help"
  | "apps"
  | "computer";

const QUICK: ReadonlyArray<{ id: AccountAction; icon: LucideIcon; key: string }> = [
  { id: "appearance", icon: Palette, key: "account.appearance" },
  { id: "ai", icon: Sparkles, key: "account.ai" },
  { id: "usage", icon: Gauge, key: "account.usage" }
];

const ROWS: ReadonlyArray<{ id: AccountAction; icon: LucideIcon; key: string; kbd?: string }> = [
  { id: "settings", icon: Settings2, key: "account.settings", kbd: "account.shortcutSettings" },
  { id: "schedules", icon: CalendarClock, key: "sidebar.schedules" },
  { id: "updates", icon: Download, key: "account.updates" },
  { id: "help", icon: CircleHelp, key: "account.help" }
];

/**
 * Bottom-left profile button (avatar and name) with its menu: three quick tiles, then
 * settings, scheduled tasks, updates and help.  The menu is a portal popover, so it may reach
 * outside a narrow rail.  `compact` = icon-only rail.
 */
export function AccountMenu({
  name,
  computerTarget,
  compact = false,
  onAction
}: {
  name: string;
  computerTarget: ComputerTarget;
  compact?: boolean;
  onAction: (action: AccountAction) => void;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement | null>(null);
  const label = name.trim() || t("account.defaultName");
  const initial = label.slice(0, 1).toUpperCase();
  const close = useCallback(() => setOpen(false), []);
  const run = (id: AccountAction) => {
    setOpen(false);
    onAction(id);
  };

  return (
    <>
      <button
        ref={trigger}
        type="button"
        className={styles.trigger}
        aria-haspopup="menu"
        aria-expanded={open}
        title={label}
        aria-label={label}
        data-testid="account-menu"
        onClick={() => setOpen((o) => !o)}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown" || event.key === "ArrowUp") {
            event.preventDefault();
            setOpen(true);
          }
        }}
      >
        <span className={styles.avatar} aria-hidden="true">
          {initial}
        </span>
        {compact ? null : <span className={styles.name}>{label}</span>}
      </button>
      <Popover open={open} anchor={trigger} onClose={close} placement="top-start" label={label} width={272} className={styles.menu}>
        <div className={styles.head}>
          <span className={`${styles.avatar} ${styles.avatarLg}`} aria-hidden="true">
            {initial}
          </span>
          <div className={styles.headText}>
            <b>{label}</b>
            <small>{t(`settingsModal.computer.targets.${computerTarget}.title`)}</small>
          </div>
        </div>
        <div className={styles.quick}>
          {QUICK.map(({ id, icon: Icon, key }) => (
            <button key={id} type="button" role="menuitem" className={styles.tile} data-testid={`account-${id}`} onClick={() => run(id)}>
              <Icon aria-hidden="true" />
              <span>{t(key)}</span>
            </button>
          ))}
        </div>
        {ROWS.map(({ id, icon: Icon, key, kbd }) => (
          <button key={id} type="button" role="menuitem" className={styles.item} data-testid={`account-${id}`} onClick={() => run(id)}>
            <Icon aria-hidden="true" />
            <span>{t(key)}</span>
            {kbd ? <kbd>{t(kbd)}</kbd> : null}
          </button>
        ))}
      </Popover>
    </>
  );
}
