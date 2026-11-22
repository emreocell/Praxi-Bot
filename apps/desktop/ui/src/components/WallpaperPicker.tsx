import { Ban, Check } from "lucide-react";
import { useTranslation } from "react-i18next";
import { VEIL_MAX, VEIL_MIN } from "ui/surface-model";
import {
  resolveWallpaper,
  WALLPAPER_KINDS,
  WALLPAPER_MOTIONS,
  type WallpaperKind,
  type WallpaperMotion,
  type WallpaperSetting
} from "../lib/wallpaper";
import { Segmented } from "./SettingsUi";
import { WallpaperSwatch } from "./Wallpaper";
import styles from "./AppearancePicker.module.css";

const MOTION_LABEL_KEY: Readonly<Record<WallpaperMotion, "appearance.wallpaper.motionOff" | "appearance.wallpaper.motionSubtle" | "appearance.wallpaper.motionLive">> = {
  off: "appearance.wallpaper.motionOff",
  subtle: "appearance.wallpaper.motionSubtle",
  live: "appearance.wallpaper.motionLive"
};

/** Settings > Appearance: Off, the four wallpapers, "Follow theme", and Cloud motion (Off / Subtle / Live). */
export function WallpaperPicker({
  value,
  motion,
  themeId,
  themeIsDark,
  veil,
  onVeil,
  onMotion,
  onChange
}: {
  value: WallpaperSetting;
  motion: WallpaperMotion;
  themeId: string;
  themeIsDark: boolean;
  veil: number;
  onVeil: (next: number) => void;
  onMotion: (next: WallpaperMotion) => void;
  onChange: (next: WallpaperSetting) => void;
}) {
  const { t } = useTranslation();
  const followKind: WallpaperKind = resolveWallpaper("theme", themeId, themeIsDark) ?? "sky";
  const tile = (id: WallpaperSetting, label: string, preview: React.ReactNode) => (
    <button key={id} type="button" className={styles.tile} aria-pressed={value === id} onClick={() => onChange(id)}>
      <span className={styles.preview}>{preview}</span>
      <span className={styles.label}>
        {label}
        {value === id ? <Check /> : null}
      </span>
    </button>
  );
  return (
    <div className={styles.root}>
      <div className={styles.tiles}>
        {tile("off", t("appearance.wallpaper.off"), <Ban />)}
        {WALLPAPER_KINDS.map((k) => tile(k, t(`appearance.wallpaper.${k}`), <WallpaperSwatch kind={k} />))}
        {tile("theme", t("appearance.wallpaper.theme"), <WallpaperSwatch kind={followKind} />)}
      </div>
      <div className={styles.row}>
        <span className={styles.rangeName}>{t("appearance.wallpaper.motion")}</span>
        <Segmented
          label={t("appearance.wallpaper.motion")}
          value={motion}
          onChange={onMotion}
          options={WALLPAPER_MOTIONS.map((m) => ({ value: m, label: t(MOTION_LABEL_KEY[m]) }))}
        />
      </div>
      {value !== "off" ? (
        <label className={styles.range}>
          <span className={styles.rangeName}>{t("appearance.wallpaper.veil")}</span>
          <span>{t("appearance.wallpaper.veilClear")}</span>
          <input
            type="range"
            min={VEIL_MIN}
            max={VEIL_MAX}
            step={1}
            value={veil}
            aria-label={t("appearance.wallpaper.veil")}
            onChange={(e) => onVeil(Number(e.target.value))}
          />
          <span>{t("appearance.wallpaper.veilSoft")}</span>
        </label>
      ) : null}
      <p className={styles.hint}>{value !== "off" ? t("appearance.wallpaper.veilHint") : t("appearance.wallpaper.hint")}</p>
      <p className={styles.hint}>{t("appearance.wallpaper.motionHint")}</p>
    </div>
  );
}
