import { useEffect, useRef, type CSSProperties } from "react";
import { publishSceneSource, sampleCanvasSource } from "../lib/backdropTone";
import { buildField, packPalette, renderField, startWallpaper, WALLPAPERS, wallpaperSceneId, WALLPAPER_DIM, type WallpaperKind } from "../lib/wallpaper";
import styles from "./Backdrop.module.css";

/** Full-window animated wallpaper (dithered clouds); stands in for <Backdrop> when one is chosen. */
export function Wallpaper({ kind }: { kind: WallpaperKind }) {
  const ref = useRef<HTMLCanvasElement | null>(null);
  useEffect(() => {
    const cv = ref.current;
    if (!cv) return undefined;
    const scene = wallpaperSceneId(kind);
    document.body.classList.add("has-wallpaper");
    const handle = startWallpaper(cv, kind, () => publishSceneSource(scene, sampleCanvasSource(cv, window.innerWidth)));
    return () => {
      handle.stop();
      publishSceneSource(scene, null);
      document.body.classList.remove("has-wallpaper");
    };
  }, [kind]);
  return (
    <div className={styles.root} aria-hidden="true" style={{ "--backdrop-dim": WALLPAPER_DIM / 100 } as CSSProperties}>
      <canvas ref={ref} className={styles.canvas} style={{ imageRendering: "pixelated" }} />
    </div>
  );
}

/** Static preview swatch for the Appearance picker (one frame, small). */
export function WallpaperSwatch({ kind }: { kind: WallpaperKind }) {
  const ref = useRef<HTMLCanvasElement | null>(null);
  useEffect(() => {
    const cv = ref.current;
    if (!cv) return;
    const style = WALLPAPERS[kind];
    cv.width = 96;
    cv.height = 60;
    const ctx = cv.getContext("2d");
    if (!ctx) return;
    const img = ctx.createImageData(96, 60);
    renderField(buildField(96, 60, style.cover, style.haze), 96, 60, 0, style, packPalette(style.palette), new Uint32Array(img.data.buffer));
    ctx.putImageData(img, 0, 0);
  }, [kind]);
  return <canvas ref={ref} className={styles.thumb} style={{ imageRendering: "pixelated" }} aria-hidden="true" />;
}
