import { Ban, Check } from "lucide-react";
import { useTranslation } from "react-i18next";
import { resolveWallpaper, WALLPAPER_KINDS, type WallpaperKind, type WallpaperSetting } from "../lib/wallpaper";
import { WallpaperSwatch } from "./Wallpaper";
import styles from "./AppearancePicker.module.css";

/** Settings > Appearance: Off, the four wallpapers, and "Follow theme". */
export function WallpaperPicker({
  value,
  themeId,
  themeIsDark,
  onChange
}: {
  value: WallpaperSetting;
  themeId: string;
  themeIsDark: boolean;
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
      <p className={styles.hint}>{t("appearance.wallpaper.hint")}</p>
    </div>
  );
}
