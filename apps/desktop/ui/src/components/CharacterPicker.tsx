import { type CSSProperties } from "react";
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
  return (
    <div className={styles.shapeGrid} role="group" aria-label={t("newBot.shapeGroup")}>
      {BOT_SHAPES.map((s) => {
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
  const isPreset = BOT_COLORS.some((c) => c.toLowerCase() === normalized);
  return (
    <div
      className={styles.swatchRow}
      style={centered ? undefined : { justifyContent: "flex-start" }}
      role="group"
      aria-label={t("botForm.color")}
    >
      {BOT_COLORS.map((hex) => {
        const active = normalized === hex.toLowerCase();
        return (
          <button
            key={hex}
            type="button"
            className={[styles.swatch, active ? styles.swatchActive : ""]
              .filter(Boolean)
              .join(" ")}
            style={{ "--swatch-color": hex } as CSSProperties}
            aria-label={hex}
            aria-pressed={active}
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
