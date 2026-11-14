import { Bot as BotIcon, Check } from "lucide-react";
import { useRef, type CSSProperties } from "react";
import type { TFunction } from "i18next";
import {
  accentFor,
  accentKind,
  BUBBLE_STYLES,
  Button,
  CORNERS,
  OWN_BUBBLES,
  paletteById,
  resolveTheme,
  UI_STYLES,
  type BubbleStyle,
  type UiStyle
} from "ui";
import { sceneColors } from "../../components/Backdrop";
import { BackdropPicker } from "../../components/BackdropPicker";
import formStyles from "../../components/Form.module.css";
import pickerStyles from "../../components/AppearancePicker.module.css";
import { Segmented, SettingRow, Switch } from "../../components/SettingsUi";
import { selectedThemeId, ThemePicker } from "../../components/ThemePicker";
import { CUSTOM_BACKDROP, NO_BACKDROP } from "../../lib/backdropCatalog";
import { ACCENT_PRESETS, BLUR_PRESETS, DIM_PRESETS } from "../../config/product";
import styles from "../SettingsModal.module.css";
import type { CardDef, SettingsBridge } from "./types";

/** The existing background image / GIF override (kept as is). */
function BackgroundControls({ b, t }: { b: SettingsBridge; t: TFunction }) {
  const fileRef = useRef<HTMLInputElement | null>(null);
  const { settings, update } = b;
  const onFile = (file: File | undefined) => {
    if (!file) {
      return;
    }
    const reader = new FileReader();
    reader.onload = () => {
      const result = typeof reader.result === "string" ? reader.result : "";
      if (result) {
        update({ backgroundUrl: result, backgroundFileName: file.name });
      }
    };
    reader.readAsDataURL(file);
  };
  return (
    <div className={styles.stack}>
      <input
        ref={fileRef}
        type="file"
        className={styles.hidden}
        accept="image/png,image/jpeg,image/webp,image/gif,image/svg+xml"
        onChange={(e) => {
          onFile(e.target.files?.[0]);
          e.target.value = "";
        }}
      />
      <div className={styles.inline}>
        <Button size="sm" variant="secondary" onClick={() => fileRef.current?.click()}>
          {t("settings.background.chooseFile")}
        </Button>
        {settings.backgroundUrl ? (
          <Button
            size="sm"
            variant="ghost"
            onClick={() =>
              update({
                backgroundUrl: "",
                backgroundFileName: "",
                // The own-image backdrop has nothing to show any more.
                ...(settings.backdrop === CUSTOM_BACKDROP ? { backdrop: NO_BACKDROP } : {})
              })
            }
          >
            {t("settings.background.clearFile")}
          </Button>
        ) : null}
        <span className={formStyles.hint}>
          {settings.backgroundFileName || t("settings.background.noFile")}
        </span>
      </div>
      <div className={styles.inline}>
        <span className={formStyles.hint}>{t("settings.background.blur")}</span>
        {BLUR_PRESETS.map((p) => (
          <Button
            key={p.id}
            size="sm"
            variant="secondary"
            active={settings.backgroundBlur === p.value}
            onClick={() => update({ backgroundBlur: p.value })}
          >
            {t(`settings.background.blurLevels.${p.id}`)}
          </Button>
        ))}
      </div>
      <div className={styles.inline}>
        <span className={formStyles.hint}>{t("settings.background.dim")}</span>
        {DIM_PRESETS.map((p) => (
          <Button
            key={p.id}
            size="sm"
            variant="secondary"
            active={settings.backgroundDim === p.value}
            onClick={() => update({ backgroundDim: p.value })}
          >
            {t(`settings.background.dimLevels.${p.id}`)}
          </Button>
        ))}
      </div>
    </div>
  );
}

