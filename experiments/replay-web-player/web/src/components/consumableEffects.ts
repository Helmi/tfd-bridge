import { Graphics, Sprite, type Container, type Texture } from 'pixi.js';
import { activeConsumableVisuals } from '../engine/consumableVisuals';
import type { SceneState, WorldPoint } from '../types';

/** Kept behind tactical markers; these circles show capability, never targets. */
export function drawConsumableRanges(graphics: Graphics, effects: ReturnType<typeof activeConsumableVisuals>,
  size: number, project: (point: WorldPoint) => WorldPoint) {
  graphics.clear();
  for (const effect of effects) {
    const p = project(effect.ship.displayPose);
    const color = effect.definition.name === 'Radar' ? '#88bdb8' : '#bab18a';
    if (effect.shipRadius !== undefined) {
      graphics.circle(p.x, p.y, effect.shipRadius * size)
        .fill({color, alpha:.012}).stroke({color, width:1.25, alpha:.65});
    }
    // Hydro's torpedo-detection radius is distinct: faint dashed inner boundary.
    if (effect.torpedoRadius !== undefined && effect.torpedoRadius !== effect.shipRadius) {
      const radius = effect.torpedoRadius * size;
      const segments = Math.max(24, Math.ceil(radius * Math.PI / 8));
      for (let i=0;i<segments;i++) {
        const start = i * Math.PI * 2 / segments;
        graphics.beginPath().arc(p.x,p.y,radius,start,start+Math.PI*2/segments*.45)
          .stroke({color,width:1,alpha:.45});
      }
    }
  }
}

export function drawConsumableIcons(markers: Container, effects: ReturnType<typeof activeConsumableVisuals>,
  textures: Record<string, Texture>, viewport: {left:number;top:number;size:number},
  project: (point: WorldPoint) => WorldPoint, scaleFor: (ship: SceneState['ships'][number]) => number) {
  const groups = new Map<string, typeof effects>();
  for (const effect of effects) {
    const group = groups.get(effect.definition.shipId) ?? [];
    group.push(effect); groups.set(effect.definition.shipId, group);
  }
  for (const group of groups.values()) {
    group.sort((a,b)=>a.definition.name.localeCompare(b.definition.name));
    const p = project(group[0].ship.displayPose);
    const size = Math.max(14,Math.min(19,viewport.size*.018));
    const width = group.length*(size+2)-2;
    const x = Math.max(viewport.left,Math.min(viewport.left+viewport.size-width,p.x+17*scaleFor(group[0].ship)));
    const y = Math.max(viewport.top,Math.min(viewport.top+viewport.size-size,p.y-size/2));
    for (const [i,effect] of group.entries()) {
      const texture = textures[effect.iconKey];
      if (!texture) continue;
      const icon = new Sprite({texture,label:`consumable:${effect.definition.name}`});
      icon.width=size; icon.height=size; icon.position.set(x+i*(size+2),y); icon.alpha=1;
      markers.addChild(icon);
    }
  }
}
