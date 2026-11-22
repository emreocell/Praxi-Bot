import { type CSSProperties, useEffect, useId, useRef, useState, useSyncExternalStore } from 'react';
import {
  BOT_SHAPE_GEOMETRY,
  EYE_HEIGHT,
  EYE_WIDTH,
  botAvatarMaterial,
  botAvatarTiming,
  botShape,
  fallbackBotColor,
  fnv1a32,
  normalizeBotAvatarState,
  resolveAvatarMotion,
  type BotAvatarSize,
  type BotAvatarState
} from '../botShape';
import { shapeSway } from '../avatarBehavior';
import { AvatarLife, type AvatarFace } from '../avatarLife';
import { trackAvatarActivity } from '../avatarActivity';
import { AVATAR_GRAIN_FILTER_ID, AVATAR_SOFT_FILTER_ID, AVATAR_WIDE_FILTER_ID, ensureAvatarDefs } from '../avatarDefs';
import { isReducedMotionPreferred, subscribeReducedMotion } from '../theme';
import { FILM_RIM_COOL, FILM_RIM_WARM, renderLayout, renderTint, socketColors } from '../avatarRender';
import { RENDER_URLS } from '../avatarRenderAssets';
import { getAvatarSkin, subscribeAvatarSkin, type AvatarSkin } from '../avatarSkin';
import styles from './BotAvatar.module.css';

export type { AvatarSkin, BotAvatarSize, BotAvatarState };

export interface BotIdentity {
  id: string;
  name: string;
  color?: string;
  shape?: string | null;
  avatar?: string | null;
}

/** Sizes that render the full clay material (inner shading filters + grain). Smaller ones keep gradients only. */
const HI_FI_SIZES: ReadonlySet<BotAvatarSize> = new Set(['md', 'lg', 'panel', 'hero']);

/** Small avatars get relatively larger eyes so the face stays readable. */
const EYE_BOOST: Readonly<Record<BotAvatarSize, number>> = { xs: 1.4, sm: 1.28, md: 1.16, lg: 1.07, panel: 1.05, hero: 1 };

const pct = (v: number) => `${((v / 64) * 100).toFixed(3)}%`;

/**
 * One art slot for every bot surface: a soft-clay character (a photoreal clay body render
 * recoloured to the bot's tint, or the procedural SVG clay) with inset pill eyes that turn like a
 * head in 3D, a seeded idle behaviour schedule and calm state expressions, or custom uploaded
 * artwork when configured.
 */
