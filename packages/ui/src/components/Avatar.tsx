import * as AvatarPrimitive from "@radix-ui/react-avatar";
import type { CSSProperties } from "react";
import { useEffect, useState } from "react";
import { isReducedMotionPreferred } from "../theme";
import styles from "./Avatar.module.css";

export type AvatarSize = "sm" | "md" | "lg";

export interface AvatarProps {
  name: string;
  src?: string;
  initials?: string;
  color?: string;
  size?: AvatarSize;
  isAnimated?: boolean;
  className?: string;
}

function getInitials(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  if (parts.length === 0) {
    return "?";
  }
  if (parts.length === 1) {
    return parts[0].slice(0, 2).toUpperCase();
  }
  return `${parts[0][0]}${parts[parts.length - 1][0]}`.toUpperCase();
}

function detectAnimatedGif(src?: string, isAnimated?: boolean): boolean {
  if (typeof isAnimated === "boolean") {
    return isAnimated;
  }
  if (!src) {
    return false;
  }
  const normalized = src.trim().toLowerCase();
  return (
    normalized.endsWith(".gif") || normalized.startsWith("data:image/gif")
  );
}

export function Avatar({
  name,
  src,
  initials,
  color,
  size = "md",
  isAnimated,
  className
}: AvatarProps) {
  const [reducedMotion, setReducedMotion] = useState<boolean>(() =>
    isReducedMotionPreferred()
  );

  useEffect(() => {
    if (
      typeof window === "undefined" ||
      typeof window.matchMedia !== "function"
    ) {
      return;
    }
    const mediaQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
    const listener = (event: MediaQueryListEvent) => {
      setReducedMotion(event.matches);
    };
    mediaQuery.addEventListener("change", listener);
    return () => {
      mediaQuery.removeEventListener("change", listener);
    };
  }, []);

  const animated = detectAnimatedGif(src, isAnimated);
  const effectiveSrc = animated && reducedMotion ? undefined : src;
  const fallbackText = initials ?? getInitials(name);

  const rootStyle: CSSProperties | undefined = color
    ? ({ "--avatar-bg": color } as CSSProperties)
    : undefined;

  const classes = [styles.root, styles[size], className ?? ""]
    .filter(Boolean)
    .join(" ");

  return (
    <AvatarPrimitive.Root className={classes} style={rootStyle}>
      {effectiveSrc ? (
        <AvatarPrimitive.Image
          className={styles.image}
          src={effectiveSrc}
          alt={name}
        />
      ) : null}
      <AvatarPrimitive.Fallback className={styles.fallback} delayMs={0}>
        {fallbackText}
      </AvatarPrimitive.Fallback>
    </AvatarPrimitive.Root>
  );
}
