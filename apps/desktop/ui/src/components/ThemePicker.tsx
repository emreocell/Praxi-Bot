import { Check, Monitor, Plus } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import {
  accentAfterThemeChange,
  Button,
  CUSTOM_THEME_ID,
  customThemeContrast,
  deriveTheme,
  FAVORITE_THEMES,
  PALETTES,
  paletteById,
  resolveTheme,
  type CustomTheme,
  type ThemeDef,
  type ThemeMode
} from "ui";
import { Modal } from "./Modal";
import styles from "./AppearancePicker.module.css";
import tileStyles from "./ThemePicker.module.css";

/** The palette a saved mode shows as selected (legacy names map to their palette). */
export function selectedThemeId(mode: ThemeMode): string {
  if (mode === "light") return "clay";
  if (mode === "dark") return "obsidian";
  return mode;
}

/** A tiny rendition of a theme: page, text lines, a raised card and the accent pill. */
export function ThemePreview({ theme }: { theme: ThemeDef }) {
  const d = deriveTheme(theme);
  return (
    <span
      className={`${styles.preview} ${tileStyles.pal}`}
      data-theme-preview={theme.id}
      style={{ background: `radial-gradient(130% 100% at 30% 0%, ${d.bg2}, ${d.bg} 70%)` }}
    >
      <i style={{ width: "70%", background: d.text }} />
      <i style={{ width: "45%", background: d.text, opacity: 0.4 }} />
      <i className={tileStyles.card} style={{ background: d.raised }}>
        <b style={{ background: d.accent ?? d.text }} />
      </i>
    </span>
  );
}

function ThemeTile({
  theme,
  selected,
  label,
  onPick
}: {
  theme: ThemeDef;
  selected: boolean;
  label: string;
  onPick: () => void;
}) {
  return (
    <button type="button" className={styles.tile} aria-pressed={selected} onClick={onPick}>
      <ThemePreview theme={theme} />
      <span className={styles.label}>
        {label}
        {selected ? <Check /> : null}
      </span>
    </button>
  );
}

/** "Your own theme": two colours; warns (never blocks) when the text is hard to read. */
export function CustomThemeEditor({
  custom,
  active,
  onChange
}: {
  custom: CustomTheme;
  active: boolean;
  onChange: (next: CustomTheme) => void;
}) {
  const { t } = useTranslation();
  const { ratio, ok } = customThemeContrast(custom);
  return (
    <div className={styles.customBox}>
      <div className={styles.customText}>
        <strong>{t("appearance.themes.customHint")}</strong>
        <span>{t("appearance.themes.customDetail")}</span>
        {active && !ok ? (
          <span className={styles.warn} role="status">
            {t("appearance.themes.lowContrast", { ratio: ratio.toFixed(1) })}
          </span>
        ) : null}
      </div>
      <label className={styles.colorPick}>
        <input
          type="color"
          value={custom.bg}
          onChange={(e) => onChange({ ...custom, bg: e.target.value })}
        />
        {t("appearance.themes.background")}
      </label>
      <label className={styles.colorPick}>
        <input
          type="color"
          value={custom.text}
          onChange={(e) => onChange({ ...custom, text: e.target.value })}
        />
        {t("appearance.themes.text")}
      </label>
    </div>
  );
}

export function ThemePicker({
  mode,
  custom,
  accent,
  onChange
}: {
  mode: ThemeMode;
  custom: CustomTheme;
  /** The saved accent preference (`""`, `"theme"` or a colour). */
  accent: string;
  /** The caller persists the patch. */
  onChange: (patch: { themeMode?: string; customTheme?: CustomTheme; accentColor?: string }) => void;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const selected = selectedThemeId(mode);
  const isFavorite = FAVORITE_THEMES.includes(selected);
  const isSystem = selected === "system";
  const shown = paletteById(selected) ?? resolveTheme(mode, custom);
  const pick = (id: string) => {
    const next: Pick<ThemeDef, "accent"> = id === CUSTOM_THEME_ID ? {} : (paletteById(id) as ThemeDef);
    onChange({ themeMode: id, accentColor: accentAfterThemeChange(accent, next) });
  };
  const editCustom = (next: CustomTheme) => {
    onChange({
      customTheme: next,
      themeMode: CUSTOM_THEME_ID,
      accentColor: accentAfterThemeChange(accent, {})
    });
  };
  const nameOf = (id: string) => t(`theme.names.${id}`);
  const dark = PALETTES.filter((p) => !deriveTheme(p).light);
  const light = PALETTES.filter((p) => deriveTheme(p).light);

  return (
    <div className={styles.root} role="group" aria-label={t("appearance.theme.title")}>
      <div className={styles.tiles}>
        {FAVORITE_THEMES.map((id) => (
          <ThemeTile
            key={id}
            theme={paletteById(id) as ThemeDef}
            selected={selected === id}
            label={nameOf(id)}
            onPick={() => pick(id)}
          />
        ))}
        <button
          type="button"
          className={styles.tile}
          aria-pressed={!isFavorite && !isSystem}
          aria-haspopup="dialog"
          onClick={() => setOpen(true)}
        >
          {!isFavorite && !isSystem ? (
            <ThemePreview theme={shown} />
          ) : (
            <span className={`${styles.preview} ${styles.more}`}>
              <Plus />
            </span>
          )}
          <span className={styles.label}>
            {isFavorite || isSystem ? t("appearance.theme.all") : nameOf(selected)}
          </span>
        </button>
      </div>
      <button
        type="button"
        className={tileStyles.system}
        aria-pressed={isSystem}
        onClick={() => onChange({ themeMode: "system", accentColor: accentAfterThemeChange(accent, {}) })}
      >
        <Monitor />
        <span>{t("appearance.theme.system")}</span>
        {isSystem ? <Check /> : null}
      </button>

      <Modal open={open} onClose={() => setOpen(false)} label={t("appearance.themes.modalTitle")} size="md">
        <div className={styles.gallery}>
          <div className={styles.galleryHead}>
            <h3>{t("appearance.themes.modalTitle")}</h3>
            <Button size="sm" variant="primary" onClick={() => setOpen(false)} data-autofocus>
              {t("appearance.themes.done")}
            </Button>
          </div>
          <div className={styles.galleryBody}>
            {([["dark", dark], ["light", light]] as const).map(([key, list]) => (
              <section key={key} className={styles.gallerySection}>
                <h4>{t(`appearance.themes.${key}`)}</h4>
                <div className={styles.tiles}>
                  {list.map((p) => (
                    <ThemeTile
                      key={p.id}
                      theme={p}
                      selected={selected === p.id}
                      label={nameOf(p.id)}
                      onPick={() => pick(p.id)}
                    />
                  ))}
                </div>
              </section>
            ))}
            <section className={styles.gallerySection}>
              <h4>{t("appearance.themes.custom")}</h4>
              <CustomThemeEditor
                custom={custom}
                active={selected === CUSTOM_THEME_ID}
                onChange={editCustom}
              />
            </section>
          </div>
        </div>
      </Modal>
    </div>
  );
}