export function BotAvatar({
  bot,
  size = 'md',
  state = 'idle',
  live,
  decorative = false,
  lookAt,
  skin,
  className
}: {
  bot: BotIdentity;
  size?: BotAvatarSize;
  state?: BotAvatarState;
  /** Ambient life (breathing, blinking, idle behaviours). Defaults on; featured sizes (`lg`, `panel`, `hero`) also follow a near pointer. Pass `false` for static pickers. */
  live?: boolean;
  /** Set when the bot's name is shown next to it: hidden from assistive tech. */
  decorative?: boolean;
  /** Fix the look direction in degrees (yaw: + right, pitch: + down), e.g. a cast looking at a headline. */
  lookAt?: { yaw: number; pitch: number } | null;
  /** Force a skin (gallery comparisons); defaults to the page-wide Appearance setting. */
  skin?: AvatarSkin;
  className?: string;
}) {
  const shape = botShape(bot.id, bot.shape);
  const geometry = BOT_SHAPE_GEOMETRY[shape.id];
  const material = botAvatarMaterial(bot.color || fallbackBotColor(bot.id));
  const timing = botAvatarTiming(bot.id);
  const canonicalState = normalizeBotAvatarState(state);
  const hiFi = HI_FI_SIZES.has(size);
  if (hiFi) ensureAvatarDefs();

  const [failedSrc, setFailedSrc] = useState<string | null>(null);
  const [renderFailed, setRenderFailed] = useState(false);
  const pageSkin = useSyncExternalStore(subscribeAvatarSkin, getAvatarSkin, getAvatarSkin);
  const useRender = (skin ?? pageSkin) === 'render' && !renderFailed;
  const layout = useRender ? renderLayout(shape.id) : null;
  const [reducedMotion, setReducedMotion] = useState<boolean>(() => isReducedMotionPreferred());
  const uid = useId().replace(/[^a-zA-Z0-9_-]/g, '');
  const rootRef = useRef<HTMLSpanElement | null>(null);
  const stageRef = useRef<HTMLSpanElement | null>(null);
  const actRef = useRef<HTMLSpanElement | null>(null);
  const bodyRef = useRef<HTMLSpanElement | null>(null);
  const shadowRef = useRef<HTMLSpanElement | null>(null);
  const eyeRefs = useRef<(HTMLSpanElement | null)[]>([]);
  const lidRefs = useRef<(HTMLSpanElement | null)[]>([]);
  const depthRefs = useRef<(HTMLSpanElement | null)[]>([]);
  const faceRef = useRef<HTMLSpanElement | null>(null);
  const lifeRef = useRef<AvatarLife | null>(null);

  useEffect(() => subscribeReducedMotion(setReducedMotion), []);
  // Preview surfaces (dev gallery) can force reduced motion on a subtree.
  useEffect(() => {
    if (rootRef.current?.closest('[data-reduced-motion="true"]')) setReducedMotion(true);
  }, []);

  const hasCustomAvatar = Boolean(bot.avatar && bot.avatar !== failedSrc);
  const motion = resolveAvatarMotion({ size, state: canonicalState, live, prefersReducedMotion: reducedMotion });
  const alive = live !== false && !motion.reducedMotion;

  // Face anchors: the render skin has its own table (the bodies differ from the SVG silhouettes).
  const leftEyeX = layout ? layout.leftEyeX : shape.leftEyeX;
  const rightEyeX = layout ? layout.rightEyeX : shape.rightEyeX;
  const eyeY = layout ? layout.eyeY : shape.eyeY;
  const eyeTilt = layout ? layout.eyeTilt : geometry.eyeTilt;
  const eyeScale = layout ? layout.eyeScale : geometry.eyeScale;
  const face: AvatarFace = {
    eyes: [
      { x: leftEyeX, y: eyeY },
      { x: rightEyeX, y: eyeY }
    ],
    center: { x: layout ? layout.faceCx : geometry.faceCx, y: layout ? layout.faceCy : geometry.faceCy },
    radius: layout ? layout.faceR : geometry.faceR,
    eyeTilt
  };
  const lifeOptions = {
    seed: fnv1a32(`${bot.id}:life`),
    state: canonicalState,
    alive,
    trackNear: motion.gaze || live === true,
    reducedMotion: motion.reducedMotion,
    lookAt: lookAt ?? null
  };

  // Pause CSS animation while the window is hidden or the avatar is scrolled off screen.
  useEffect(() => {
    const el = rootRef.current;
    return el ? trackAvatarActivity(el) : undefined;
  }, []);

  // One behaviour controller per mounted character; recreated only when the art changes.
  useEffect(() => {
    if (hasCustomAvatar) return undefined;
    const root = rootRef.current;
    const stage = stageRef.current;
    const act = actRef.current;
    const body = bodyRef.current;
    const shadow = shadowRef.current;
    const eyes = eyeRefs.current.filter((e): e is HTMLSpanElement => Boolean(e));
    const lids = lidRefs.current.filter((e): e is HTMLSpanElement => Boolean(e));
    const depths = depthRefs.current.filter((e): e is HTMLSpanElement => Boolean(e));
    if (!root || !stage || !act || !body || !shadow || eyes.length !== 2) return undefined;
    const life = new AvatarLife(
      { root, stage, act, body, shadow, eyes, lids, depths, face: faceRef.current },
      face,
      lifeOptions
    );
    lifeRef.current = life;
    return () => {
      life.destroy();
      lifeRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hasCustomAvatar, shape.id]);

  useEffect(() => {
    lifeRef.current?.update(face, lifeOptions);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    canonicalState,
    alive,
    lifeOptions.trackNear,
    motion.reducedMotion,
    lookAt?.yaw,
    lookAt?.pitch,
    leftEyeX,
    rightEyeX,
    eyeY,
    face.radius
  ]);

  const tint = layout ? renderTint(material.base) : null;
  const socket = socketColors(tint ? tint.body : material.mid);
  const groundPct = pct(layout ? layout.maxY : geometry.maxY);
  const shadowRx = layout ? layout.shadowRx : geometry.shadowRx;
  const shadowCx = layout ? (layout.minX + layout.maxX) / 2 : (geometry.minX + geometry.maxX) / 2;
  const avatarVars = {
    '--bot-color': material.base,
    '--avatar-ground': groundPct,
    '--avatar-breath-duration': `${timing.breathDurationSec}s`,
    '--avatar-breath-delay': `${timing.breathDelaySec}s`,
    '--avatar-shadow-width': `${((shadowRx * 2) / 0.64).toFixed(1)}%`,
    '--avatar-shadow-left': pct(shadowCx),
    // The render skin lifts dark tints, so pick the eye ink against the body actually shown.
    '--eye-ink': tint ? botAvatarMaterial(tint.body).eyeColor : material.eyeColor,
    '--eye-lip': socket.lip,
    '--eye-shade': socket.shade
  } as CSSProperties;

  const sway = shapeSway(shape.id);
  const eyeW = EYE_WIDTH * eyeScale * EYE_BOOST[size];
  const eyeH = EYE_HEIGHT * eyeScale * EYE_BOOST[size];
  const renderUrl = layout ? RENDER_URLS[layout.render] : '';

  const ids = {
    body: `ba-body-${uid}`,
    spec: `ba-spec-${uid}`,
    occl: `ba-occl-${uid}`,
    clip: `ba-clip-${uid}`,
    core: `ba-core-${uid}`,
    rim: `ba-rim-${uid}`,
    bounce: `ba-bounce-${uid}`
  };
  const w = geometry.maxX - geometry.minX;
  const h = geometry.maxY - geometry.minY;
  const lightX = geometry.minX + w * 0.3;
  const lightY = geometry.minY + h * 0.18;
  const reach = Math.max(w, h) * 1.02;

  const crescent = (id: string, dx: number, dy: number) => (
    <mask id={id} maskUnits="userSpaceOnUse" x="-8" y="-8" width="80" height="80">
      <rect x="-8" y="-8" width="80" height="80" fill="#fff" />
      <path d={geometry.path} transform={`translate(${dx} ${dy})`} fill="#000" />
    </mask>
  );

  return (
    <span
      className={[
        styles.avatar,
        styles[size],
        styles[canonicalState],
        alive ? styles.alive : '',
        alive && sway !== 'none' ? styles[`sway-${sway}`] : '',
        className
      ]
        .filter(Boolean)
        .join(' ')}
      ref={rootRef}
      style={avatarVars}
      {...(decorative ? { 'aria-hidden': true } : { role: 'img', 'aria-label': bot.name, title: bot.name })}
      data-state={canonicalState}
      data-shape={shape.id}
      data-skin={useRender ? 'render' : 'procedural'}
    >
      {hasCustomAvatar ? (
        <span className={styles.stage}>
          <img
            className={styles.customImg}
            src={bot.avatar ?? ''}
            alt=""
            draggable={false}
            onError={() => setFailedSrc(bot.avatar ?? null)}
          />
        </span>
      ) : (
        <>
          <span ref={shadowRef} className={styles.groundShadow} aria-hidden="true">
            <span className={styles.groundShadowInner} />
          </span>
          <span ref={stageRef} className={styles.stage}>
            <span className={styles.press}>
              <span ref={actRef} className={styles.act}>
                <span className={styles.breath}>
                  <span className={styles.sway}>
                    <span ref={bodyRef} className={styles.body}>
                      {layout && tint ? (
                        <span
                          className={[styles.render, hiFi ? styles.renderHi : ''].filter(Boolean).join(' ')}
                          style={
                            {
                              left: pct(layout.imgX),
                              top: pct(layout.imgY),
                              width: pct(layout.imgSize),
                              height: pct(layout.imgSize),
                              '--render-mask': `url("${renderUrl}")`,
                              '--render-gain': layout.gain,
                              '--tint-mul': tint.multiply,
                              '--tint-glow': tint.glow,
                              '--tint-lift': tint.lift,
                              '--tint-lift-opacity': tint.liftOpacity,
                              '--rim-warm': FILM_RIM_WARM,
                              '--rim-cool': FILM_RIM_COOL
                            } as CSSProperties
                          }
                        >
                          <img
                            className={styles.renderImg}
                            src={renderUrl}
                            alt=""
                            draggable={false}
                            decoding="async"
                            onError={() => setRenderFailed(true)}
                          />
                          <span className={styles.renderTint} />
                          {hiFi ? (
                            <>
                              <img className={styles.renderGlow} src={renderUrl} alt="" draggable={false} decoding="async" />
                              {tint.liftOpacity > 0 ? <span className={styles.renderLift} /> : null}
                              <span className={styles.rim}>
                                <span className={styles.rimWarm} />
                                <span className={styles.rimCool} />
                              </span>
                            </>
                          ) : null}
                        </span>
                      ) : (
                      <svg className={styles.bodySvg} viewBox="0 0 64 64" aria-hidden="true" focusable="false">
                        <defs>
                          <radialGradient
                            id={ids.body}
                            gradientUnits="userSpaceOnUse"
                            cx={lightX}
                            cy={lightY}
                            r={reach}
                            fx={lightX - 1}
                            fy={lightY - 1}
                          >
                            <stop offset="0%" stopColor={material.highlight} />
                            <stop offset="38%" stopColor={material.mid} />
                            <stop offset="78%" stopColor={material.shadow} />
                            <stop offset="100%" stopColor={material.deepShadow} />
                          </radialGradient>
                          <radialGradient
                            id={ids.spec}
                            gradientUnits="userSpaceOnUse"
                            cx={geometry.sheenCx}
                            cy={geometry.sheenCy}
                            r={Math.max(geometry.sheenRx, geometry.sheenRy) * 1.25}
                            gradientTransform={`translate(${geometry.sheenCx} ${geometry.sheenCy}) rotate(-28) scale(1 0.72) translate(${-geometry.sheenCx} ${-geometry.sheenCy})`}
                          >
                            <stop offset="0%" stopColor={material.sheen} stopOpacity={material.sheenOpacity} />
                            <stop offset="55%" stopColor={material.sheen} stopOpacity={material.sheenOpacity * 0.35} />
                            <stop offset="100%" stopColor={material.sheen} stopOpacity="0" />
                          </radialGradient>
                          <linearGradient id={ids.occl} gradientUnits="userSpaceOnUse" x1="0" y1={geometry.minY + h * 0.55} x2="0" y2={geometry.maxY}>
                            <stop offset="0%" stopColor={material.deepShadow} stopOpacity="0" />
                            <stop offset="100%" stopColor={material.deepShadow} stopOpacity={hiFi ? 0.32 : 0.4} />
                          </linearGradient>
                          <clipPath id={ids.clip}>
                            <path d={geometry.path} />
                          </clipPath>
                          {hiFi ? crescent(ids.core, -2.4, -4.8) : null}
                          {hiFi ? crescent(ids.rim, 1.1, 1.5) : null}
                          {hiFi ? crescent(ids.bounce, 0.4, -1.3) : null}
                        </defs>
                        <path d={geometry.path} fill={`url(#${ids.body})`} />
                        <g clipPath={`url(#${ids.clip})`}>
                          <rect x="0" y="0" width="64" height="64" fill={`url(#${ids.occl})`} />
                          {hiFi ? (
                            <>
                              <g filter={`url(#${AVATAR_WIDE_FILTER_ID})`} opacity="0.5">
                                <rect x="-8" y="-8" width="80" height="80" fill={material.deepShadow} mask={`url(#${ids.core})`} />
                              </g>
                              <g filter={`url(#${AVATAR_SOFT_FILTER_ID})`} opacity="0.5">
                                <rect x="-8" y="-8" width="80" height="80" fill={material.bounceLight} mask={`url(#${ids.bounce})`} />
                              </g>
                              <g filter={`url(#${AVATAR_SOFT_FILTER_ID})`} opacity={material.rimOpacity}>
                                <rect x="-8" y="-8" width="80" height="80" fill={material.rimLight} mask={`url(#${ids.rim})`} />
                              </g>
                            </>
                          ) : null}
                          <rect x="0" y="0" width="64" height="64" fill={`url(#${ids.spec})`} />
                          {hiFi ? (
                            <rect
                              className={styles.grain}
                              x={geometry.minX}
                              y={geometry.minY}
                              width={w}
                              height={h}
                              filter={`url(#${AVATAR_GRAIN_FILTER_ID})`}
                              opacity={material.grainOpacity}
                            />
                          ) : null}
                        </g>
                        <path
                          d={geometry.path}
                          fill="none"
                          stroke={material.contour}
                          strokeOpacity={material.contourOpacity}
                          strokeWidth="0.6"
                        />
                      </svg>
                      )}
                    </span>
                    <span ref={faceRef} className={styles.face} aria-hidden="true">
                      {[leftEyeX, rightEyeX].map((cx, index) => (
                        <span
                          key={index}
                          ref={(el) => {
                            eyeRefs.current[index] = el;
                          }}
                          className={styles.eye}
                          style={{ transformOrigin: `${pct(cx)} ${pct(eyeY)}` }}
                        >
                          <span
                            className={styles.pill}
                            style={{
                              left: pct(cx - eyeW / 2),
                              top: pct(eyeY - eyeH / 2),
                              width: pct(eyeW),
                              height: pct(eyeH),
                              transform: eyeTilt ? `rotate(${eyeTilt}deg)` : undefined
                            }}
                          >
                            <span
                              ref={(el) => {
                                lidRefs.current[index] = el;
                              }}
                              className={styles.lid}
                            >
                              {hiFi ? (
                                <span
                                  ref={(el) => {
                                    depthRefs.current[index] = el;
                                  }}
                                  className={styles.depth}
                                />
                              ) : null}
                            </span>
                          </span>
                        </span>
                      ))}
                    </span>
                  </span>
                </span>
              </span>
            </span>
          </span>
        </>
      )}
    </span>
  );
}
