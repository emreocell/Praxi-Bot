import { type CSSProperties, useEffect, useState } from "react";
import {
  applyTheme,
  BOT_AVATAR_STATES,
  BOT_SHAPES,
  BotAvatar,
  CHARACTER_COLORS,
  paletteById,
  setAvatarTimeScale,
  type BotAvatarSize,
  type BotAvatarState,
  type BotShapeId
} from "ui";
import styles from "./AvatarGallery.module.css";

const GALLERY_THEMES = ["obsidian", "cobalt", "raven", "clay"] as const;
type GalleryThemeId = (typeof GALLERY_THEMES)[number];

const SIZES: readonly BotAvatarSize[] = ["xs", "sm", "md", "lg", "panel", "hero"];

/** The reference-like cast: coral drop, white gumdrop, butter capsule, sky "ears" (rounded tilted square). */
const CAST: readonly { id: string; shape: BotShapeId; color: string }[] = [
  { id: "cast-drop", shape: "drop", color: "#f39a84" },
  { id: "cast-gumdrop", shape: "gumdrop", color: "#f4f4f5" },
  { id: "cast-capsule", shape: "capsule", color: "#f6dfa0" },
  { id: "cast-ears", shape: "ears", color: "#86d3e6" }
];

const TURNS = [-34, -18, 0, 18, 34] as const;

const LIST_BOTS: readonly { id: string; name: string; role: string; shape: BotShapeId; color: string; state: BotAvatarState }[] = [
  { id: "gal-atlas", name: "Atlas", role: "Idle", shape: "gem", color: "#4c8dff", state: "idle" },
  { id: "gal-nova", name: "Nova", role: "Thinking through sources", shape: "diamond", color: "#2fc4b2", state: "thinking" },
  { id: "gal-mira", name: "Mira", role: "Working on pull request", shape: "cloud", color: "#f25fa8", state: "working" },
  { id: "gal-echo", name: "Echo", role: "Waiting for your approval", shape: "flower", color: "#f5803e", state: "waiting-approval" },
  { id: "gal-praxi", name: "Praxi", role: "Idle (porcelain)", shape: "ghost", color: "#f4f4f5", state: "idle" },
  { id: "gal-byte", name: "Byte", role: "Tool call failed", shape: "cube", color: "#f2545b", state: "error" }
];

function param(name: string): string | null {
  return new URLSearchParams(window.location.search).get(name);
}

function initialTheme(): GalleryThemeId {
  const wanted = param("theme");
  return (GALLERY_THEMES.find((id) => id === wanted) ?? "obsidian") as GalleryThemeId;
}

/**
 * Dev/mock-only gallery for reviewing the clay avatars: shapes, material in the 4 core themes,
 * the 3D head turn, idle behaviour, state expressions, hover/press and reduced motion.
 * Reachable only in dev or `VITE_MOCK=1` builds via `?mock=1&gallery=avatars`
 * (`&shape=`, `&state=`, `&color=rrggbb`, `&reduced=1`, `&fast=1`, `&only=cast|hero|turn|shapes|states|crowd` preset it).
 */
