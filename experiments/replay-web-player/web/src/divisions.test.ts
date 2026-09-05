import { describe, expect, it } from 'vitest';
import type { ShipDefinition } from './types';
import { isDivisionMate, shipIconColor, DIVISION_COLOR, SELECTED_SHIP_COLOR } from './divisions';

const recorder: ShipDefinition = {
  id: 'self', teamId: 'allies', relation: 'self', divisionId: '123', divisionLabel: 'A',
  playerName: 'Recorder', shipName: 'Daring', shipClass: 'destroyer', maxHealth: 100,
  pose: [], health: [], knowledge: [],
};

describe('replay division membership', () => {
  it('selects pink before division yellow, including the recorder when unselected', () => {
    const mate = { ...recorder, id: 'mate', relation: 'ally' as const };
    const ships = [recorder, mate];
    expect(shipIconColor(recorder, ships, 'mate', 'green')).toBe(DIVISION_COLOR);
    expect(shipIconColor(mate, ships, 'mate', 'green')).toBe(SELECTED_SHIP_COLOR);
    expect(shipIconColor(mate, ships, 'self', 'green')).toBe(DIVISION_COLOR);
    expect(shipIconColor({ ...mate, teamId: 'enemy' }, ships, 'mate', 'red')).toBe(SELECTED_SHIP_COLOR);
    const solo = { ...recorder, divisionId: undefined };
    expect(shipIconColor(solo, [solo], undefined, 'green')).toBe('green');
  });
  it('uses membership and team, not letters or the selected ship', () => {
    const mate = { ...recorder, id: 'mate', relation: 'ally' as const };
    expect(isDivisionMate(mate, [recorder, mate])).toBe(true);
    expect(isDivisionMate(recorder, [recorder, mate])).toBe(false);
    expect(isDivisionMate({ ...mate, divisionId: '456' }, [recorder])).toBe(false);
    expect(isDivisionMate({ ...mate, teamId: 'enemy', relation: 'enemy' }, [recorder])).toBe(false);
  });

  it('does not invent membership for solo players or older scenes', () => {
    expect(isDivisionMate({ ...recorder, id: 'mate' }, [])).toBe(false);
    for (const divisionId of [undefined, '0']) {
      expect(isDivisionMate({ ...recorder, id: 'mate', divisionId }, [{ ...recorder, divisionId }])).toBe(false);
    }
  });
});
