import type { BotShapeId } from "./botShape";

import pebbleSm from "./assets/chars/pebble-sm.webp";
import pebbleLg from "./assets/chars/pebble-lg.webp";
import cubeSm from "./assets/chars/cube-sm.webp";
import cubeLg from "./assets/chars/cube-lg.webp";
import diamondSm from "./assets/chars/diamond-sm.webp";
import diamondLg from "./assets/chars/diamond-lg.webp";
import cloudSm from "./assets/chars/cloud-sm.webp";
import cloudLg from "./assets/chars/cloud-lg.webp";
import triSm from "./assets/chars/tri-sm.webp";
import triLg from "./assets/chars/tri-lg.webp";
import flowerSm from "./assets/chars/flower-sm.webp";
import flowerLg from "./assets/chars/flower-lg.webp";
import gemSm from "./assets/chars/gem-sm.webp";
import gemLg from "./assets/chars/gem-lg.webp";
import dropSm from "./assets/chars/drop-sm.webp";
import dropLg from "./assets/chars/drop-lg.webp";
import starSm from "./assets/chars/star-sm.webp";
import starLg from "./assets/chars/star-lg.webp";
import ghostSm from "./assets/chars/ghost-sm.webp";
import ghostLg from "./assets/chars/ghost-lg.webp";
import capsuleSm from "./assets/chars/capsule-sm.webp";
import capsuleLg from "./assets/chars/capsule-lg.webp";
import earsSm from "./assets/chars/ears-sm.webp";
import earsLg from "./assets/chars/ears-lg.webp";
import gumdropSm from "./assets/chars/gumdrop-sm.webp";
import gumdropLg from "./assets/chars/gumdrop-lg.webp";
import heartSm from "./assets/chars/heart-sm.webp";
import heartLg from "./assets/chars/heart-lg.webp";
import cloverSm from "./assets/chars/clover-sm.webp";
import cloverLg from "./assets/chars/clover-lg.webp";
import boulderSm from "./assets/chars/boulder-sm.webp";
import boulderLg from "./assets/chars/boulder-lg.webp";

/** Small (96px) and large (320px) transparent off-white 3D clay body WebPs. */
export const CHARACTER_ASSETS: Record<BotShapeId, { readonly sm: string; readonly lg: string }> = {
  pebble: { sm: pebbleSm, lg: pebbleLg },
  cube: { sm: cubeSm, lg: cubeLg },
  diamond: { sm: diamondSm, lg: diamondLg },
  cloud: { sm: cloudSm, lg: cloudLg },
  tri: { sm: triSm, lg: triLg },
  flower: { sm: flowerSm, lg: flowerLg },
  gem: { sm: gemSm, lg: gemLg },
  drop: { sm: dropSm, lg: dropLg },
  star: { sm: starSm, lg: starLg },
  ghost: { sm: ghostSm, lg: ghostLg },
  capsule: { sm: capsuleSm, lg: capsuleLg },
  ears: { sm: earsSm, lg: earsLg },
  gumdrop: { sm: gumdropSm, lg: gumdropLg },
  heart: { sm: heartSm, lg: heartLg },
  clover: { sm: cloverSm, lg: cloverLg },
  boulder: { sm: boulderSm, lg: boulderLg }
};
