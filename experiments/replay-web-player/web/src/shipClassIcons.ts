import type { ShipDefinition } from './types';

export type ShipClass = ShipDefinition['shipClass'];

const files: Record<ShipClass, string> = {
  destroyer: 'destroyer.svg',
  cruiser: 'cruiser.svg',
  battleship: 'battleship.svg',
  carrier: 'carrier.svg',
  submarine: 'submarine.svg',
};

export const shipClassNames: Record<ShipClass, string> = {
  destroyer: 'Destroyer',
  cruiser: 'Cruiser',
  battleship: 'Battleship',
  carrier: 'Aircraft carrier',
  submarine: 'Submarine',
};

export function shipClassIconUrl(shipClass: ShipClass): string {
  return `${import.meta.env.BASE_URL}assets/ship-classes/${files[shipClass]}`;
}
