import { ChevronsUpDown, CircleHelp, Grid2X2, Monitor, Settings2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ComputerTarget } from "../lib/settingsPrefs";
import styles from "./AccountMenu.module.css";

const ICONS = { settings: Settings2, apps: Grid2X2, computer: Monitor, help: CircleHelp };


export type AccountAction = "settings" | "apps" | "computer" | "help";

/** Bottom-left account button with a pop-up menu (settings, apps, computer, help). */
export function AccountMenu({
  name,
  computerTarget,
  onAction
}: {
  name: string;
  computerTarget: ComputerTarget;
  onAction: (action: AccountAction) => void;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement | null>(null);
  const label = name.trim() || t("account.defaultName");

  useEffect(() => {
    if (!open) {
      return undefined;
    }
    root.current?.querySelector<HTMLButtonElement>('[role="menuitem"]')?.focus();
    const onDown = (e: MouseEvent) => {
      if (root.current && !root.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        setOpen(false);
        root.current?.querySelector<HTMLButtonElement>('[aria-haspopup="menu"]')?.focus();
      } else if (["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) {
        e.preventDefault();
        const items = [...(root.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]') ?? [])];
        const index = items.indexOf(document.activeElement as HTMLButtonElement);
        const next = e.key === "Home" ? 0 : e.key === "End" ? items.length - 1 : (index + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
        items[next]?.focus();
      }
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const items: ReadonlyArray<{ id: AccountAction; text: string; hint?: string }> = [
    { id: "settings", text: t("account.settings") },
    { id: "apps", text: t("account.apps") },
    {
      id: "computer",
      text: t("account.computer"),
      hint: t(`settingsModal.computer.targets.${computerTarget}.title`)
    },
    { id: "help", text: t("account.help") }
  ];

  return (
    <div className={styles.root} ref={root}>
      {open ? (
        <div className={styles.menu} role="menu" aria-label={label}>
          {items.map((i) => { const ItemIcon = ICONS[i.id]; return (
            <button
              key={i.id}
              type="button"
              role="menuitem"
              className={styles.item}
              data-testid={`account-${i.id}`}
              onClick={() => {
                setOpen(false);
                onAction(i.id);
              }}
            >
              <ItemIcon /><span>{i.text}</span>
              {i.hint ? <span className={styles.hint}>{i.hint}</span> : null}
            </button>
          ); })}
        </div>
      ) : null}
      <button
        type="button"
        className={styles.trigger}
        aria-haspopup="menu"
        aria-expanded={open}
        data-testid="account-menu"
        onClick={() => setOpen((o) => !o)}
        onKeyDown={(event) => { if (event.key === "ArrowDown" || event.key === "ArrowUp") { event.preventDefault(); setOpen(true); } }}
      >
        <span className={styles.avatar} aria-hidden="true">
          {label.slice(0, 1).toUpperCase()}
        </span>
        <span className={styles.name}>{label}</span><ChevronsUpDown />
      </button>
    </div>
  );
}
