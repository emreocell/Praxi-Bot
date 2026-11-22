# Live Cloud Wallpaper: Measurements & Architecture

Status: implemented (`feat/c501-live-cloud-wallpaper`). Answers the owner's question: *"would a real animated cloud background be too heavy?"* with measurements across candidate implementations.

## Verdict

No, a real animated cloud background is **not** too heavy when rendered on the GPU with a small WebGL fragment shader at low internal resolution, capped at **30 fps** with `setTimeout` + `requestAnimationFrame` pacing, and paused whenever the window is hidden, unfocused, minimised, on battery saver, or under `prefers-reduced-motion`.

- **Chosen approach:** WebGL fragment shader (`apps/desktop/ui/src/lib/wallpaperLive.ts`) with 3 parallax 2D fbm cloud layers + the existing ordered Bayer / 45° diagonal dither post-effect mapped strictly to the 4-colour theme palette (`sky`, `dusk`, `night`, `warm`), internal resolution capped at `360,000` pixels and upscaled via `image-rendering: pixelated`.
- **Why not layered CSS-transformed cloud sprites?** Pre-rendered sprite layers used **1.97 %–4.16 %** total system CPU (exceeding the `< 3 %` budget at `1280×800` scale 2), cannot apply the ordered dither/halftone post-effect across overlapping layers without moiré interference, and only translate rigid textures without soft cloud billowing.
- **Why not 30 fps 2D CPU canvas?** Shifting a precomputed 2D buffer at 30 fps spent `2.164 ms` per frame in JS (`201.2 ms` script time over 3 s, `10.71 %` main-thread CPU) vs `0.019–0.023 ms` per frame (`10.72–22.68 ms` script time over 4 s) for WebGL.

## Measurements

Measured in headless Chrome (`ANGLE (NVIDIA GeForce RTX 3060 Direct3D11 vs_5_0 ps_5_0, D3D11)`) on a 12-core Windows host over a 4-second steady-state window (3 s for `cpu-2d-30fps`).

### Standalone Renderer Comparison

| Mode | Viewport | Internal size | Draws | Avg JS draw (ms) | GPU sync (`gl.finish`, ms) | Main-thread CPU (1 core) | System CPU (12 cores) | Dropped frames (>50 ms) |
|---|---|---|---|---|---|---|---|---|
| `subtle-2d` (1 col / 4 s) | 1280×800 | 640×400 | 1 | 31.200 | — | 2.28 % | 1.72 % | 0 |
| `cpu-2d-30fps` (3 s) | 1280×800 | 640×400 | 85 | 2.164 | — | 10.71 % | 2.18 % | 3 (>25 ms rAF) |
| `css-sprites` (3 layers) | 1280×800 | 640×400 | 0 | 0 | — | 6.06 % | 4.16 % | 0 |
| `css-sprites` (3 layers) | 1920×1080 | 640×360 | 0 | 0 | — | 4.13 % | 1.97 % | 0 |
| `webgl-fbm-30fps` | 1280×800 | 640×400 | 116 | 0.023 | 0.003 | 1.41 % | 2.20 % | 0 (max 45.2 ms) |
| `webgl-fbm-30fps` | 1280×800 | 427×267 | 117 | 0.021 | — | 1.34 % | 0.96 % | 1 (max 265.1 ms) |
| `webgl-fbm-30fps` | 1920×1080 | 640×360 | 113 | 0.019 | <0.001 | 2.52 % | 0.96 % | 3 (max 415.1 ms) |

> **Scheduling note:** Spinning raw `requestAnimationFrame` on a high-refresh display woke JS ~200×/s to skip frames (`5.28 %–7.55 %` main-thread CPU). Scheduling the 30 fps interval with `setTimeout` before `requestAnimationFrame` keeps the main thread asleep between frames (`1.34 %–2.52 %`). One-time cold ANGLE shader compilation on initial mount took ~1.7 s main-thread task time before steady state.

### Full Praxi UI Delta (`?mock=1`, 1280×800, Obsidian + Night)

| Shell style | Motion | Script time (3 s) | Main-thread CPU | System CPU | Delta (`live - off`) |
|---|---|---|---|---|---|
| `solid` | `off` | 63.20 ms | 19.73 % | 11.66 % | — |
| `solid` | `live` | 79.98 ms | 30.96 % | 9.02 % | +16.78 ms JS (+0.56 % of 1 core) |
| `glass` | `off` | 51.80 ms | 25.27 % | 9.38 % | — |
| `glass` | `live` | 554.87 ms | 46.14 % | 11.21 % | +1.83 % system CPU (includes `backdrop-filter: blur(28px)` recomposition) |

### Production Bundle Size (`apps/desktop/ui`)

| Asset | Before | After | Delta |
|---|---|---|---|
| `index-*.js` (raw) | 2,035,436 B (2,035.44 kB) | 2,045,690 B (2,045.69 kB) | **+10,254 B (+10.01 kB)** |
| `index-*.css` (raw) | 388,399 B (388.40 kB) | 388,399 B (388.40 kB) | **0 B** |

## Controls, Power Guards & Contrast

- **Settings → Appearance → Wallpaper → Cloud motion:** `Off` / `Subtle` / `Live` (defaults to `Subtle`).
- **Pause guards:** Both `Subtle` and `Live` immediately pause (and accumulate elapsed time cleanly so resuming never jumps) when:
  - `document.hidden` is true (window hidden / minimised)
  - window loses focus (`!document.hasFocus()`, `blur` / `focus`)
  - `prefers-reduced-motion: reduce` matches (reacts to live OS changes)
  - Battery saver heuristic triggers (`navigator.getBattery()` reports `!charging && level <= 0.20`)
- **WebGL fallback:** If `getContext("webgl")` is unavailable or `webglcontextlost` fires, `startWallpaper` falls back to the 2D static wallpaper (`buildField` + `renderField`).
- **Theme palettes & text contrast:** Reuses the four palettes (`night` for Obsidian, `sky` for Cobalt, `dusk` for Raven, `warm` for Clay) and the PR #514 **Wallpaper veil** + message plate system; `gl.readPixels` samples frame 0 once on init/resize (never per frame) for `publishSceneSource`, and `wallpaperLegibility.test.ts` verifies worst-case palette coverage stays ≥ 4.5:1 body / ≥ 7:1 panel contrast.
