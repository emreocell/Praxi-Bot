import type { CSSProperties } from 'react';
import { FolderOpen } from 'lucide-react';
import { brandIcon } from '../lib/brandIcons';
import styles from './BrandIcon.module.css';

export function BrandIcon({ name, small = false }: { name: string; small?: boolean }) {
  const brand = brandIcon(name);
  const key = name.toLowerCase().replace(/[^a-z]/g, '');
  const multicolor = ['gmail', 'googledrive', 'googlecalendar', 'figma'].includes(key);
  return <span className={`${styles.tile} ${small ? styles.small : ''}`} style={{ '--brand-color': brand.color } as CSSProperties} title={brand.name} aria-hidden="true">
    {multicolor ? <img src={`/brands/${key}.svg`} alt="" /> : key === 'filesystem' ? <FolderOpen /> : key === 'slack' ?
      <svg viewBox="0 0 24 24"><path fill="#36C5F0" d="M9 2a2 2 0 0 1 2 2v6H7V4a2 2 0 0 1 2-2M2 9a2 2 0 0 1 2-2h2v4H4a2 2 0 0 1-2-2"/><path fill="#2EB67D" d="M22 9a2 2 0 0 1-2 2h-6V7h6a2 2 0 0 1 2 2M15 2a2 2 0 0 1 2 2v2h-4V4a2 2 0 0 1 2-2"/><path fill="#ECB22E" d="M15 22a2 2 0 0 1-2-2v-6h4v6a2 2 0 0 1-2 2M22 15a2 2 0 0 1-2 2h-2v-4h2a2 2 0 0 1 2 2"/><path fill="#E01E5A" d="M2 15a2 2 0 0 1 2-2h6v4H4a2 2 0 0 1-2-2M9 22a2 2 0 0 1-2-2v-2h4v2a2 2 0 0 1-2 2"/></svg>
      : brand.path ? <svg viewBox="0 0 24 24"><path d={brand.path} /></svg> : <span>{brand.monogram}</span>}
  </span>;
}
