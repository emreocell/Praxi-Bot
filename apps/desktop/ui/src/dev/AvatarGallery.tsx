import { type CSSProperties, useEffect, useState } from "react";
import {
  applyTheme,
  BOT_AVATAR_STATES,
  BOT_SHAPES,
  BotAvatar,
  CHARACTER_COLORS,
  paletteById,
  type BotAvatarSize,
  type BotAvatarState,
  type BotShapeId
} from "ui";
import styles from "./AvatarGallery.module.css";

const GALLERY_THEMES = ["obsidian", "cobalt", "raven", "clay"] as const;
type GalleryThemeId = (typeof GALLERY_THEMES)[number];

const SIZES: readonly BotAvatarSize[] = ["xs", "sm", "md", "lg", "panel", "hero"];

const LIST_BOTS: readonly { id: string; name: string; role: string; shape: BotShapeId; color: string; state: BotAvatarState }[] = [
  { id: "gal-atlas", name: "Atlas", role: "Idle breathing (staggered)", shape: "gem", color: "#4c8dff", state: "idle" },
  { id: "gal-nova", name: "Nova", role: "Thinking through sources", shape: "diamond", color: "#2fc4b2", state: "thinking" },
  { id: "gal-mira", name: "Mira", role: "Working on pull request", shape: "cloud", color: "#f25fa8", state: "working" },
  { id: "gal-echo", name: "Echo", role: "Waiting for your approval", shape: "flower", color: "#f5803e", state: "waiting-approval" },
  { id: "gal-praxi", name: "Praxi", role: "Idle breathing (porcelain)", shape: "ghost", color: "#f4f4f5", state: "idle" },
  { id: "gal-byte", name: "Byte", role: "Tool call failed", shape: "cube", color: "#f2545b", state: "error" }
];

function initialTheme(): GalleryThemeId {
  const param = new URLSearchParams(window.location.search).get("theme");
  return (GALLERY_THEMES.find((id) => id === param) ?? "obsidian") as GalleryThemeId;
}

/**
 * Dev/mock-only gallery for reviewing bot avatar shapes, material shading,
 * hover/press feel, per-state animations, reduced motion and the 4 core themes.
 * Reachable only in dev or `VITE_MOCK=1` builds via `?mock=1&gallery=avatars`.
 */
export function AvatarGallery() {
  const [themeId, setThemeId] = useState<GalleryThemeId>(initialTheme);
  const [color, setColor] = useState<string>("#34c77b");
  const [heroShape, setHeroShape] = useState<BotShapeId>("pebble");
  const [heroState, setHeroState] = useState<BotAvatarState>("idle");
  const [reduced, setReduced] = useState<boolean>(
    () => new URLSearchParams(window.location.search).get("reduced") === "1"
  );

  useEffect(() => {
    const def = paletteById(themeId);
    if (def) {
      applyTheme(def, { style: "solid", corners: "soft" });
    }
  }, [themeId]);

  return (
    <main className={styles.page} data-reduced-motion={reduced ? "true" : undefined}>
      <header className={styles.header}>
        <div>
          <h1 className={styles.title}>Bot Avatar Motion &amp; Material Gallery</h1>
          <p className={styles.subtitle}>
            Dev-only review surface &mdash; hover or press any avatar to feel spring lift, squish &amp; eye reaction.
          </p>
        </div>
        <div className={styles.controls}>
          <div className={styles.pillRow} role="group" aria-label="Theme">
            {GALLERY_THEMES.map((id) => (
              <button
                key={id}
                type="button"
                className={[styles.pill, themeId === id ? styles.pillActive : ""].filter(Boolean).join(" ")}
                onClick={() => setThemeId(id)}
              >
                {id}
              </button>
            ))}
          </div>
          <button
            type="button"
            className={[styles.pill, reduced ? styles.pillActive : ""].filter(Boolean).join(" ")}
            aria-pressed={reduced}
            onClick={() => setReduced((v) => !v)}
          >
            {reduced ? "Reduced motion: ON" : "Reduced motion: OFF"}
          </button>
        </div>
      </header>

      <section className={styles.card}>
        <div className={styles.sectionHead}>
          <h2 className={styles.sectionTitle}>Hero Preview &amp; Tint Swatches</h2>
          <div className={styles.pillRow}>
            {BOT_AVATAR_STATES.map((st) => (
              <button
                key={st}
                type="button"
                className={[styles.pill, heroState === st ? styles.pillActive : ""].filter(Boolean).join(" ")}
                onClick={() => setHeroState(st)}
              >
                {st}
              </button>
            ))}
          </div>
        </div>
        <div className={styles.heroRow}>
          <button type="button" className={styles.heroStage} aria-label={`Hero ${heroShape}`}>
            <BotAvatar
              bot={{ id: `hero-${heroShape}`, name: heroShape, shape: heroShape, color }}
              size="hero"
              state={heroState}
              live
              decorative
            />
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

      <section className={styles.card}>
        <h2 className={styles.sectionTitle}>All 16 Shapes (Click to Select Hero)</h2>
        <div className={styles.shapeGrid}>
          {BOT_SHAPES.map((s) => (
            <button
              key={s.id}
              type="button"
              className={[styles.shapeTile, heroShape === s.id ? styles.shapeTileActive : ""].filter(Boolean).join(" ")}
              onClick={() => setHeroShape(s.id)}
            >
              <BotAvatar
                bot={{ id: `grid-${s.id}`, name: s.id, shape: s.id, color }}
                size="lg"
                live={false}
                decorative
              />
              <span className={styles.tileLabel}>{s.id}</span>
            </button>
          ))}
        </div>
      </section>

      <div className={styles.splitRow}>
        <section className={styles.card}>
          <h2 className={styles.sectionTitle}>States Side by Side</h2>
          <div className={styles.stateRow}>
            {BOT_AVATAR_STATES.map((st, idx) => (
              <button key={st} type="button" className={styles.stateTile}>
                <BotAvatar
                  bot={{
                    id: `state-${st}`,
                    name: st,
                    shape: BOT_SHAPES[idx * 2].id,
                    color: CHARACTER_COLORS[(idx + 3) % CHARACTER_COLORS.length]
                  }}
                  size="panel"
                  state={st}
                  decorative
                />
                <span className={styles.tileLabel}>{st}</span>
              </button>
            ))}
          </div>
        </section>

        <section className={styles.card}>
          <h2 className={styles.sectionTitle}>Sizes (xs &rarr; hero)</h2>
          <div className={styles.sizeRow}>
            {SIZES.map((sz) => (
              <div key={sz} className={styles.sizeItem}>
                <BotAvatar
                  bot={{ id: `size-${sz}`, name: sz, shape: heroShape, color }}
                  size={sz}
                  live={false}
                  decorative
                />
                <span className={styles.tileLabel}>{sz}</span>
              </div>
            ))}
          </div>
        </section>
      </div>

      <section className={styles.card}>
        <h2 className={styles.sectionTitle}>Sidebar List (Staggered Non-Lockstep Breathing &amp; Zero Per-Frame JS)</h2>
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
    </main>
  );
}
