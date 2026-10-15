import { Assets, Sprite, type Texture } from 'pixi.js';
import { shipClassIconUrl, type ShipClass } from '../shipClassIcons';

const CLASSES: ShipClass[] = ['destroyer', 'cruiser', 'battleship', 'carrier', 'submarine'];

export type ClassIconTextures = Record<ShipClass, Texture>;

/** Load the five class icons. Called once per stage. */
export async function loadClassIcons(): Promise<ClassIconTextures> {
  const textures = await Promise.all(CLASSES.map(shipClass => Assets.load<Texture>(shipClassIconUrl(shipClass))));
  return Object.fromEntries(CLASSES.map((shipClass, index) => [shipClass, textures[index]])) as ClassIconTextures;
}

/** Centered sprite rotated 90 degrees, like the DOM `.ship-icon`. The caller sets tint and size. */
export function createClassIconSprite(textures: ClassIconTextures, shipClass: string): Sprite {
  const texture = textures[shipClass as ShipClass];
  if (!texture) throw new Error(`Unknown ship class "${shipClass}"`);
  const sprite = new Sprite(texture);
  sprite.anchor.set(0.5);
  sprite.rotation = Math.PI / 2;
  return sprite;
}
