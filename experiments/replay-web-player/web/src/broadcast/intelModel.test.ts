import { describe, expect, it } from 'vitest';
import { sampleScene } from '../data/sampleScene';
import { evaluateScene } from '../engine/timeline';
import type { EvaluatedConsumable, OwnerStats, ReplayScene, RibbonDefinition } from '../types';
import { consumableList, formatNumber, formatStat, ribbonList, selectedPanelModel } from './intelModel';

const owner = sampleScene.ships[0];
const ally = { ...sampleScene.ships.find(s => s.id !== owner.id && s.teamId === owner.teamId) ?? sampleScene.ships[1], clan: undefined };
const enemy = { ...sampleScene.ships.find(s => s.teamId !== owner.teamId)!, clan: undefined };

function panel(options: { selectedId: string; time?: number; scene?: Partial<ReplayScene>; ships?: ReplayScene['ships'] }) {
  const time = options.time ?? 30;
  const scene: ReplayScene = {
    ...sampleScene, ...options.scene,
    replay: { ...sampleScene.replay, perspectiveEntityId: owner.id },
    ships: options.ships ?? sampleScene.ships.map(s => s.id === ally.id ? ally : s.id === enemy.id ? enemy : s),
  };
  const state = evaluateScene(scene, Math.min(time, scene.replay.duration));
  return { scene, state, model: selectedPanelModel({ scene, time, state, selectedId: options.selectedId, ownerId: owner.id, ownerTeamId: owner.teamId }) };
}

describe('formatters', () => {
  it('formatStat shows a dash for a missing value and groups thousands', () => {
    expect(formatStat(undefined)).toBe('—');
    expect(formatStat(0)).toBe('0');
    expect(formatStat(12345.6)).toBe('12,346');
  });
  it('formatNumber rounds and groups thousands', () => {
    expect(formatNumber(1234.4)).toBe('1,234');
    expect(formatNumber(0)).toBe('0');
  });
});

describe('selectedPanelModel eyebrow', () => {
  it('uses the clan tag first', () => {
    expect(panel({ selectedId: owner.id }).model.eyebrow).toBe(`[${owner.clan}]`);
  });
  it('says REPLAY OWNER, ALLY or ENEMY without a clan', () => {
    const ships = sampleScene.ships.map(s => ({ ...s, clan: undefined }));
    expect(panel({ selectedId: owner.id, ships }).model.eyebrow).toBe('REPLAY OWNER');
    expect(panel({ selectedId: ally.id, ships }).model.eyebrow).toBe('ALLY');
    expect(panel({ selectedId: enemy.id, ships }).model.eyebrow).toBe('ENEMY');
  });
  it('falls back to the owner for an unknown selection', () => {
    const { model } = panel({ selectedId: 'missing' });
    expect(model.isOwner).toBe(true);
    expect(model.name).toBe(owner.playerName);
  });
});

describe('selectedPanelModel owner', () => {
  const ownerStats = [{ t: 0, value: { damage: 123456, potentialDamage: 555555, spottingDamage: undefined as unknown as number, ribbons: {} } }];
  it('lists damage (primary), potential and spotting', () => {
    const { model } = panel({ selectedId: owner.id, scene: { ownerStats } });
    expect(model.isOwner).toBe(true);
    expect(model.stats).toEqual([
      { label: 'DAMAGE', value: '123,456', primary: true },
      { label: 'POTENTIAL', value: '555,555' },
      { label: 'SPOTTING', value: '—' },
    ]);
    expect(model.name).toBe(owner.playerName);
    expect(model.shipName).toBe(owner.shipName);
  });
  it('shows dashes without owner stats', () => {
    const { model } = panel({ selectedId: owner.id, scene: { ownerStats: undefined } });
    expect(model.stats.map(s => s.value)).toEqual(['—', '—', '—']);
  });
});

describe('selectedPanelModel other ship', () => {
  it('pads the heading to three digits and appends the degree sign', () => {
    const { model, state } = panel({ selectedId: enemy.id });
    const yaw = state.ships.find(s => s.definition.id === enemy.id)!.pose.yaw;
    expect(model.stats[0]).toEqual({ label: 'HEADING', value: `${Math.round(yaw).toString().padStart(3, '0')}°` });
    expect(model.stats[0].value).toMatch(/^\d{3}°$/);
  });
  it('reports visibility from knowledge and Sunk at zero health', () => {
    const { model, state } = panel({ selectedId: enemy.id });
    const knowledge = state.ships.find(s => s.definition.id === enemy.id)!.knowledge;
    expect(model.stats[1]).toEqual({ label: 'VISIBILITY', value: { spotted: 'Spotted', 'last-known': 'Last known', hidden: 'Unspotted' }[knowledge] });
    const dead = panel({ selectedId: enemy.id, time: 30, ships: sampleScene.ships.map(s => s.id === enemy.id ? { ...s, health: [{ t: 0, value: 0 }] } : s) });
    expect(dead.model.stats[1].value).toBe('Sunk');
    expect(dead.model.healthRatio).toBe(0);
  });
  it('sums observed damage on the selected ship up to the time only', () => {
    const damage = [
      { id: 'a', t: 5, targetId: enemy.id, amount: 1200 },
      { id: 'b', t: 30, targetId: enemy.id, amount: 300 },
      { id: 'c', t: 31, targetId: enemy.id, amount: 5000 },
      { id: 'd', t: 6, targetId: owner.id, amount: 7000 },
    ] as ReplayScene['damage'];
    expect(panel({ selectedId: enemy.id, time: 30, scene: { damage } }).model.stats[2]).toEqual({ label: 'OBSERVED HP LOST', value: '1,500' });
    expect(panel({ selectedId: enemy.id, time: 4, scene: { damage } }).model.stats[2].value).toBe('0');
  });
  it('counts damage past the end of the battle only up to the duration', () => {
    const duration = sampleScene.replay.duration;
    const damage = [{ id: 'a', t: duration + 1, targetId: enemy.id, amount: 100 }] as ReplayScene['damage'];
    expect(panel({ selectedId: enemy.id, time: duration + 5, scene: { damage } }).model.stats[2].value).toBe('0');
  });
  it('exposes the health values and a clamped ratio', () => {
    const { model } = panel({ selectedId: enemy.id });
    expect(model.maxHealth).toBe(enemy.maxHealth);
    expect(model.healthRatio).toBeGreaterThanOrEqual(0);
    expect(model.healthRatio).toBeLessThanOrEqual(1);
    expect(model.healthRatio).toBeCloseTo(model.health / model.maxHealth);
  });
});

