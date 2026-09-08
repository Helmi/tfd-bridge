import { Texture } from 'pixi.js';
import catalog from '../assets/maps/catalog.json';

interface MapAsset {
  id: string;
  sourceCompositeSha256: string;
  width: number;
  height: number;
  land: string;
  water: string;
}

export interface EnhancedMapTextureLease {
  texture: Texture;
  /** Release once this renderer no longer uses the texture. Safe to repeat. */
  release(): void;
}

interface CachedTexture {
  result: Promise<Texture | undefined>;
  texture?: Texture;
  references: number;
  lastUsed: number;
}

const maps = new Map<string, MapAsset>(catalog.version === 1
  ? catalog.maps.map(map => [map.id, map]) : []);
const textures = new Map<string, CachedTexture>();
const sourceChecks = new Map<string, Promise<boolean>>();
const MAX_IDLE_TEXTURES = 2;
const MAX_SOURCE_CHECKS = 4;
let lastUse = 0;

async function matchesSource(map: MapAsset, href: string): Promise<boolean> {
  const key = `${map.sourceCompositeSha256}\0${href}`;
  let result = sourceChecks.get(key);
  if (result) {
    sourceChecks.delete(key);
    sourceChecks.set(key, result);
    return result;
  }
  result = (async () => {
    const response = await fetch(href);
    if (!response.ok) throw new Error('Original map image is unavailable');
    const digest = await crypto.subtle.digest('SHA-256', await response.arrayBuffer());
    const hash = Array.from(new Uint8Array(digest), value => value.toString(16).padStart(2, '0')).join('');
    return hash === map.sourceCompositeSha256;
  })().catch(() => {
    // Retry transient failures next time; do not retain an unsuccessful load.
    if (sourceChecks.get(key) === result) sourceChecks.delete(key);
    return false;
  }).finally(() => {
    // Scene data/blob URLs identify immutable bytes. Ordinary URLs may serve
    // a newer game asset later, so only deduplicate their in-flight checks.
    if (!/^(data|blob):/i.test(href) && sourceChecks.get(key) === result) sourceChecks.delete(key);
  });
  sourceChecks.set(key, result);
  while (sourceChecks.size > MAX_SOURCE_CHECKS) sourceChecks.delete(sourceChecks.keys().next().value!);
  return result;
}

async function loadLayer(path: string, map: MapAsset): Promise<HTMLImageElement> {
  const image = new Image();
  const base = new URL(import.meta.env.BASE_URL, document.baseURI);
  image.src = new URL(path, base).href;
  await image.decode();
  if (image.naturalWidth !== map.width || image.naturalHeight !== map.height) {
    throw new Error('Bundled map layer has unexpected dimensions');
  }
  return image;
}

async function composeTexture(map: MapAsset): Promise<Texture> {
  const [water, land] = await Promise.all([loadLayer(map.water, map), loadLayer(map.land, map)]);
  const canvas = document.createElement('canvas');
  canvas.width = map.width;
  canvas.height = map.height;
  const context = canvas.getContext('2d');
  if (!context) throw new Error('Map composition canvas is unavailable');
  // The baked land alpha is the exact game mask. Draw both layers at their
  // native size: no runtime filtering, coastline generation or pixel edits.
  context.drawImage(water, 0, 0);
  context.drawImage(land, 0, 0);
  const texture = Texture.from(canvas);
  texture.source.scaleMode = 'linear';
  return texture;
}

function evictIdleTextures(): void {
  const idle = [...textures.entries()]
    .filter(([, entry]) => entry.references === 0 && entry.texture)
    .sort(([, a], [, b]) => a.lastUsed - b.lastUsed);
  for (const [key, entry] of idle.slice(0, Math.max(0, idle.length - MAX_IDLE_TEXTURES))) {
    textures.delete(key);
    entry.texture!.destroy(true);
  }
}

/** Acquire one shared, validated map texture. Unknown maps, changed game
 * assets or optional-layer failures return undefined so callers use the
 * original scene image. Active renderer leases are never evicted. */
export async function acquireEnhancedMapTexture(name: string, href: string): Promise<EnhancedMapTextureLease | undefined> {
  const map = maps.get(name);
  if (!map || !await matchesSource(map, href)) return undefined;
  let entry = textures.get(name);
  if (!entry) {
    const created: CachedTexture = {
      references: 0,
      lastUsed: ++lastUse,
      result: Promise.resolve(undefined),
    };
    created.result = composeTexture(map).then(texture => {
      created.texture = texture;
      return texture;
    }).catch(() => {
      if (textures.get(name) === created) textures.delete(name);
      return undefined;
    });
    entry = created;
    textures.set(name, entry);
  }
  // Reserve the reference before awaiting, including concurrent consumers
  // which are still loading. A release cannot destroy their pending texture.
  entry.references++;
  const texture = await entry.result;
  if (!texture) {
    entry.references--;
    return undefined;
  }
  let released = false;
  return {
    texture,
    release() {
      if (released) return;
      released = true;
      entry.references--;
      entry.lastUsed = ++lastUse;
      evictIdleTextures();
    },
  };
}
