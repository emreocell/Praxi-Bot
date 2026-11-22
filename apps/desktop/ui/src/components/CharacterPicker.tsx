import { type CSSProperties, type KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import { BOT_SHAPES, BotAvatar, type BotShapeId } from "ui";
import { BOT_COLORS } from "../lib/botProfile";
import styles from "./CharacterPicker.module.css";

/** 8x2 grid picking one of the 16 3D clay silhouettes, previewed in the active tint. */
export function CharacterShapeGrid({
  value,
  color,
  onChange
}: {
  value: BotShapeId;
  color: string;
  onChange: (shape: BotShapeId) => void;
}) {
  const { t } = useTranslation();
  const activeIdx = Math.max(
    0,
    BOT_SHAPES.findIndex((s) => s.id === value)
  );
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const step =
      e.key === "ArrowRight" || e.key === "ArrowDown"
        ? 1
        : e.key === "ArrowLeft" || e.key === "ArrowUp"
          ? -1
          : 0;
    if (!step) {
      return;
    }
    e.preventDefault();
    const next = BOT_SHAPES[(activeIdx + step + BOT_SHAPES.length) % BOT_SHAPES.length];
    onChange(next.id);
    e.currentTarget.querySelector<HTMLElement>(`[data-testid="shape-${next.id}"]`)?.focus();
  };
  return (
    <div
      className={styles.shapeGrid}
      role="group"
      aria-label={t("newBot.shapeGroup")}
      onKeyDown={onKeyDown}
    >
      {BOT_SHAPES.map((s, idx) => {
        const label = t(`newBot.shapes.${s.id}`);
        const active = value === s.id;
        return (
          <button
            key={s.id}
            type="button"
            className={[styles.shapeButton, active ? styles.shapeActive : ""]
              .filter(Boolean)
              .join(" ")}
            title={label}
            aria-label={label}
            aria-pressed={active}
            tabIndex={idx === activeIdx ? 0 : -1}
            data-testid={`shape-${s.id}`}
            onClick={() => onChange(s.id)}
          >
            <BotAvatar
              bot={{ id: `pick-${s.id}`, name: label, shape: s.id, color }}
              size="sm"
              live={false}
              decorative
            />
          </button>
        );
      })}
    </div>
  );
}

/** Palette swatches plus a rainbow custom `<input type="color">` picker. */
export function CharacterColorRow({
  value,
  onChange,
  centered = true
}: {
  value: string;
  onChange: (color: string) => void;
  centered?: boolean;
}) {
  const { t } = useTranslation();
  const normalized = value.trim().toLowerCase();
  const presetIdx = BOT_COLORS.findIndex((c) => c.toLowerCase() === normalized);
  const isPreset = presetIdx >= 0;
  const activeIdx = isPreset ? presetIdx : 0;
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if ((e.target as HTMLElement | null)?.tagName === "INPUT") {
      return;
    }
    const step =
      e.key === "ArrowRight" || e.key === "ArrowDown"
        ? 1
        : e.key === "ArrowLeft" || e.key === "ArrowUp"
          ? -1
          : 0;
    if (!step) {
      return;
    }
    e.preventDefault();
    const next = BOT_COLORS[(activeIdx + step + BOT_COLORS.length) % BOT_COLORS.length];
    onChange(next);
    e.currentTarget.querySelector<HTMLElement>(`[data-testid="color-${next.slice(1)}"]`)?.focus();
  };
  return (
    <div
      className={styles.swatchRow}
      style={centered ? undefined : { justifyContent: "flex-start" }}
      role="group"
      aria-label={t("botForm.color")}
      onKeyDown={onKeyDown}
    >
      {BOT_COLORS.map((hex, idx) => {
        const active = normalized === hex.toLowerCase();
        return (
          <button
            key={hex}
            type="button"
            className={[styles.swatch, active ? styles.swatchActive : ""]
              .filter(Boolean)
              .join(" ")}
            style={{ "--swatch-color": hex } as CSSProperties}
            aria-label={t("botForm.colorSwatch", { hex })}
            title={t("botForm.colorSwatch", { hex })}
            aria-pressed={active}
            tabIndex={idx === activeIdx ? 0 : -1}
            data-testid={`color-${hex.slice(1)}`}
            onClick={() => onChange(hex)}
          />
        );
      })}
      <label
        className={[styles.customLabel, !isPreset ? styles.customActive : ""]
          .filter(Boolean)
          .join(" ")}
        title={t("botForm.colorCustom")}
      >
        {!isPreset ? (
          <span
            className={styles.customDot}
            style={{ "--swatch-color": value } as CSSProperties}
            aria-hidden="true"
          />
        ) : null}
        <input
          type="color"
          className={styles.customInput}
          value={normalized.startsWith("#") && normalized.length === 7 ? normalized : "#34c77b"}
          aria-label={t("botForm.colorCustom")}
          onChange={(e) => onChange(e.target.value)}
        />
      </label>
    </div>
  );
}
