import { type CSSProperties, useEffect, useId, useRef, useState } from 'react';
import {
  BOT_SHAPE_GEOMETRY,
  botAvatarMaterial,
  botAvatarTiming,
  botShape,
  fallbackBotColor,
  normalizeBotAvatarState,
  resolveAvatarMotion,
  type BotAvatarSize,
  type BotAvatarState
} from '../botShape';
import { registerCharacterGaze } from '../eyeGaze';
import { isReducedMotionPreferred, subscribeReducedMotion } from '../theme';
import styles from './BotAvatar.module.css';

export type { BotAvatarSize, BotAvatarState };

export interface BotIdentity {
  id: string;
  name: string;
  color?: string;
  shape?: string | null;
  avatar?: string | null;
}

/**
 * One art slot for every bot surface: renders a procedural satin-matte vector
 * character with layered studio shading, expressive living eyes, and tactile
 * spring motion — or custom uploaded artwork when configured.
 */
export function BotAvatar({
  bot,
  size = 'md',
  state = 'idle',
  live,
  decorative = false,
  className
}: {
  bot: BotIdentity;
  size?: BotAvatarSize;
  state?: BotAvatarState;
  /** Ambient breathing and cursor gaze. Defaults to staggered breathing in lists and gaze on featured sizes (`lg`, `panel`, `hero`). Pass `false` for static pickers. */
  live?: boolean;
  /** Set when the bot's name is shown next to it: hidden from assistive tech. */
  decorative?: boolean;
  className?: string;
}) {
  const shape = botShape(bot.id, bot.shape);
  const geometry = BOT_SHAPE_GEOMETRY[shape.id];
  const material = botAvatarMaterial(bot.color || fallbackBotColor(bot.id));
  const timing = botAvatarTiming(bot.id);
  const canonicalState = normalizeBotAvatarState(state);

  const [failedSrc, setFailedSrc] = useState<string | null>(null);
  const [reducedMotion, setReducedMotion] = useState<boolean>(() => isReducedMotionPreferred());
  const uid = useId().replace(/[^a-zA-Z0-9_-]/g, '');
  const gazeRef = useRef<HTMLSpanElement | null>(null);

  useEffect(() => subscribeReducedMotion(setReducedMotion), []);

  const hasCustomAvatar = Boolean(bot.avatar && bot.avatar !== failedSrc);
  const motion = resolveAvatarMotion({
    size,
    state: canonicalState,
    live,
    prefersReducedMotion: reducedMotion
  });

  useEffect(() => {
    if (hasCustomAvatar || !motion.gaze) {
      return undefined;
    }
    const gazeEl = gazeRef.current;
    if (!gazeEl) {
      return undefined;
    }
    return registerCharacterGaze({ gazeEl });
  }, [hasCustomAvatar, motion.gaze]);

  const bodyGradId = `ba-body-${uid}`;
  const sheenGradId = `ba-sheen-${uid}`;
  const rimGradId = `ba-rim-${uid}`;
  const bounceGradId = `ba-bounce-${uid}`;
  const clipId = `ba-clip-${uid}`;

  const avatarVars = {
    '--bot-color': material.base,
    '--avatar-breath-duration': `${timing.breathDurationSec}s`,
    '--avatar-breath-delay': `${timing.breathDelaySec}s`,
    '--avatar-blink-duration': `${timing.blinkDurationSec}s`,
    '--avatar-blink-delay': `${timing.blinkDelaySec}s`
  } as CSSProperties;

  const eyeW = Number((4.8 * geometry.eyeScale).toFixed(2));
  const eyeH = Number((9.2 * geometry.eyeScale).toFixed(2));
  const eyeRx = Number((eyeW / 2).toFixed(2));
  const catchR = Number((1.05 * geometry.eyeScale).toFixed(2));
  const showCatchlight = material.catchlightOpacity > 0 && size !== 'xs';
  const tiltTransform = geometry.tilt !== 0 ? `rotate(${geometry.tilt} 32 32)` : undefined;
  const counterTiltTransform = geometry.tilt !== 0 ? `rotate(${-geometry.tilt} 32 32)` : undefined;

  return (
    <span
      className={[
        styles.avatar,
        styles[size],
        styles[canonicalState],
        motion.breathe ? styles.breathing : '',
        motion.blink ? styles.blinking : '',
        className
      ]
        .filter(Boolean)
        .join(' ')}
      style={avatarVars}
      {...(decorative
        ? { 'aria-hidden': true }
        : { role: 'img', 'aria-label': bot.name, title: bot.name })}
      data-state={canonicalState}
      data-shape={shape.id}
    >
      {hasCustomAvatar ? (
        <span className={styles.stage}>
          <span className={styles.pose}>
            <img
              className={styles.customImg}
              src={bot.avatar ?? ''}
              alt=""
              draggable={false}
              onError={() => setFailedSrc(bot.avatar ?? null)}
            />
          </span>
        </span>
      ) : (
        <>
          <span
            className={styles.groundShadow}
            style={{ '--shadow-width': `${Math.round((geometry.shadowRx * 2) / 0.64)}%` } as CSSProperties}
            aria-hidden="true"
          />
          <span className={styles.stage}>
            <span className={styles.pose}>
              <svg
                className={styles.bodySvg}
                viewBox="0 0 64 64"
                aria-hidden="true"
                focusable="false"
              >
                <defs>
                  <radialGradient
                    id={bodyGradId}
                    gradientUnits="userSpaceOnUse"
                    cx="23"
                    cy="17"
                    r="44"
                    fx="21"
                    fy="15"
                    gradientTransform={counterTiltTransform}
                  >
                    <stop offset="0%" stopColor={material.highlight} />
                    <stop offset="48%" stopColor={material.mid} />
                    <stop offset="84%" stopColor={material.shadow} />
                    <stop offset="100%" stopColor={material.deepShadow} />
                  </radialGradient>
                  <radialGradient
                    id={sheenGradId}
                    gradientUnits="userSpaceOnUse"
                    cx={geometry.sheenCx}
                    cy={geometry.sheenCy}
                    r={Math.max(geometry.sheenRx, geometry.sheenRy) * 1.15}
                  >
                    <stop offset="0%" stopColor={material.sheen} stopOpacity={material.sheenOpacity} />
                    <stop offset="65%" stopColor={material.sheen} stopOpacity={material.sheenOpacity * 0.28} />
                    <stop offset="100%" stopColor={material.sheen} stopOpacity="0" />
                  </radialGradient>
                  <linearGradient
                    id={rimGradId}
                    gradientUnits="userSpaceOnUse"
                    x1="12"
                    y1="8"
                    x2="50"
                    y2="54"
                    gradientTransform={counterTiltTransform}
                  >
                    <stop offset="0%" stopColor={material.rimLight} stopOpacity={material.rimOpacity} />
                    <stop offset="38%" stopColor={material.rimLight} stopOpacity={material.rimOpacity * 0.25} />
                    <stop offset="72%" stopColor={material.rimLight} stopOpacity="0" />
                    <stop offset="100%" stopColor={material.bounceLight} stopOpacity="0.16" />
                  </linearGradient>
                  <radialGradient
                    id={bounceGradId}
                    gradientUnits="userSpaceOnUse"
                    cx="44"
                    cy="52"
                    r="24"
                    gradientTransform={counterTiltTransform}
                  >
                    <stop offset="0%" stopColor={material.bounceLight} stopOpacity="0.2" />
                    <stop offset="100%" stopColor={material.bounceLight} stopOpacity="0" />
                  </radialGradient>
                  <clipPath id={clipId}>
                    <path d={geometry.path} />
                  </clipPath>
                </defs>
                <g transform={tiltTransform}>
                  <path d={geometry.path} fill={`url(#${bodyGradId})`} />
                  <g clipPath={`url(#${clipId})`}>
                    <ellipse
                      cx={geometry.sheenCx}
                      cy={geometry.sheenCy}
                      rx={geometry.sheenRx}
                      ry={geometry.sheenRy}
                      transform={`rotate(-22 ${geometry.sheenCx} ${geometry.sheenCy})`}
                      fill={`url(#${sheenGradId})`}
                    />
                    <rect x="0" y="0" width="64" height="64" fill={`url(#${bounceGradId})`} />
                    <path
                      d={geometry.path}
                      fill="none"
                      stroke={`url(#${rimGradId})`}
                      strokeWidth="2.4"
                      strokeLinejoin="round"
                    />
                  </g>
                  <path
                    d={geometry.path}
                    fill="none"
                    stroke={material.contour}
                    strokeOpacity={material.contourOpacity}
                    strokeWidth="0.9"
                    strokeLinejoin="round"
                  />
                </g>
              </svg>

              <span className={styles.eyeState} aria-hidden="true">
                <span className={styles.eyeReact}>
                  <span ref={gazeRef} className={styles.eyeGaze}>
                    <svg
                      className={styles.eyesSvg}
                      viewBox="0 0 64 64"
                      aria-hidden="true"
                      focusable="false"
                    >
                      {[shape.leftEyeX, shape.rightEyeX].map((cx, index) => (
                        <g
                          key={index}
                          transform={
                            geometry.eyeTilt !== 0
                              ? `rotate(${geometry.eyeTilt} ${cx} ${shape.eyeY})`
                              : undefined
                          }
                        >
                          <g className={styles.eye}>
                            <rect
                              x={cx - eyeW / 2}
                              y={shape.eyeY - eyeH / 2}
                              width={eyeW}
                              height={eyeH}
                              rx={eyeRx}
                              fill={material.eyeColor}
                            />
                            {showCatchlight ? (
                              <circle
                                cx={cx - eyeW * 0.16}
                                cy={shape.eyeY - eyeH * 0.22}
                                r={catchR}
                                fill={material.rimLight}
                                opacity={material.catchlightOpacity}
                              />
                            ) : null}
                          </g>
                        </g>
                      ))}
                    </svg>
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
