/**
 * Shared SVG filters for every avatar, injected once per document into a hidden <svg>:
 * soft blurs for the inner shading and a fine felt/clay grain. Avatars reference them by id,
 * so 20 avatars cost one filter definition, not twenty.
 */
export const AVATAR_GRAIN_FILTER_ID = 'praxi-avatar-grain';
export const AVATAR_SOFT_FILTER_ID = 'praxi-avatar-soft';
export const AVATAR_WIDE_FILTER_ID = 'praxi-avatar-wide';

const HOST_ID = 'praxi-avatar-defs';

const DEFS_MARKUP = `
<filter id="${AVATAR_GRAIN_FILTER_ID}" x="0" y="0" width="100%" height="100%" color-interpolation-filters="sRGB">
  <feTurbulence type="fractalNoise" baseFrequency="2.3" numOctaves="2" seed="11" stitchTiles="stitch" result="n"/>
  <feColorMatrix in="n" type="matrix" values="0.33 0.33 0.33 0 0  0.33 0.33 0.33 0 0  0.33 0.33 0.33 0 0  0 0 0 0 1"/>
</filter>
<filter id="${AVATAR_SOFT_FILTER_ID}" x="-20%" y="-20%" width="140%" height="140%" color-interpolation-filters="sRGB">
  <feGaussianBlur stdDeviation="0.9"/>
</filter>
<filter id="${AVATAR_WIDE_FILTER_ID}" x="-20%" y="-20%" width="140%" height="140%" color-interpolation-filters="sRGB">
  <feGaussianBlur stdDeviation="3.2"/>
</filter>`;

/** Idempotently add the shared defs to the document (no-op outside the browser). */
export function ensureAvatarDefs(): void {
  if (typeof document === 'undefined' || !document.body) return;
  if (document.getElementById(HOST_ID)) return;
  const ns = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(ns, 'svg');
  svg.setAttribute('id', HOST_ID);
  svg.setAttribute('aria-hidden', 'true');
  svg.setAttribute('focusable', 'false');
  svg.setAttribute('width', '0');
  svg.setAttribute('height', '0');
  svg.style.position = 'absolute';
  svg.style.width = '0';
  svg.style.height = '0';
  svg.style.overflow = 'hidden';
  svg.style.pointerEvents = 'none';
  const defs = document.createElementNS(ns, 'defs');
  defs.innerHTML = DEFS_MARKUP;
  svg.appendChild(defs);
  document.body.appendChild(svg);
}
