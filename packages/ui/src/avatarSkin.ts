/**
 * Page-wide avatar skin: `render` (photoreal clay body renders, the default) or `procedural`
 * (the SVG clay drawn by the component). Set once from the app's Appearance setting; every
 * BotAvatar follows it unless it gets an explicit `skin` prop (gallery comparisons).
 */
export type AvatarSkin = 'render' | 'procedural';

export const AVATAR_SKINS: readonly AvatarSkin[] = ['render', 'procedural'];

let skin: AvatarSkin = 'render';
const listeners = new Set<() => void>();

export function getAvatarSkin(): AvatarSkin {
  return skin;
}

export function setAvatarSkin(next: AvatarSkin): void {
  if (next === skin) return;
  skin = next;
  for (const l of listeners) l();
}

export function subscribeAvatarSkin(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
