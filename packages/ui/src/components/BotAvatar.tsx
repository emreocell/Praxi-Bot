import { type CSSProperties, useEffect, useId, useRef, useState } from 'react';
import { botShape, fallbackBotColor, gradientMapConfig } from '../botShape';
import { CHARACTER_ASSETS } from '../characterAssets';
import { registerCharacterGaze } from '../eyeGaze';
import styles from './BotAvatar.module.css';

export type BotAvatarState = 'idle' | 'thinking' | 'working' | 'waiting-approval' | 'error';
export interface BotIdentity {
  id: string;
  name: string;
  color?: string;
  shape?: string | null;
  avatar?: string | null;
}

/**
 * One art slot for every bot surface: renders a gradient-mapped 3D clay
 * character with cursor-tracking tilted pill eyes, or uploaded art if set.
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
  size?: 'xs' | 'sm' | 'md' | 'lg' | 'hero';
  state?: BotAvatarState;
  /** Ambient bobbing and idle gestures. Defaults on for `lg` and `hero` when idle. */
  live?: boolean;
  /** Set when the bot's name is shown next to it: hidden from assistive tech. */
  decorative?: boolean;
  className?: string;
}) {
  const shape = botShape(bot.id, bot.shape);
  const gm = gradientMapConfig(bot.color || fallbackBotColor(bot.id));
  const [failedSrc, setFailedSrc] = useState<string | null>(null);
  const uid = useId().replace(/[^a-zA-Z0-9_-]/g, '');
  const filterId = `bot-gm-${uid}`;
  const characterRef = useRef<HTMLSpanElement | null>(null);
  const gazeRef = useRef<SVGGElement | null>(null);

  const hasCustomAvatar = Boolean(bot.avatar && bot.avatar !== failedSrc);
  const isLive = live ?? ((size === 'hero' || size === 'lg') && state === 'idle');

  useEffect(() => {
    if (hasCustomAvatar) {
      return undefined;
    }
    const characterEl = characterRef.current;
    const gazeEl = gazeRef.current;
    if (!characterEl || !gazeEl) {
      return undefined;
    }
    return registerCharacterGaze({
      characterEl,
      gazeEl,
      isLive,
      motionClasses: {
        hop: styles.hop,
        tilt: styles.tilt,
        stretch: styles.stretch
      }
    });
  }, [hasCustomAvatar, isLive]);

  const assetSrc =
    size === 'hero' || size === 'lg'
      ? CHARACTER_ASSETS[shape.id].lg
      : CHARACTER_ASSETS[shape.id].sm;
  const blinkDelay = `${-((shape.hash % 50) / 10).toFixed(1)}s`;
  const eyeStyle = { animationDelay: blinkDelay } as CSSProperties;

  return (
    <span
      className={[styles.avatar, styles[size], styles[state], className].filter(Boolean).join(' ')}
      style={{ '--bot-color': gm.color } as CSSProperties}
      {...(decorative
        ? { 'aria-hidden': true }
        : { role: 'img', 'aria-label': bot.name, title: bot.name })}
      data-state={state}
      data-shape={shape.id}
    >
      {hasCustomAvatar ? (
        <img
          className={styles.customImg}
          src={bot.avatar ?? ''}
          alt=""
          draggable={false}
          onError={() => setFailedSrc(bot.avatar ?? null)}
        />
      ) : (
        <span
          ref={characterRef}
          className={[styles.character, isLive ? styles.live : ''].filter(Boolean).join(' ')}
        >
          <svg className={styles.filterDefs} aria-hidden="true" focusable="false">
            <defs>
              <filter
                id={filterId}
                x="0"
                y="0"
                width="1"
                height="1"
                colorInterpolationFilters="sRGB"
              >
                <feColorMatrix type="matrix" values={gm.luminanceMatrix} />
                <feComponentTransfer>
                  <feFuncR type="linear" slope={gm.slope} intercept={gm.intercept} />
                  <feFuncG type="linear" slope={gm.slope} intercept={gm.intercept} />
                  <feFuncB type="linear" slope={gm.slope} intercept={gm.intercept} />
                </feComponentTransfer>
                <feComponentTransfer>
                  <feFuncR type="table" tableValues={gm.tableR} />
                  <feFuncG type="table" tableValues={gm.tableG} />
                  <feFuncB type="table" tableValues={gm.tableB} />
                </feComponentTransfer>
              </filter>
            </defs>
          </svg>
          <img
            className={styles.bodyImg}
            src={assetSrc}
            alt=""
            draggable={false}
            style={{ filter: `url(#${filterId})` }}
          />
          <svg className={styles.eyes} viewBox="0 0 64 64" aria-hidden="true" focusable="false">
            <g ref={gazeRef} className={styles.gaze}>
              {[shape.leftEyeX, shape.rightEyeX].map((cx, index) => (
                <g key={index} transform={`rotate(9 ${cx} ${shape.eyeY})`}>
                  <rect
                    className={styles.eye}
                    style={eyeStyle}
                    x={cx - 2.8}
                    y={shape.eyeY - 5.3}
                    width="5.6"
                    height="10.6"
                    rx="2.8"
                  />
                </g>
              ))}
            </g>
          </svg>
        </span>
      )}
    </span>
  );
}
