import { useState, type CSSProperties } from 'react';
import { botShape } from '../botShape';
import styles from './BotAvatar.module.css';

export type BotAvatarState = 'idle' | 'thinking' | 'working' | 'waiting-approval' | 'error';
export interface BotIdentity { id: string; name: string; color?: string; avatar?: string | null }

/** One art slot for every bot surface. Uploaded character art replaces the SVG. */
export function BotAvatar({ bot, size = 'md', state = 'idle' }: {
  bot: BotIdentity; size?: 'xs' | 'sm' | 'md' | 'lg' | 'hero'; state?: BotAvatarState;
}) {
  const shape = botShape(bot.id);
  const [failedSrc, setFailedSrc] = useState<string | null>(null);
  return <span className={`${styles.avatar} ${styles[size]} ${styles[state]}`} style={{ '--bot-color': bot.color || 'var(--color-accent)' } as CSSProperties} role="img" aria-label={bot.name} title={bot.name} data-state={state}>
    {bot.avatar && bot.avatar !== failedSrc ? <img src={bot.avatar} alt="" onError={() => setFailedSrc(bot.avatar ?? null)} /> :
      <svg viewBox="0 0 72 72" aria-hidden="true">
        <path className={styles.body} d={shape.path} />
        <path className={styles.sheen} d={shape.path} />
        <g className={styles.eyes}>
          <rect x={36 - shape.eyeGap / 2 - 3} y={shape.eyeY} width="4" height="7" rx="2" />
          <rect x={36 + shape.eyeGap / 2 - 1} y={shape.eyeY} width="4" height="7" rx="2" />
        </g>
      </svg>}
  </span>;
}
