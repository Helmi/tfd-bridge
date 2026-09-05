import { describe, expect, it } from 'vitest';
import type { ReplaySceneV1 } from '../types';
import { loadReplayScene } from './importScene';
import { evaluateScene } from './timeline';

const payload: ReplaySceneV1 = {
  schema: 'tfd-replay-scene', version: 1,
  replay: { id: 'real-1', name: 'Exporter fixture', gameBuild: '15.5', durationMs: 90_000, battleStartMs: 1_700_000_000_000, perspective: { teamId: 'blue', entityId: 'ship-1' } },
  map: { name: 'Fixture map', imageUrl: '/assets/map.png', spaceSize: { width: 1_000, height: 800 } },
  assets: { powerupIcons: { reload_inactive: './powerups/reload.png' } },
  teams: [
    { id: 'blue', name: 'Blue', color: '#55c7ff' },
    { id: 'red', name: 'Red', color: '#ff6b66' },
  ],
  entities: [{ id: 'ship-1', teamId: 'blue', relation: 'self', playerName: 'Tester', shipName: 'Daring', shipCode: 'PBSD110', species: 'destroyer', maxHp: 24_300 }],
  tracks: {
    ships: {
      'ship-1': [
        { t: 0, x: 100, y: 700, headingDeg: 350, hp: 24_300, maxHp: 24_300, alive: true, visible: true, lastKnown: false, detectedByEnemy: false, submerged: false },
        { t: 10_000, x: 120, y: 660, headingDeg: 10, hp: 20_000, maxHp: 24_300, alive: true, visible: true, lastKnown: false, detectedByEnemy: true, submerged: false },
      ],
    },
    scores: [{ t: 0, teams: { blue: 300, red: 300 } }, { t: 10_000, teams: { blue: 312, red: 300 } }],
    caps: [
      { t: 5_000, id: 'b', label: 'B', x: 500, y: 400, radius: 70, ownerTeamId: '-1', invaderTeamId: '-1', progress: 0, hasInvaders: false, enabled: false },
      { t: 10_000, id: 'b', label: 'B', x: 520, y: 420, radius: 45, ownerTeamId: '-1', invaderTeamId: 'red', progress: 0.42, hasInvaders: true, enabled: true },
      { t: 0, id: 'a', label: 'A', x: 200, y: 300, radius: 70, ownerId: null, progress: 0, enabled: true },
    ],
    buffs: [
      { t: 15_000, id: 'buff-7', x: 300, y: 350, radius: 50, active: true, activationAt: null, teamId: '-1', markerName: null },
      { t: 16_000, id: 'buff-7', x: 300, y: 350, radius: 50, active: true, activationAt: 20_000, teamId: '-1', markerName: 'reload_inactive' },
      { t: 25_000, id: 'buff-7', x: 300, y: 350, radius: 50, active: false, activationAt: 20_000, teamId: 'blue', markerName: 'reload_inactive' },
      { t: 15_000, id: 'buff-8', x: 700, y: 350, radius: 50, active: true, activationAt: null, teamId: '-1', markerName: null },
      { t: 17_000, id: 'buff-8', x: 700, y: 350, radius: 50, active: true, activationAt: 22_000, teamId: '-1', markerName: 'health_inactive' },
      { t: 25_000, id: 'buff-8', x: 700, y: 350, radius: 50, active: false, activationAt: 22_000, teamId: 'red', markerName: 'health_inactive' },
    ],
    smoke: [
      { t: 8_000, id: 'smoke:9', active: true, radius: 12, points: [{ x: 400, y: 500 }] },
      { t: 9_000, id: 'smoke:9', active: true, radius: 12, points: [{ x: 400, y: 500 }, { x: 410, y: 505 }] },
      { t: 40_000, id: 'smoke:9', active: false, radius: 12, points: [{ x: 400, y: 500 }, { x: 410, y: 505 }] },
    ],
    planes: [
      { t: 12_000, id: 'plane:77:1', x: 100, y: 100, active: true },
      { t: 22_000, id: 'plane:77:1', x: 200, y: 100, active: true },
      { t: 23_000, id: 'plane:77:1', x: 200, y: 100, active: false },
    ],
  },
  events: {
    salvos: [{ id: 'salvo', sourceId: 'ship-1', ammoType: 'HE', projectiles: [{ id: 'shell', startMs: 1_000, endMs: 3_000, path: [{ t: 1_000, x: 100, y: 700 }, { t: 3_000, x: 300, y: 500 }] }] }],
    torpedoes: [], kills: [],
    consumables: [{ t: 5_000, shipId: 'ship-1', name: 'HydroacousticSearch', durationMs: 10_000 }],
    chat: [{ t: 2_000, senderId: 'ship-1', senderName: 'Tester', channel: 'team', message: 'pushing B' }],
    pickups: [{ t: 25_000, ownerId: 'ship-1', teamId: 'blue', zoneId: 'buff-7', markerName: 'reload_active' }],
  },
  aviation: {
    'plane:77:1': { id: 'plane:77:1', ownerId: 'ship-1', teamId: 'blue', kind: 'fighter', category: 'controllable', iconDir: 'controllable', iconBase: 'fighter_he' },
  },
  wards: [{ id: 'ward:5', ownerId: 'ship-1', x: 300, y: 300, radius: 60, addedAt: 14_000, removedAt: 50_000 }],
};

