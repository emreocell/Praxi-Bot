/**
 * Bundler-imported (hashed, cached) URLs of the 16 photoreal clay body renders: 512 px
 * transparent webp, neutral warm clay, same camera and light. Kept apart from `avatarRender.ts`
 * so the maths stays importable in node tests.
 */
import type { RenderName } from './avatarRender';
import cloud from './assets/renders/cloud.webp';
import clover from './assets/renders/clover.webp';
import diamond from './assets/renders/diamond.webp';
import drop from './assets/renders/drop.webp';
import egg from './assets/renders/egg.webp';
import flower from './assets/renders/flower.webp';
import ghost from './assets/renders/ghost.webp';
import gumdrop from './assets/renders/gumdrop.webp';
import heart from './assets/renders/heart.webp';
import hexgem from './assets/renders/hexgem.webp';
import pillow from './assets/renders/pillow.webp';
import sphere from './assets/renders/sphere.webp';
import squircle from './assets/renders/squircle.webp';
import star from './assets/renders/star.webp';
import tiltcube from './assets/renders/tiltcube.webp';
import tri from './assets/renders/tri.webp';

export const RENDER_URLS: Readonly<Record<RenderName, string>> = {
  cloud,
  clover,
  diamond,
  drop,
  egg,
  flower,
  ghost,
  gumdrop,
  heart,
  hexgem,
  pillow,
  sphere,
  squircle,
  star,
  tiltcube,
  tri
};
