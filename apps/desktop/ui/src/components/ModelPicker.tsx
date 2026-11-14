import { Check, ChevronDown, Search, Sparkles } from "lucide-react";
import { useCallback, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { isMockMode } from "../api/mockMode";
import { filterModelGroups, type ModelGroup } from "../lib/modelPicker";
import { productBrandKey, providerBrand } from "../lib/modelBrand";
import { matchesQuery } from "../lib/searchText";
import { THINKING_LEVELS, useThinkingLevel } from "../lib/thinkingLevel";
import { BrandMark } from "./BrandMark";
import styles from "./ModelPicker.module.css";
import { Popover, type PopoverPlacement } from "./Popover";

export interface ModelPickerAutoOption {
  label: string;
  sub?: string;
  selected: boolean;
  onPick: () => void;
}

/**
 * Model popover, grouped by provider: the company logo heads each group, the product logo
 * sits on every row, and (when `thinking`) a thinking-level segment closes the list.
 * `value` and the picked option use the `provider/model` form.
 */
export function ModelPicker({
  groups,
  value,
  onPick,
  autoOption,
  disabled = false,
  placeholder,
  reviewOpen = true,
  thinking = false,
  placement = "bottom-end",
  testId = "model-picker"
}: {
  groups: ModelGroup[];
  value: string;
  onPick: (ref: string) => void;
  autoOption?: ModelPickerAutoOption;
  disabled?: boolean;
  placeholder?: string;
  reviewOpen?: boolean;
  /** Show the thinking-level segment (stored on this device, not yet sent to the daemon). */
  thinking?: boolean;
  placement?: PopoverPlacement;
  testId?: string;
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
    if (autoOption?.selected || ref !== value) {
      onPick(ref);
    }
  };

  const pickAuto = () => {
    if (!autoOption) return;
    close();
    trigger.current?.focus();
    autoOption.onPick();
  };

  const showAuto = Boolean(
    autoOption && (!query.trim() || matchesQuery(query, autoOption.label, autoOption.sub))
  );

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
        data-testid={testId}
        onClick={() => (open ? close() : setOpen(true))}
      >
        {autoOption?.selected ? (
          <Sparkles aria-hidden="true" />
        ) : value ? (
          <BrandMark name={productBrandKey(valueProvider, valueModel)} size={15} />
        ) : null}
        <span className={styles.triggerText}>
          {autoOption?.selected
            ? autoOption.label
            : valueModel || placeholder || t("modelPicker.none")}
        </span>
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
                if (first && query.trim()) {
                  e.preventDefault();
                  pick(first.ref);
                } else if (showAuto && autoOption) {
                  e.preventDefault();
                  pickAuto();
                } else if (first) {
                  e.preventDefault();
                  pick(first.ref);
                }
              }
            }}
          />
        </label>
        <div className={styles.scroll} role="listbox" aria-label={t("modelPicker.title")}>
          {shown.length === 0 && !showAuto ? (
            <p className={styles.empty}>{t("modelPicker.empty")}</p>
          ) : (
            <>
              {showAuto && autoOption ? (
                <button
                  type="button"
                  role="option"
                  aria-selected={autoOption.selected}
                  className={[
                    styles.option,
                    autoOption.selected ? styles.optionActive : ""
                  ]
                    .filter(Boolean)
                    .join(" ")}
                  onClick={pickAuto}
                >
                  <Sparkles aria-hidden="true" />
                  <span className={styles.optionText}>{autoOption.label}</span>
                  {autoOption.sub ? (
                    <span className={styles.optionSub}>{autoOption.sub}</span>
                  ) : null}
                  {autoOption.selected ? <Check aria-hidden="true" /> : null}
                </button>
              ) : null}
              {shown.map((g) => {
                const brand = providerBrand(g.provider);
                return (
                  <div key={g.provider} className={styles.group} role="group" aria-label={`${brand.label}${brand.note ? ` ${brand.note}` : ""}`}>
                    <div className={styles.groupLabel} aria-hidden="true">
                      <BrandMark name={brand.company} size={14} />
                      <span>{brand.label}</span>
                      {brand.note ? <span className={styles.groupNote}>{brand.note}</span> : null}
                    </div>
                    {g.models.map((m) => {
                      const active = !autoOption?.selected && m.ref === value;
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
              })}
            </>
          )}
        </div>
        {thinking ? (
          <div className={styles.effort}>
            <span className={styles.effortLabel} id="thinking-level-label">
              {t("modelPicker.thinking")}
            </span>
            <div
              className={styles.seg}
              role="radiogroup"
              aria-labelledby="thinking-level-label"
              onKeyDown={(e) => {
                const step = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
                if (!step) return;
                e.preventDefault();
                const i = THINKING_LEVELS.indexOf(level);
                const next = THINKING_LEVELS[(i + step + THINKING_LEVELS.length) % THINKING_LEVELS.length];
                if (next) {
                  setLevel(next);
                  e.currentTarget.querySelectorAll<HTMLElement>('[role="radio"]')[THINKING_LEVELS.indexOf(next)]?.focus();
                }
              }}
            >
              {THINKING_LEVELS.map((l) => (
                <button
                  key={l}
                  type="button"
                  role="radio"
                  tabIndex={l === level ? 0 : -1}
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