describe('ReplayScene V1 adapter', () => {
  it('plays ballistic progress and removes a shell after its recorded impact', () => {
    const ballistic = structuredClone(payload);
    ballistic.events.salvos = [{ id: 'lethal', ownerId: 'ship-1', t: 1000, projectiles: [{
      id: '1', flightMs: 8000,
      path: [{ t: 1000, x: 100, y: 100 }, { t: 5000, x: 580, y: 220 }, { t: 9000, x: 900, y: 300 }],
    }] }];
    const scene = loadReplayScene(ballistic);
    expect(evaluateScene(scene, 5).ordnance[0].position).toMatchObject({ x: 580, y: 220 });
    expect(evaluateScene(scene, 9).ordnance[0].position).toMatchObject({ x: 900, y: 300 });
    expect(evaluateScene(scene, 9.001).ordnance).toHaveLength(0);
  });
  it('preserves optional division IDs and toolkit labels', () => {
    const withDivision = structuredClone(payload);
    withDivision.entities[0].divisionId = '9007199254740993';
    withDivision.entities[0].divisionLabel = 'B';
    expect(loadReplayScene(withDivision).ships[0]).toMatchObject({ divisionId: '9007199254740993', divisionLabel: 'B' });
    expect(loadReplayScene(payload).ships[0].divisionId).toBeUndefined();
  });
  it('normalizes exporter milliseconds and semantic tracks', () => {
    const scene = loadReplayScene(payload);
    expect(scene.replay.duration).toBe(90);
    expect(scene.map.bounds).toMatchObject({ maxX: 1_000, maxY: 800 });
    expect(scene.ships[0].pose[1]).toMatchObject({ t: 10, value: { yaw: 10 } });
    expect(scene.ships[0].detectedByEnemy?.[1]).toEqual({ t: 10, value: true });
    expect(scene.ordnance[0]).toMatchObject({ kind: 'shell', start: 1, end: 3 });
    expect(scene.captureZones.map((zone) => zone.label)).toEqual(['A', 'B']);
    expect(scene.captureZones[1].progress).toEqual([{ t: 0, value: 0 }, { t: 10, value: 42 }]);
    expect(scene.captureZones[1].invader).toEqual([{ t: 0, value: null }, { t: 10, value: 'red' }]);
    expect(scene.captureZones[1].enabled).toEqual([{ t: 0, value: false }, { t: 10, value: true }]);
    expect(evaluateScene(scene, 6).captureZones[1]).toMatchObject({ enabled: false, center: { x: 500, y: 400 }, radius: 70 });
    expect(evaluateScene(scene, 11).captureZones[1]).toMatchObject({ enabled: true, center: { x: 520, y: 420 }, radius: 45 });
    expect(scene.assets?.powerupIcons?.reload_inactive.href).toBe('./powerups/reload.png');
    expect(scene.buffZones[0]).toMatchObject({ spawnsAt: 15, activatesAt: 20 });
    expect(evaluateScene(scene, 14).buffZones[0]).toMatchObject({ active: false, collectible: false, activationProgress: 0, markerName: null });
    expect(evaluateScene(scene, 16).buffZones[0]).toMatchObject({
      active: true,
      collectible: false,
      activationProgress: 0.2,
      center: { x: 300, y: 350 },
      radius: 50,
      teamId: null,
      markerName: 'reload_inactive',
    });
    expect(evaluateScene(scene, 20).buffZones[0]).toMatchObject({ active: true, collectible: true, activationProgress: 1 });
    expect(evaluateScene(scene, 26).buffZones[0]).toMatchObject({ active: false, collectible: false, activationProgress: 1, teamId: 'blue' });
    expect(scene.buffZones.map((zone) => ({ id: zone.id, spawnsAt: zone.spawnsAt, activatesAt: zone.activatesAt }))).toEqual([
      { id: 'buff-7', spawnsAt: 15, activatesAt: 20 },
      { id: 'buff-8', spawnsAt: 15, activatesAt: 22 },
    ]);
    expect(evaluateScene(scene, 18).buffZones[1].activationProgress).toBeCloseTo(3 / 7);
    expect(evaluateScene(scene, 18).buffZones[1].collectible).toBe(false);
    expect(evaluateScene(scene, 23).buffZones[1].collectible).toBe(true);
  });

  it('imports and evaluates smoke, planes, wards, consumables, chat, and pickups', () => {
    const scene = loadReplayScene(payload);

    expect(scene.smokeScreens).toHaveLength(1);
    expect(scene.smokeScreens![0].puffs.map((sample) => sample.value.length)).toEqual([1, 2]);
    expect(evaluateScene(scene, 7).smoke[0]).toMatchObject({ active: false, puffs: [] });
    expect(evaluateScene(scene, 9.5).smoke[0].puffs).toHaveLength(2);
    expect(evaluateScene(scene, 41).smoke[0].active).toBe(false);

    expect(scene.planes![0]).toMatchObject({ ownerId: 'ship-1', teamId: 'blue', kind: 'fighter', iconDir: 'controllable', iconBase: 'fighter_he', relation: 'self' });
    expect(scene.ordnance.find((event) => event.kind === 'shell')?.ammoType).toBe('HE');
    const midFlight = evaluateScene(scene, 17).planes[0];
    expect(midFlight.active).toBe(true);
    expect(midFlight.position.x).toBeCloseTo(150);
    expect(midFlight.heading).toBeCloseTo(90);
    expect(evaluateScene(scene, 11).planes[0].active).toBe(false);
    expect(evaluateScene(scene, 24).planes[0].active).toBe(false);

    expect(scene.wards![0]).toMatchObject({ teamId: 'blue', center: { x: 300, y: 300 }, radius: 60 });
    expect(evaluateScene(scene, 13).wards[0].active).toBe(false);
    expect(evaluateScene(scene, 15).wards[0].active).toBe(true);
    expect(evaluateScene(scene, 51).wards[0].active).toBe(false);

    expect(evaluateScene(scene, 4).consumables).toHaveLength(0);
    const hydro = evaluateScene(scene, 6).consumables[0];
    expect(hydro.definition.name).toBe('HydroacousticSearch');
    expect(hydro.remaining).toBeCloseTo(9);
    expect(evaluateScene(scene, 15.5).consumables).toHaveLength(0);

    expect(scene.chat![0]).toMatchObject({ t: 2, senderName: 'Tester', channel: 'team', message: 'pushing B' });
    expect(scene.pickups![0]).toMatchObject({ t: 25, ownerId: 'ship-1', zoneId: 'buff-7' });
  });

  it('attributes HP losses to shells, fire, and leaves the rest unattributed', () => {
    const withDamage: ReplaySceneV1 = {
      ...payload,
      entities: [{ id: 'v1', teamId: 'blue', relation: 'self', playerName: 'Vic', shipName: 'Conqueror', shipCode: 'PBSB110', species: 'battleship', maxHp: 80_000 }],
      tracks: {
        ...payload.tracks,
        ships: {
          v1: [
            { t: 0, x: 0.5, y: 0.5, headingDeg: 0, hp: 80_000, maxHp: 80_000, alive: true, visible: true, lastKnown: false, detectedByEnemy: false, submerged: false },
            // gun salvo lands: -3000
            { t: 10_000, x: 0.5, y: 0.5, headingDeg: 0, hp: 77_000, maxHp: 80_000, alive: true, visible: true, lastKnown: false, detectedByEnemy: false, submerged: false },
            // fire ticks: three equal -500 drops
            { t: 12_000, x: 0.5, y: 0.5, headingDeg: 0, hp: 76_500, maxHp: 80_000, alive: true, visible: true, lastKnown: false, detectedByEnemy: false, submerged: false },
            { t: 13_000, x: 0.5, y: 0.5, headingDeg: 0, hp: 76_000, maxHp: 80_000, alive: true, visible: true, lastKnown: false, detectedByEnemy: false, submerged: false },
            { t: 14_000, x: 0.5, y: 0.5, headingDeg: 0, hp: 75_500, maxHp: 80_000, alive: true, visible: true, lastKnown: false, detectedByEnemy: false, submerged: false },
            // unseen torpedo: one-off -9000, no hit records
            { t: 30_000, x: 0.5, y: 0.5, headingDeg: 0, hp: 66_500, maxHp: 80_000, alive: true, visible: true, lastKnown: false, detectedByEnemy: false, submerged: false },
          ],
        },
      },
      events: {
        salvos: [], torpedoes: [], kills: [],
        hits: [
          { t: 9_900, attackerId: 'a1', victimId: 'v1', ammoType: 'AP', quality: 'citadel' },
          { t: 10_050, attackerId: 'a1', victimId: 'v1', ammoType: 'AP', quality: 'penetration' },
          { t: 10_100, attackerId: 'a1', victimId: 'v1', ammoType: 'AP', quality: 'shatter' },
        ],
      },
    };
    const scene = loadReplayScene(withDamage);
    const byTime = new Map(scene.damage.map((event) => [Math.round(event.t), event]));
    const gun = byTime.get(10);
    expect(gun).toMatchObject({ kind: 'shell', attackerId: 'a1', ammoType: 'AP', quality: 'citadel', amount: 3_000, hits: 2 });
    expect(byTime.get(13)?.kind).toBe('fire');
    expect(byTime.get(14)?.kind).toBe('fire');
    expect(byTime.get(30)).toMatchObject({ kind: 'other', amount: 9_000 });
  });

  it('fails clearly on an incompatible schema', () => {
    expect(() => loadReplayScene({ schema: 'other', version: 1 })).toThrow(/Unsupported replay scene/);
  });

  it('applies the scene cache key to a rewritten relative map asset', () => {
    const relativeMap = { ...payload, map: { ...payload.map, imageUrl: './map.png' } };
    const scene = loadReplayScene(relativeMap, { baseUrl: 'http://127.0.0.1:4173/generated/scene.json?v=replay-2' });
    expect(scene.map.image?.href).toBe('http://127.0.0.1:4173/generated/map.png?v=replay-2');
    expect(scene.assets?.powerupIcons?.reload_inactive.href).toBe('http://127.0.0.1:4173/generated/powerups/reload.png?v=replay-2');
  });
});
