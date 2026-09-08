import { consumableIconKey } from '../consumableIcons';
import type { SceneState } from '../types';

/** Only observed, living, currently spotted ships can reveal moving ranges. */
export function activeConsumableVisuals(state: SceneState) {
  const ships = new Map(state.ships.map(ship => [ship.definition.id, ship]));
  const seen = new Set<string>();
  return state.consumables.flatMap(({definition}) => {
    const ship = ships.get(definition.shipId);
    const iconKey = consumableIconKey(definition);
    if (!ship || ship.destroyed || ship.knowledge !== 'spotted' || !iconKey) return [];
    const key = `${definition.shipId}:${definition.name}`;
    if (seen.has(key)) return [];
    seen.add(key);
    const detection = definition.name === 'Radar' || definition.name === 'HydroacousticSearch';
    const radius = (value?: number) => value !== undefined && Number.isFinite(value) && value > 0 ? value : undefined;
    return [{ship, definition, iconKey,
      shipRadius: detection ? radius(definition.visual?.shipRadius) : undefined,
      torpedoRadius: definition.name === 'HydroacousticSearch' ? radius(definition.visual?.torpedoRadius) : undefined,
    }];
  });
}
