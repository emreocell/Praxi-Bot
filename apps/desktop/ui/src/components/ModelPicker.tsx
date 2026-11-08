import { BrainCircuit, Check, ChevronDown } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { filterModelGroups, type ModelGroup } from "../lib/modelPicker";
import styles from "./ModelPicker.module.css";

/**
 * Dropdown of models, grouped by provider, with a search box.  `value` and the
 * picked option use the `provider/model` form.
 */
export function ModelPicker({
  groups,
  value,
  onPick,
  disabled = false,
  placeholder
}: {
  groups: ModelGroup[];
  value: string;
  onPick: (ref: string) => void;
  disabled?: boolean;
  placeholder?: string;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const root = useRef<HTMLDivElement | null>(null);
  const shown = useMemo(() => filterModelGroups(groups, query), [groups, query]);

  useEffect(() => {
    if (!open) {
      return undefined;
    }
    const onDown = (e: MouseEvent) => {
      if (root.current && !root.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        setOpen(false);
        root.current?.querySelector<HTMLButtonElement>('[aria-haspopup="listbox"]')?.focus();
      } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const items = [...(root.current?.querySelectorAll<HTMLButtonElement>('[role="option"]') ?? [])];
        const index = items.indexOf(document.activeElement as HTMLButtonElement);
        items[(index + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length]?.focus();
      }
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey, true);
    };
  }, [open]);

  const pick = (ref: string) => {
    setOpen(false);
    setQuery("");
    root.current?.querySelector<HTMLButtonElement>('[aria-haspopup="listbox"]')?.focus();
    if (ref !== value) {
      onPick(ref);
    }
  };

  return (
    <div className={styles.root} ref={root}>
      <button
        type="button"
        className={styles.trigger}
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        title={t("modelPicker.title")}
        data-testid="model-picker"
        onClick={() => setOpen((o) => !o)}
      >
        <BrainCircuit /><span className={styles.triggerText}>
          {value.split("/").pop() || placeholder || t("modelPicker.none")}
        </span>
        <ChevronDown />
      </button>
      {open ? (
        <div className={styles.popover}>
          <input
            className={styles.search}
            value={query}
            placeholder={t("modelPicker.search")}
            aria-label={t("modelPicker.search")}
            autoFocus
            onChange={(e) => setQuery(e.target.value)}
          />
          <div className={styles.scroll} role="listbox" aria-label={t("modelPicker.title")}>
            {shown.length === 0 ? (
              <p className={styles.empty}>{t("modelPicker.empty")}</p>
            ) : (
              shown.map((g) => (
                <div key={g.provider} className={styles.group} role="group" aria-label={g.provider}>
                  <div className={styles.groupLabel}>{g.provider}</div>
                  {g.models.map((m) => (
                    <button
                      key={m.ref}
                      type="button"
                      role="option"
                      aria-selected={m.ref === value}
                      className={[
                        styles.option,
                        m.ref === value ? styles.optionActive : ""
                      ]
                        .filter(Boolean)
                        .join(" ")}
                      onClick={() => pick(m.ref)}
                    >
                      <span className={styles.optionText}>{m.model}</span>
                      {m.ref === value ? (
                        <Check />
                      ) : null}
                    </button>
                  ))}
                </div>
              ))
            )}
          </div>
        </div>
      ) : null}
    </div>
  );
}
