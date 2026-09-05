import type { ShipDefinition } from './types';

export const DIVISION_COLOR = '#ffd369';
export const SELECTED_SHIP_COLOR = '#ff69b4';

export function isOwnDivision(ship: ShipDefinition, ships: ShipDefinition[]): boolean {
  const recorder = ships.find((candidate) => candidate.relation === 'self');
  return Boolean(recorder && recorder.teamId === ship.teamId
    && recorder.divisionId && recorder.divisionId !== '0' && recorder.divisionId === ship.divisionId);
}

export function isDivisionMate(ship: ShipDefinition, ships: ShipDefinition[]): boolean {
  return ship.relation !== 'self' && isOwnDivision(ship, ships);
}

export function shipIconColor(ship: ShipDefinition, ships: ShipDefinition[], selectedId: string | undefined, teamColor: string): string {
  return ship.id === selectedId ? SELECTED_SHIP_COLOR : isOwnDivision(ship, ships) ? DIVISION_COLOR : teamColor;
}
