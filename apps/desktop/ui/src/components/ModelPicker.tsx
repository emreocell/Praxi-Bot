import { Check, ChevronDown, Search } from "lucide-react";
import { useCallback, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { isMockMode } from "../api/mockMode";
import { filterModelGroups, type ModelGroup } from "../lib/modelPicker";
import { productBrandKey, providerBrand } from "../lib/modelBrand";
import { THINKING_LEVELS, useThinkingLevel } from "../lib/thinkingLevel";
import { BrandMark } from "./BrandMark";
import styles from "./ModelPicker.module.css";
import { Popover, type PopoverPlacement } from "./Popover";

/**
 * Model popover, grouped by provider: the company logo heads each group, the product logo
 * sits on every row, and (when `thinking`) a thinking-level segment closes the list.
 * `value` and the picked option use the `provider/model` form.
 */
export function ModelPicker({
  groups,
  value,
  onPick,
  disabled = false,
  placeholder,
  reviewOpen = true,
  thinking = false,
  placement = "bottom-end"
}: {
  groups: ModelGroup[];
  value: string;
  onPick: (ref: string) => void;
  disabled?: boolean;
  placeholder?: string;
  reviewOpen?: boolean;
  /** Show the thinking-level segment (stored on this device, not yet sent to the daemon). */
  thinking?: boolean;
  placement?: PopoverPlacement;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(
    () => reviewOpen && isMockMode() && new URLSearchParams(window.location.search).get("screen") === "models"
  );
  const [query, setQuery] = useState("");
  const [level, setLevel] = useThinkingLevel();
  const trigger = useRef<HTMLButtonElement | null>(null);
  const shown = useMemo(
    () =>
      filterModelGroups(groups, query).sort((a, b) => {
        const order = [groups[0]?.provider, "openai", "gemini", "ollama", "claude-code"];
        const priority = (id: string) => (order.includes(id) ? order.indexOf(id) : order.length);
        return priority(a.provider) - priority(b.provider);
      }),
    [groups, query]
  );

  const close = useCallback(() => {
    setOpen(false);
    setQuery("");
  }, []);

  const pick = (ref: string) => {
    close();
    trigger.current?.focus();
    if (ref !== value) {
      onPick(ref);
    }
  };

  const [valueProvider = "", ...rest] = value.split("/");
  const valueModel = rest.join("/");

  return (
    <div className={styles.root}>
      <button
        ref={trigger}
        type="button"
        className={styles.trigger}
        disabled={disabled}
        aria-haspopup="dialog"
        aria-expanded={open}
        title={t("modelPicker.title")}
        data-testid="model-picker"
        onClick={() => (open ? close() : setOpen(true))}
      >
        {value ? <BrandMark name={productBrandKey(valueProvider, valueModel)} size={15} /> : null}
        <span className={styles.triggerText}>{valueModel || placeholder || t("modelPicker.none")}</span>
        <ChevronDown />
      </button>
      <Popover
        open={open}
        anchor={trigger}
        onClose={close}
        placement={placement}
        role="dialog"
        label={t("modelPicker.title")}
        width={320}
        initialFocus="none"
      >
        <label className={styles.search}>
          <Search aria-hidden="true" />
          <input
            value={query}
            placeholder={t("modelPicker.search")}
            aria-label={t("modelPicker.search")}
            autoFocus
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                const first = shown[0]?.models[0];
                if (first) {
                  e.preventDefault();
                  pick(first.ref);
                }
              }
            }}
          />
        </label>
        <div className={styles.scroll} role="listbox" aria-label={t("modelPicker.title")}>
          {shown.length === 0 ? (
            <p className={styles.empty}>{t("modelPicker.empty")}</p>
          ) : (
            shown.map((g) => {
              const brand = providerBrand(g.provider);
              return (
                <div key={g.provider} className={styles.group} role="group" aria-label={`${brand.label}${brand.note ? ` ${brand.note}` : ""}`}>
                  <div className={styles.groupLabel} aria-hidden="true">
                    <BrandMark name={brand.company} size={14} />
                    <span>{brand.label}</span>
                    {brand.note ? <span className={styles.groupNote}>{brand.note}</span> : null}
                  </div>
                  {g.models.map((m) => {
                    const active = m.ref === value;
                    return (
                      <button
                        key={m.ref}
                        type="button"
                        role="option"
                        aria-selected={active}
                        className={[styles.option, active ? styles.optionActive : ""].filter(Boolean).join(" ")}
                        onClick={() => pick(m.ref)}
                      >
                        <BrandMark name={productBrandKey(m.provider, m.model)} size={16} />
                        <span className={styles.optionText}>{m.model}</span>
                        {active ? <Check aria-hidden="true" /> : null}
                      </button>
                    );
                  })}
                </div>
              );
            })
          )}
        </div>
        {thinking ? (
          <div className={styles.effort}>
            <span className={styles.effortLabel} id="thinking-level-label">
              {t("modelPicker.thinking")}
            </span>
            <div className={styles.seg} role="radiogroup" aria-labelledby="thinking-level-label">
              {THINKING_LEVELS.map((l) => (
                <button
                  key={l}
                  type="button"
                  role="radio"
                  aria-checked={l === level}
                  className={l === level ? styles.segOn : undefined}
                  onClick={() => setLevel(l)}
                >
                  {t(`modelPicker.levels.${l}`)}
                </button>
              ))}
            </div>
          </div>
        ) : null}
      </Popover>
    </div>
  );
}
