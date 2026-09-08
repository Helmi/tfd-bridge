import { Container, Graphics, Sprite, type Texture } from 'pixi.js';
import type { ShipClass } from '../shipClassIcons';
import type { WorldPoint } from '../types';

export const HULL_POINTS = [[0,-11],[5,-4],[4.5,8],[0,12],[-4.5,8],[-5,-4]];

export function createShipMarker(opts: {
  texture?: Texture; shipClass: ShipClass; position: WorldPoint; yaw: number;
  scale: number; color: string; alpha: number; selected: boolean;
}): Container {
  const {texture, scale, color, selected} = opts;
  const marker = new Container({label:'ship-marker'});
  marker.position.set(opts.position.x, opts.position.y);
  marker.rotation = opts.yaw * Math.PI / 180;
  marker.alpha = opts.alpha;
  if (texture) {
    const icon = new Sprite({texture, label:'ship-glyph'});
    icon.anchor.set(.5);
    icon.height = 16 * scale;
    icon.width = icon.height * texture.orig.width / texture.orig.height;
    icon.tint = color;
    marker.addChild(icon);
  } else {
    const hull = new Graphics({label:'ship-glyph'});
    hull.poly(HULL_POINTS.flatMap(([x,y]) => [x*scale,y*scale]))
      .fill({color}).stroke({color:'#eef4f1',width:selected ? 1.35 : .75,alpha:.8});
    marker.addChild(hull);
  }

  return marker;
}