describe('ribbonList', () => {
  const stats = (ribbons: Record<string, number>): OwnerStats => ({ damage: 0, potentialDamage: 0, spottingDamage: 0, ribbons });
  const main: Record<string, RibbonDefinition> = {
    RIBBON_MAIN_CALIBER: { label: 'Main battery hits', iconKey: 'm', isSubribbon: false, imageUrl: 'data:image/png;base64,main' },
    RIBBON_CITADEL: { label: 'Citadel hits', iconKey: 'c', isSubribbon: false },
  };
  it('is empty without owner stats', () => {
    expect(ribbonList(undefined, main)).toEqual([]);
  });
  it('collapses the main-hit breakdown, keeps citadel separate and carries the image', () => {
    const list = ribbonList(stats({ RIBBON_BULGE: 2, RIBBON_CITADEL: 1, RIBBON_MAIN_CALIBER_NO_PENETRATION: 3, RIBBON_MAIN_CALIBER_OVER_PENETRATION: 15, RIBBON_MAIN_CALIBER_PENETRATION: 9, RIBBON_MAIN_CALIBER_RICOCHET: 4 }), main);
    expect(list).toEqual([
      { key: 'RIBBON_CITADEL', label: 'Citadel hits', count: 1, imageUrl: undefined },
      { key: 'RIBBON_MAIN_CALIBER', label: 'Main battery hits', count: 33, imageUrl: 'data:image/png;base64,main' },
    ]);
  });
  it('keeps a recorded aggregate instead of adding the breakdown', () => {
    const list = ribbonList(stats({ RIBBON_MAIN_CALIBER: 80, RIBBON_MAIN_CALIBER_PENETRATION: 9 }), main);
    expect(list.map(r => [r.key, r.count])).toEqual([['RIBBON_MAIN_CALIBER', 80]]);
  });
  it('does not invent an aggregate from aircraft ribbons', () => {
    expect(ribbonList(stats({ RIBBON_BOMB_PENETRATION: 3, RIBBON_CITADEL: 2 }), main).map(r => r.key)).toEqual(['RIBBON_BOMB_PENETRATION', 'RIBBON_CITADEL']);
  });
  it('skips zero counts and sub-ribbons, sorts by key and falls back to the key or the main label', () => {
    const defs: Record<string, RibbonDefinition> = {
      pens: { label: 'Penetrations', iconKey: 'p', isSubribbon: true },
      hits: { label: 'Hits', iconKey: 'h', isSubribbon: false },
    };
    expect(ribbonList(stats({ pens: 5, kills: 0, hits: 2, zeta: 1, alpha: 4 }), defs).map(r => [r.key, r.label, r.count])).toEqual([
      ['alpha', 'alpha', 4], ['hits', 'Hits', 2], ['zeta', 'zeta', 1],
    ]);
    expect(ribbonList(stats({ RIBBON_MAIN_CALIBER: 1 })).map(r => r.label)).toEqual(['Main battery hits']);
  });
});

describe('consumableList', () => {
  const consumable = (id: string, shipId: string, name: string, remaining: number): EvaluatedConsumable =>
    ({ definition: { id, shipId, name, start: 0, end: 10 }, remaining });
  it('keeps only the ship, splits camelCase and rounds seconds up', () => {
    const list = consumableList([
      consumable('1', 's1', 'RegenCrew', 9.01),
      consumable('2', 's2', 'Radar', 5),
      consumable('3', 's1', 'HydroacousticSearch', 0.2),
      consumable('4', 's1', 'Sonar2Ping', 3),
    ], 's1');
    expect(list).toEqual([
      { label: 'Regen Crew', seconds: 10 },
      { label: 'Hydroacoustic Search', seconds: 1 },
      { label: 'Sonar2 Ping', seconds: 3 },
    ]);
  });
  it('is empty when the ship has none', () => {
    expect(consumableList([consumable('1', 's2', 'Radar', 5)], 's1')).toEqual([]);
    expect(consumableList([], 's1')).toEqual([]);
  });
});