const SAMPLE_CLASS: Record<UiStyle | BubbleStyle, string> = {
  solid: pickerStyles.sampleSolid,
  soft: pickerStyles.sampleSolid,
  glass: pickerStyles.sampleGlass,
  line: pickerStyles.sampleLine,
  plain: pickerStyles.samplePlain
};

/** A row of preview tiles for a small closed set (style, bubbles). */
function ShapeTiles<T extends UiStyle | BubbleStyle>({
  values,
  value,
  label,
  onPick,
  t,
  labelKey
}: {
  values: readonly T[];
  value: T;
  label: string;
  onPick: (v: T) => void;
  t: TFunction;
  labelKey: (v: T) => string;
}) {
  return (
    <div className={pickerStyles.tiles} role="group" aria-label={label}>
      {values.map((v) => (
        <button
          key={v}
          type="button"
          className={pickerStyles.tile}
          aria-pressed={value === v}
          onClick={() => onPick(v)}
        >
          <span className={`${pickerStyles.preview} ${pickerStyles.shape} ${SAMPLE_CLASS[v]}`}>
            <i />
            <i />
          </span>
          <span className={pickerStyles.label}>
            {t(labelKey(v))}
            {value === v ? <Check /> : null}
          </span>
        </button>
      ))}
    </div>
  );
}

