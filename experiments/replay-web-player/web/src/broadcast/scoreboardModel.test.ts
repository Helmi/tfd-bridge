import { describe, expect, it } from 'vitest';
import { sampleScene } from '../data/sampleScene';
import { compareFleetShips } from '../engine/fleetOrder';
import { evaluateScene } from '../engine/timeline';
import { DIVISION_COLOR, SELECTED_SHIP_COLOR } from '../divisions';
import type { EvaluatedShip, ReplayScene } from '../types';
import { buildScoreboardModel, type ScoreboardInput } from './scoreboardModel';

const COLORS = { friendly: '#67d9a3', enemy: '#fa8274' };
const ownerShip = sampleScene.ships[0];
const baseScene: ReplayScene = { ...sampleScene, replay: { ...sampleScene.replay, perspectiveEntityId: ownerShip.id } };

function input(scene: ReplayScene, time: number, selectedId = ownerShip.id): ScoreboardInput {
  const state = evaluateScene(scene, Math.min(time, scene.replay.duration));
  const ownerTeamId = ownerShip.teamId;
  const order = (a: EvaluatedShip, b: EvaluatedShip) => compareFleetShips(a.definition, b.definition);
  return {
    scene, time, state, selectedId, ownerTeamId,
    ally: state.ships.filter(s => s.definition.teamId === ownerTeamId).sort(order),
    enemy: state.ships.filter(s => s.definition.teamId !== ownerTeamId).sort(order),
  };
}
const model = (time: number, scene = baseScene, selectedId?: string) => buildScoreboardModel(input(scene, time, selectedId), COLORS);

describe('scoreboard model', () => {
  it('reads the score of the owner team and of the enemy team', () => {
    expect(model(0)).toMatchObject({ friendlyScore: 300, enemyScore: 300 });
    expect(model(250)).toMatchObject({ friendlyScore: 518, enemyScore: 472 });
    expect(model(420)).toMatchObject({ friendlyScore: 826, enemyScore: 702 });
  });

  it('scores 0 for an empty enemy side', () => {
    const base = input(baseScene, 250);
    expect(buildScoreboardModel({ ...base, enemy: [] }, COLORS).enemyScore).toBe(0);
  });

  it('lists survivors per team, the enemy strip mirrored, with the dead state', () => {
    const start = model(0);
    const ids = (ships: { id: string }[]) => ships.map(s => s.id);
    const base = input(baseScene, 0);
    expect(ids(start.friendlyShips)).toEqual(base.ally.map(s => s.definition.id));
    expect(ids(start.enemyShips)).toEqual(base.enemy.map(s => s.definition.id).reverse());
    expect(start.friendlyShips[0]).toMatchObject({ shipName: 'Midway', shipClass: 'carrier', dead: false });
    expect([...start.friendlyShips, ...start.enemyShips].some(s => s.dead)).toBe(false);
    const late = model(345);
    expect(late.friendlyShips.find(s => s.shipName === 'Hindenburg')?.dead).toBe(true);
    expect([...late.friendlyShips, ...late.enemyShips].filter(s => s.dead)).toHaveLength(4);
  });

  it('colours icons: team colour, own division gold, selected ship pink', () => {
    const mate = baseScene.ships[1];
    const enemyShip = baseScene.ships.find(s => s.teamId !== ownerShip.teamId)!;
    const ships = baseScene.ships.map(s => s.id === ownerShip.id ? { ...s, relation: 'self' as const, divisionId: '7' }
      : s.id === mate.id ? { ...s, divisionId: '7' }
      : s.id === enemyShip.id ? { ...s, divisionId: '7' } : s);
    const scene = { ...baseScene, ships };
    const color = (id: string, selected?: string) => {
      const m = model(0, scene, selected);
      return [...m.friendlyShips, ...m.enemyShips].find(s => s.id === id)?.color;
    };
    const other = baseScene.ships.find(s => s.teamId === ownerShip.teamId && s.id !== ownerShip.id && s.id !== mate.id)!;
    expect(color(mate.id, other.id)).toBe(DIVISION_COLOR);
    expect(color(ownerShip.id, other.id)).toBe(DIVISION_COLOR);
    expect(color(other.id, mate.id)).toBe(COLORS.friendly);
    expect(color(enemyShip.id, other.id)).toBe(COLORS.enemy); // same division id, other team
    expect(color(mate.id, mate.id)).toBe(SELECTED_SHIP_COLOR);
    expect(color(enemyShip.id, enemyShip.id)).toBe(SELECTED_SHIP_COLOR);
  });

  it('formats the clock and holds it at the duration during the ending', () => {
    expect(model(0).clock).toBe('00:00');
    expect(model(125.9).clock).toBe('02:05');
    expect(model(baseScene.replay.duration + 30).clock).toBe(model(baseScene.replay.duration).clock);
  });

  it('labels the map in upper case, official name first', () => {
    const label = (map: ReplayScene['map']) => model(0, { ...baseScene, map }).mapLabel;
    expect(label({ ...baseScene.map, name: 'spaces/22_tierra_del_fuego', displayName: 'Land of Fire' })).toBe('LAND OF FIRE');
    expect(label({ ...baseScene.map, name: 'spaces/22_tierra_del_fuego', displayName: undefined })).toBe('TIERRA DEL FUEGO');
    expect(label({ ...baseScene.map, name: 'spaces/15_NE_north', displayName: undefined })).toBe('NORTH');
    expect(label({ ...baseScene.map, name: 'Northern Waters', displayName: undefined })).toBe('NORTHERN WATERS');
  });

  it('maps cap owner and invader relative to the owner team', () => {
    expect(model(0).caps).toEqual([
      { id: 'cap-a', label: 'A', owner: null, invader: null },
      { id: 'cap-b', label: 'B', owner: null, invader: null },
      { id: 'cap-c', label: 'C', owner: null, invader: null },
    ]);
    expect(model(165).caps.map(c => c.owner)).toEqual(['friendly', 'enemy', 'enemy']);
    expect(model(345).caps.map(c => c.owner)).toEqual([null, 'friendly', 'enemy']);
    const zones = baseScene.captureZones.map(z => z.id === 'cap-a'
      ? { ...z, invader: [{ t: 0, value: null }, { t: 50, value: 'enemies' }, { t: 60, value: 'allies' }] } : z);
    const invaded = (time: number) => model(time, { ...baseScene, captureZones: zones }).caps[0].invader;
    expect([invaded(10), invaded(55), invaded(65)]).toEqual([null, 'enemy', 'friendly']);
  });
});
