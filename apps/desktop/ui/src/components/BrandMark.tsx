import { FolderOpen } from "lucide-react";
import type { CSSProperties } from "react";
import { brandIcon } from "../lib/brandIcons";
import styles from "./BrandMark.module.css";

const MULTICOLOR = ["gmail", "googledrive", "googlecalendar", "figma"];

/**
 * A brand logo drawn bare: no tile behind it.  Logos that are nearly black or white turn to the
 * text colour on the theme where they would vanish (`data-tone`). Engine/provider
 * marks follow the text colour in every theme. The wrapper names the artwork.
 */
export function BrandMark({ name, size = 16, monochrome = false }: { name: string; size?: number; monochrome?: boolean }) {
  const brand = brandIcon(name);
  const key = name.toLowerCase().replace(/[^a-z]/g, "");
  const style = { "--brand-color": brand.color, width: size, height: size } as CSSProperties;
  const tone = brand.monochrome || !brand.known ? "mono" : brightness(brand.color) > 200 ? "light" : brightness(brand.color) < 70 ? "dark" : "mid";
  const accessible = { role: "img", "aria-label": brand.name };

  if (MULTICOLOR.includes(key) && !monochrome) {
    return (
      <span className={styles.mark} style={style} {...accessible}>
        <img src={`/brands/${key}.svg`} alt="" draggable={false} />
      </span>
    );
  }
  if (key === "filesystem") {
    return (
      <span className={styles.mark} style={{ ...style, color: "var(--color-text-muted)" }} {...accessible}>
        <FolderOpen />
      </span>
    );
  }
  if (key === "slack") {
    return (
      <span className={styles.mark} style={style} {...accessible}>
        <svg viewBox="0 0 24 24">
          <path fill="#36C5F0" d="M9 2a2 2 0 0 1 2 2v6H7V4a2 2 0 0 1 2-2M2 9a2 2 0 0 1 2-2h2v4H4a2 2 0 0 1-2-2" />
          <path fill="#2EB67D" d="M22 9a2 2 0 0 1-2 2h-6V7h6a2 2 0 0 1 2 2M15 2a2 2 0 0 1 2 2v2h-4V4a2 2 0 0 1 2-2" />
          <path fill="#ECB22E" d="M15 22a2 2 0 0 1-2-2v-6h4v6a2 2 0 0 1-2 2M22 15a2 2 0 0 1-2 2h-2v-4h2a2 2 0 0 1 2 2" />
          <path fill="#E01E5A" d="M2 15a2 2 0 0 1 2-2h6v4H4a2 2 0 0 1-2-2M9 22a2 2 0 0 1-2-2v-2h4v2a2 2 0 0 1-2 2" />
        </svg>
      </span>
    );
  }
  if (brand.path) {
    return (
      <span className={styles.mark} style={style} data-tone={tone} {...accessible}>
        <svg viewBox="0 0 24 24">
          <path d={brand.path} />
        </svg>
      </span>
    );
  }
  return (
    <span className={`${styles.mark} ${styles.mono}`} style={{ ...style, fontSize: Math.round(size * 0.55) }} {...accessible}>
      {brand.monogram}
    </span>
  );
}

/** Perceived brightness 0-255 of a `#rrggbb` colour. */
function brightness(hex: string): number {
  const n = parseInt(hex.slice(1), 16);
  if (Number.isNaN(n)) return 128;
  return 0.299 * ((n >> 16) & 255) + 0.587 * ((n >> 8) & 255) + 0.114 * (n & 255);
}
