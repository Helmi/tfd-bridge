import type { ShipDefinition } from '../types';

const classOrder: Record<ShipDefinition['shipClass'], number> = {
  carrier: 0,
  battleship: 1,
  cruiser: 2,
  destroyer: 3,
  submarine: 4,
};
const names = new Intl.Collator('en', { sensitivity: 'base', numeric: true });

/** Fixed battle order, independent of selection, visibility and sinking. */
export function compareFleetShips(a: ShipDefinition, b: ShipDefinition): number {
  return classOrder[a.shipClass] - classOrder[b.shipClass]
    || (b.tier ?? 0) - (a.tier ?? 0)
    || names.compare(a.shipName, b.shipName)
    || names.compare(a.playerName, b.playerName)
    || names.compare(a.id, b.id);
}
