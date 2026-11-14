import { useEffect, useRef, type CSSProperties } from "react";
import { deriveTheme, type Theme } from "ui";
import { backdropInfo, backdropSize } from "../lib/backdropCatalog";
import { drawBackdrop, sceneScale, type SceneColors } from "../lib/backdropDraw";
import styles from "./Backdrop.module.css";

/** Theme colours a scene may read; the memo key of the canvas. */
export function sceneColors(theme: Theme, accent: string): SceneColors {
  const d = deriveTheme(theme);
  return { bg: d.bg, raised: d.raised, text: d.text, accent };
}

const RESIZE_DEBOUNCE_MS = 120;

/**
 * Full-window backdrop.  Drawn once per (scene, size, colours) — no animation
 * loop — and redrawn (debounced) when the window is resized.  Scenes that do
 * not use theme colours ignore theme / accent changes.
 */
export function Backdrop({
  scene,
  dim,
  colors
}: {
  scene: string;
  /** Percent of the theme background laid over the scene (20..88). */
  dim: number;
  colors: SceneColors;
}) {
  const ref = useRef<HTMLCanvasElement | null>(null);
  const info = backdropInfo(scene);
  // Theme-aware scenes depend on the colours; the others only on the scene.
  const colorKey = info?.themed ? `${colors.bg}|${colors.raised}|${colors.text}|${colors.accent}` : "";

  useEffect(() => {
    const cv = ref.current;
    if (!cv || !info) return undefined;
    const k = sceneScale(info);
    const paint = () => {
      const { w, h } = backdropSize(window.innerWidth, window.innerHeight, k);
      drawBackdrop(cv, scene, w, h, colors);
    };
    paint();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const onResize = () => {
      clearTimeout(timer);
      timer = setTimeout(paint, RESIZE_DEBOUNCE_MS);
    };
    window.addEventListener("resize", onResize);
    return () => {
      clearTimeout(timer);
      window.removeEventListener("resize", onResize);
    };
    // `colors` is represented by `colorKey` (only theme-aware scenes depend on it).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scene, info, colorKey]);

  if (!info) return null;
  return (
    <div
      className={styles.root}
      aria-hidden="true"
      style={{ "--backdrop-dim": dim / 100 } as CSSProperties}
    >
      <canvas ref={ref} className={styles.canvas} />
    </div>
  );
}

/** Small preview of a scene for the picker; drawn once. */
export function BackdropThumb({ scene, colors }: { scene: string; colors: SceneColors }) {
  const ref = useRef<HTMLCanvasElement | null>(null);
  const info = backdropInfo(scene);
  const colorKey = info?.themed ? `${colors.bg}|${colors.raised}|${colors.text}|${colors.accent}` : "";
  useEffect(() => {
    const cv = ref.current;
    if (!cv || !info) return;
    drawBackdrop(cv, scene, info.fx ? 150 : 60, info.fx ? 90 : 36, colors);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scene, info, colorKey]);
  return <canvas ref={ref} className={styles.thumb} aria-hidden="true" />;
}