export function AvatarGallery() {
  const [themeId, setThemeId] = useState<GalleryThemeId>(initialTheme);
  const [color, setColor] = useState<string>(() => (param("color") ? `#${param("color")}` : "#34c77b"));
  const [heroShape, setHeroShape] = useState<BotShapeId>(
    () => (BOT_SHAPES.find((s) => s.id === param("shape"))?.id ?? "drop") as BotShapeId
  );
  const [heroState, setHeroState] = useState<BotAvatarState>(
    () => (BOT_AVATAR_STATES.find((s) => s === param("state")) ?? "idle") as BotAvatarState
  );
  const [reduced, setReduced] = useState<boolean>(() => param("reduced") === "1");
  const [fast, setFast] = useState<boolean>(() => param("fast") === "1");
  const only = param("only");
  const show = (section: string) => !only || only.split(",").includes(section);

  useEffect(() => {
    const def = paletteById(themeId);
    if (def) {
      applyTheme(def, { style: "solid", corners: "soft" });
    }
  }, [themeId]);

  useEffect(() => {
    setAvatarTimeScale(fast ? 6 : 1);
    return () => setAvatarTimeScale(1);
  }, [fast]);

  const pillClass = (active: boolean) => [styles.pill, active ? styles.pillActive : ""].filter(Boolean).join(" ");

  return (
    <main
      key={reduced ? "reduced" : "motion"}
      className={styles.page}
      data-reduced-motion={reduced ? "true" : undefined}
    >
      <header className={styles.header}>
        <div>
          <h1 className={styles.title}>Bot Avatar Gallery</h1>
          <p className={styles.subtitle}>
            Dev-only review surface. Hover an avatar: it turns its head toward the pointer. Press: a soft squish.
          </p>
        </div>
        <div className={styles.controls}>
          <div className={styles.pillRow} role="group" aria-label="Theme">
            {GALLERY_THEMES.map((id) => (
              <button key={id} type="button" className={pillClass(themeId === id)} onClick={() => setThemeId(id)}>
                {id}
              </button>
            ))}
          </div>
          <button type="button" className={pillClass(reduced)} aria-pressed={reduced} onClick={() => setReduced((v) => !v)}>
            {reduced ? "Reduced motion: ON" : "Reduced motion: OFF"}
          </button>
          <button type="button" className={pillClass(fast)} aria-pressed={fast} onClick={() => setFast((v) => !v)}>
            {fast ? "Watch it idle: 6x" : "Watch it idle: 1x"}
          </button>
        </div>
      </header>

      {show("cast") ? (
        <section className={styles.card}>
          <h2 className={styles.sectionTitle}>Cast</h2>
          <div className={styles.castRow}>
            {CAST.map((c) => (
              <span key={c.id} className={styles.zoomCast}>
                <BotAvatar bot={{ id: c.id, name: c.shape, shape: c.shape, color: c.color }} size="hero" live decorative />
              </span>
            ))}
          </div>
        </section>
      ) : null}

      {show("hero") ? (
        <section className={styles.card}>
          <div className={styles.sectionHead}>
            <h2 className={styles.sectionTitle}>Hero &amp; tint swatches</h2>
            <div className={styles.pillRow}>
              {BOT_AVATAR_STATES.map((st) => (
                <button key={st} type="button" className={pillClass(heroState === st)} onClick={() => setHeroState(st)}>
                  {st}
                </button>
              ))}
            </div>
          </div>
          <div className={styles.heroRow}>
            <button type="button" className={styles.heroStage} aria-label={`Hero ${heroShape}`}>
              <span className={styles.zoomHero}>
                <BotAvatar
                  bot={{ id: `hero-${heroShape}`, name: heroShape, shape: heroShape, color }}
                  size="hero"
                  state={heroState}
                  live
                  decorative
                />
              </span>
            </button>
            <div className={styles.swatches}>
              {CHARACTER_COLORS.map((hex) => (
                <button
                  key={hex}
                  type="button"
                  className={[styles.swatch, color === hex ? styles.swatchActive : ""].filter(Boolean).join(" ")}
                  style={{ "--swatch": hex } as CSSProperties}
                  aria-label={hex}
                  onClick={() => setColor(hex)}
                />
              ))}
              <input
                type="color"
                className={styles.colorInput}
                value={color}
                aria-label="Custom colour"
                onChange={(e) => setColor(e.target.value)}
              />
            </div>
          </div>
        </section>
      ) : null}

      {show("turn") ? (
        <section className={styles.card}>
          <h2 className={styles.sectionTitle}>Head turn (fixed yaw -34, -18, 0, 18, 34; then pitch up and down)</h2>
          <div className={styles.turnRow}>
            {TURNS.map((yaw) => (
              <span key={yaw} className={styles.zoomTurn}>
                <BotAvatar
                  bot={{ id: `turn-${yaw}`, name: String(yaw), shape: heroShape, color }}
                  size="hero"
                  live={false}
                  lookAt={{ yaw, pitch: 0 }}
                  decorative
                />
              </span>
            ))}
            {[-20, 20].map((pitch) => (
              <span key={pitch} className={styles.zoomTurn}>
                <BotAvatar
                  bot={{ id: `pitch-${pitch}`, name: String(pitch), shape: heroShape, color }}
                  size="hero"
                  live={false}
                  lookAt={{ yaw: 0, pitch }}
                  decorative
                />
              </span>
            ))}
          </div>
        </section>
      ) : null}

      {show("shapes") ? (
        <section className={styles.card}>
          <h2 className={styles.sectionTitle}>All 16 shapes (click to select hero)</h2>
          <div className={styles.shapeGrid}>
            {BOT_SHAPES.map((s) => (
              <button
                key={s.id}
                type="button"
                className={[styles.shapeTile, heroShape === s.id ? styles.shapeTileActive : ""].filter(Boolean).join(" ")}
                onClick={() => setHeroShape(s.id)}
              >
                <BotAvatar bot={{ id: `grid-${s.id}`, name: s.id, shape: s.id, color }} size="panel" live={false} decorative />
                <span className={styles.tileLabel}>{s.id}</span>
              </button>
            ))}
          </div>
        </section>
      ) : null}

      {show("states") ? (
        <section className={styles.card}>
          <h2 className={styles.sectionTitle}>Shapes x states</h2>
          <div className={styles.stateGrid}>
            <span />
            {BOT_AVATAR_STATES.map((st) => (
              <span key={st} className={styles.tileLabel}>
                {st}
              </span>
            ))}
            {BOT_SHAPES.map((s, row) => (
              <div key={s.id} style={{ display: "contents" }}>
                <span className={styles.tileLabel}>{s.id}</span>
                {BOT_AVATAR_STATES.map((st, col) => (
                  <span key={st} className={styles.stateCell}>
                    <BotAvatar
                      bot={{
                        id: `matrix-${s.id}-${st}`,
                        name: `${s.id} ${st}`,
                        shape: s.id,
                        color: CHARACTER_COLORS[(row + col) % CHARACTER_COLORS.length]
                      }}
                      size="lg"
                      state={st}
                      decorative
                    />
                  </span>
                ))}
              </div>
            ))}
          </div>
        </section>
      ) : null}

      {show("crowd") ? (
        <section className={styles.card}>
          <h2 className={styles.sectionTitle}>Crowd of 20 (seeded, never in sync)</h2>
          <div className={styles.crowd}>
            {Array.from({ length: 20 }, (_, i) => (
              <BotAvatar
                key={i}
                bot={{
                  id: `crowd-${i}`,
                  name: `crowd ${i}`,
                  shape: BOT_SHAPES[(i * 7) % BOT_SHAPES.length].id,
                  color: CHARACTER_COLORS[(i * 5) % CHARACTER_COLORS.length]
                }}
                size="md"
                decorative
              />
            ))}
          </div>
        </section>
      ) : null}

      {show("sizes") ? (
        <div className={styles.splitRow}>
          <section className={styles.card}>
            <h2 className={styles.sectionTitle}>Sizes (xs to hero)</h2>
            <div className={styles.sizeRow}>
              {SIZES.map((sz) => (
                <div key={sz} className={styles.sizeItem}>
                  <BotAvatar bot={{ id: `size-${sz}`, name: sz, shape: heroShape, color }} size={sz} live={false} decorative />
                  <span className={styles.tileLabel}>{sz}</span>
                </div>
              ))}
            </div>
          </section>

          <section className={styles.card}>
            <h2 className={styles.sectionTitle}>Sidebar list</h2>
            <div className={styles.listGrid}>
              {LIST_BOTS.map((item) => (
                <button key={item.id} type="button" className={styles.listItem}>
                  <BotAvatar
                    bot={{ id: item.id, name: item.name, shape: item.shape, color: item.color }}
                    size="md"
                    state={item.state}
                    decorative
                  />
                  <span className={styles.listMeta}>
                    <span className={styles.listName}>{item.name}</span>
                    <span className={styles.listRole}>{item.role}</span>
                  </span>
                </button>
              ))}
            </div>
          </section>
        </div>
      ) : null}
    </main>
  );
}