/** Cards of the "Appearance" section. */
export function appearanceCards(b: SettingsBridge, t: TFunction): CardDef[] {
  const { settings, update } = b;
  const card = (
    id: string,
    title: string,
    description: string,
    node: React.ReactNode,
    over: Partial<CardDef> = {}
  ): CardDef => ({
    id,
    section: "appearance",
    title,
    description,
    kind: "row",
    node,
    ...over
  });
  const row = (key: string, node: React.ReactNode, stacked = true) => (
    <SettingRow stacked={stacked} title={t(`appearance.${key}.title`)} description={t(`appearance.${key}.description`)}>
      {node}
    </SettingRow>
  );

  const theme = resolveTheme(settings.themeMode, settings.customTheme);
  const accent = accentFor(settings.accentColor, theme, b.botColor);
  const fixed = settings.accentColor.startsWith("#") ? settings.accentColor : "";
  const swatch = (key: string, color: string, label: string, pressed: boolean, onClick: () => void, icon?: React.ReactNode) => (
    <button
      key={key}
      type="button"
      className={pickerStyles.swatch}
      style={{ "--swatch-color": color } as CSSProperties}
      title={label}
      aria-label={label}
      aria-pressed={pressed}
      onClick={onClick}
    >
      {icon}
    </button>
  );
  const kind = accentKind(settings.accentColor, theme);
  const accentHint =
    kind === "bot"
      ? t("appearance.accent.botHint")
      : kind === "theme"
        ? t("appearance.accent.themeHint")
        : t("appearance.accent.fixedHint");
  const themeAccent = paletteById(selectedThemeId(settings.themeMode))?.accent;

  return [
    card(
      "theme",
      t("appearance.theme.title"),
      t("appearance.theme.description"),
      row(
        "theme",
        <ThemePicker
          mode={settings.themeMode}
          custom={settings.customTheme}
          accent={settings.accentColor}
          onChange={update}
        />
      ),
      { keywords: "dark light mode tema karanlik aydinlik theme" }
    ),
    card(
      "accent",
      t("appearance.accent.title"),
      t("appearance.accent.description"),
      row(
        "accent",
        <div className={pickerStyles.root}>
          <div className={pickerStyles.swatches}>
            {swatch(
              "bot",
              b.botColor ?? accent,
              t("appearance.accent.bot"),
              kind === "bot",
              () => update({ accentColor: "bot" }),
              <BotIcon />
            )}
            {themeAccent
              ? swatch("theme", themeAccent, t("appearance.accent.theme"), kind === "theme", () =>
                  update({ accentColor: "theme" })
                )
              : null}
            {ACCENT_PRESETS.map((p) =>
              swatch(
                p.id,
                p.value,
                t(`settings.accent.presets.${p.id}`),
                fixed.toLowerCase() === p.value.toLowerCase(),
                () => update({ accentColor: p.value })
              )
            )}
            <label
              className={`${pickerStyles.swatch} ${pickerStyles.custom}`}
              style={{ "--swatch-color": fixed || accent } as CSSProperties}
              title={t("appearance.accent.custom")}
            >
              <input
                type="color"
                aria-label={t("appearance.accent.custom")}
                value={fixed || accent}
                onChange={(e) => update({ accentColor: e.target.value }, { deferSave: true })}
              />
            </label>
          </div>
          <p className={pickerStyles.hint}>{accentHint}</p>
        </div>
      ),
      { keywords: "color renk vurgu accent" }
    ),
    card(
      "style",
      t("appearance.style.title"),
      t("appearance.style.description"),
      row(
        "style",
        <ShapeTiles
          values={UI_STYLES}
          value={settings.uiStyle}
          label={t("appearance.style.title")}
          onPick={(uiStyle) => update({ uiStyle })}
          t={t}
          labelKey={(v) => `appearance.style.${v}`}
        />
      ),
      { keywords: "glass solid line cam duz cizgi stil" }
    ),
    card(
      "backdrop",
      t("appearance.backdrop.title"),
      t("appearance.backdrop.description"),
      row(
        "backdrop",
        <BackdropPicker
          backdrop={settings.backdrop}
          dim={settings.backdropDim}
          backgroundUrl={settings.backgroundUrl}
          colors={sceneColors(theme, accent)}
          onChange={update}
        />
      ),
      { keywords: "wallpaper scene arka plan backdrop" }
    ),
    card(
      "corners",
      t("appearance.corners.title"),
      t("appearance.corners.description"),
      row(
        "corners",
        <Segmented
          label={t("appearance.corners.title")}
          value={settings.corners}
          onChange={(corners) => update({ corners })}
          options={CORNERS.map((v) => ({ value: v, label: t(`appearance.corners.${v}`) }))}
        />,
        false
      ),
      { keywords: "radius round sharp koseler" }
    ),
    card(
      "bubbles",
      t("appearance.bubbles.title"),
      t("appearance.bubbles.description"),
      row(
        "bubbles",
        <div className={pickerStyles.root}>
          <ShapeTiles
            values={BUBBLE_STYLES}
            value={settings.bubble}
            label={t("appearance.bubbles.title")}
            onPick={(bubble) => update({ bubble })}
            t={t}
            labelKey={(v) => `appearance.bubbles.${v}`}
          />
          <div className={pickerStyles.row}>
            <span className={pickerStyles.hint}>{t("appearance.bubbles.own")}</span>
            <Segmented
              label={t("appearance.bubbles.own")}
              value={settings.ownBubble}
              onChange={(ownBubble) => update({ ownBubble })}
              options={OWN_BUBBLES.map((v) => ({ value: v, label: t(`appearance.bubbles.${v}`) }))}
            />
          </div>
        </div>
      ),
      { keywords: "chat bubble balon mesaj" }
    ),
    card(
      "background",
      t("settings.background.title"),
      t("settings.background.description"),
      <SettingRow
        stacked
        title={t("settings.background.title")}
        description={t("settings.background.description")}
      >
        <BackgroundControls b={b} t={t} />
      </SettingRow>,
      { keywords: "wallpaper image arka plan" }
    ),
    card(
      "frame",
      t("settingsModal.general.frame.title"),
      t("settingsModal.general.frame.description"),
      <SettingRow
        title={t("settingsModal.general.frame.title")}
        description={t("settingsModal.general.frame.description")}
      >
        <Switch
          label={t("settingsModal.general.frame.title")}
          checked={settings.windowFrame}
          onChange={b.onWindowFrame}
        />
      </SettingRow>,
      { keywords: "window title bar pencere cerceve" }
    )
  ];
}
