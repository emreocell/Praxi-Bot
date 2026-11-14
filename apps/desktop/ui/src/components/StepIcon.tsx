import {
  Bot,
  CalendarClock,
  Brain,
  FileText,
  FolderOpen,
  Globe,
  ListChecks,
  Mail,
  Monitor,
  Search,
  Sparkles,
  Terminal,
  Workflow,
  Wrench,
  type LucideIcon
} from "lucide-react";
import type { CSSProperties } from "react";
import { brandIcon } from "../lib/brandIcons";
import type { StepIcon as StepIconName } from "../lib/toolLabels";
import styles from "./StepIcon.module.css";

const LUCIDE: Record<StepIconName, LucideIcon> = {
  search: Search,
  globe: Globe,
  file: FileText,
  folder: FolderOpen,
  terminal: Terminal,
  mail: Mail,
  calendar: CalendarClock,
  memory: Brain,
  workflow: Workflow,
  agent: Bot,
  desktop: Monitor,
  list: ListChecks,
  skill: Sparkles,
  tool: Wrench
};

const MULTICOLOR = new Set(["gmail", "googledrive", "googlecalendar", "figma"]);

/** Relative luminance of `#rrggbb` (0 dark .. 1 light). */
function luminance(hex: string): number {
  const n = Number.parseInt(hex.replace("#", ""), 16);
  const ch = [(n >> 16) & 255, (n >> 8) & 255, n & 255].map((v) => {
    const c = v / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * (ch[0] ?? 0) + 0.7152 * (ch[1] ?? 0) + 0.0722 * (ch[2] ?? 0);
}

/**
 * A bare tool / app glyph: no tile behind it. Known brands draw their own
 * mark (very dark or very light marks take the text colour so they stay
 * readable on every theme); everything else is a lucide icon.
 */
export function StepIcon({
  icon,
  brand,
  size = 16
}: {
  icon: StepIconName;
  brand?: string;
  size?: number;
}) {
  const style = { "--step-icon-size": `${size}px` } as CSSProperties;
  if (brand) {
    const b = brandIcon(brand);
    const key = brand.toLowerCase().replace(/[^a-z]/g, "");
    if (MULTICOLOR.has(key)) {
      return (
        <span className={styles.icon} style={style} aria-hidden="true">
          <img src={`/brands/${key}.svg`} alt="" draggable={false} />
        </span>
      );
    }
    if (b.known && b.path) {
      const lum = luminance(b.color);
      const own = lum > 0.12 && lum < 0.7;
      return (
        <span
          className={styles.icon}
          style={{ ...style, ...(own ? { color: b.color } : null) }}
          aria-hidden="true"
        >
          <svg viewBox="0 0 24 24">
            <path d={b.path} fill="currentColor" />
          </svg>
        </span>
      );
    }
  }
  const Glyph = LUCIDE[icon];
  return (
    <span className={styles.icon} style={style} aria-hidden="true">
      <Glyph />
    </span>
  );
}
