import type {
  DamageEvent,
  OrdnanceEvent,
  Pose,
  ReplayScene,
  ShipDefinition,
  ShipKnowledge,
  TimedValue,
  WorldPoint,
} from '../types';

const DURATION = 420;

interface ShipSeed {
  id: string;
  teamId: 'allies' | 'enemies';
  playerName: string;
  clan?: string;
  shipName: string;
  shipClass: ShipDefinition['shipClass'];
  maxHealth: number;
  lane: number;
  index: number;
}

const alliedSeeds: Omit<ShipSeed, 'teamId' | 'index'>[] = [
  { id: 'a-01', playerName: 'Helmi', clan: 'TFD', shipName: 'Des Moines', shipClass: 'cruiser', maxHealth: 50_600, lane: 5 },
  { id: 'a-02', playerName: 'Northstar', clan: 'TFD', shipName: 'Yamato', shipClass: 'battleship', maxHealth: 97_200, lane: 3 },
  { id: 'a-03', playerName: 'SeaWitch', clan: 'TFD', shipName: 'Daring', shipClass: 'destroyer', maxHealth: 24_300, lane: 2 },
  { id: 'a-04', playerName: 'Kestrel', shipName: 'Montana', shipClass: 'battleship', maxHealth: 96_300, lane: 7 },
  { id: 'a-05', playerName: 'CopperFox', shipName: 'Halland', shipClass: 'destroyer', maxHealth: 22_700, lane: 8 },
  { id: 'a-06', playerName: 'Mariner', shipName: 'Minotaur', shipClass: 'cruiser', maxHealth: 43_300, lane: 6 },
  { id: 'a-07', playerName: 'Aegirsson', shipName: 'Vermont', shipClass: 'battleship', maxHealth: 102_800, lane: 1 },
  { id: 'a-08', playerName: 'Mako', shipName: 'Kléber', shipClass: 'destroyer', maxHealth: 21_900, lane: 9 },
  { id: 'a-09', playerName: 'Atlas', shipName: 'Hindenburg', shipClass: 'cruiser', maxHealth: 51_900, lane: 4 },
  { id: 'a-10', playerName: 'Valkyrie', shipName: 'Gouden Leeuw', shipClass: 'cruiser', maxHealth: 51_900, lane: 10 },
  { id: 'a-11', playerName: 'Driftwood', shipName: 'Shimakaze', shipClass: 'destroyer', maxHealth: 21_400, lane: 0 },
  { id: 'a-12', playerName: 'Waypoint', shipName: 'Midway', shipClass: 'carrier', maxHealth: 67_600, lane: 11 },
];

const enemySeeds: Omit<ShipSeed, 'teamId' | 'index'>[] = [
  { id: 'e-01', playerName: 'RedOctober', clan: 'KRAK', shipName: 'Moskva', shipClass: 'cruiser', maxHealth: 65_400, lane: 5 },
  { id: 'e-02', playerName: 'IronWake', clan: 'KRAK', shipName: 'Kremlin', shipClass: 'battleship', maxHealth: 108_300, lane: 7 },
  { id: 'e-03', playerName: 'LowProfile', shipName: 'Gearing', shipClass: 'destroyer', maxHealth: 23_900, lane: 8 },
  { id: 'e-04', playerName: 'Broadside', shipName: 'Conqueror', shipClass: 'battleship', maxHealth: 82_900, lane: 4 },
  { id: 'e-05', playerName: 'NightGlass', shipName: 'Z-52', shipClass: 'destroyer', maxHealth: 20_300, lane: 2 },
  { id: 'e-06', playerName: 'Foxtrot', shipName: 'Petropavlovsk', shipClass: 'cruiser', maxHealth: 55_800, lane: 6 },
  { id: 'e-07', playerName: 'OldSalt', shipName: 'St. Vincent', shipClass: 'battleship', maxHealth: 79_400, lane: 10 },
  { id: 'e-08', playerName: 'RazorFin', shipName: 'Marceau', shipClass: 'destroyer', maxHealth: 21_900, lane: 1 },
  { id: 'e-09', playerName: 'Redline', shipName: 'Napoli', shipClass: 'cruiser', maxHealth: 59_200, lane: 9 },
  { id: 'e-10', playerName: 'ColdFront', shipName: 'Henri IV', shipClass: 'cruiser', maxHealth: 53_300, lane: 3 },
  { id: 'e-11', playerName: 'DeepWater', shipName: 'Yueyang', shipClass: 'destroyer', maxHealth: 20_900, lane: 0 },
  { id: 'e-12', playerName: 'Skyhook', shipName: 'Hakuryū', shipClass: 'carrier', maxHealth: 63_100, lane: 11 },
];

const seeds: ShipSeed[] = [
  ...alliedSeeds.map((seed, index) => ({ ...seed, teamId: 'allies' as const, index })),
  ...enemySeeds.map((seed, index) => ({ ...seed, teamId: 'enemies' as const, index })),
];

const clamp = (value: number, min: number, max: number) => Math.min(max, Math.max(min, value));

function rawPosition(seed: ShipSeed, t: number): WorldPoint {
  const progress = clamp(t / DURATION, 0, 1);
  const aggression = seed.shipClass === 'carrier' ? 0.16 : seed.shipClass === 'destroyer' ? 0.9 : 0.68;
  const startY = seed.teamId === 'allies' ? 875 : 125;
  const direction = seed.teamId === 'allies' ? -1 : 1;
  const xBase = 105 + seed.lane * 72;
  const x = xBase + Math.sin(progress * Math.PI * (1.25 + (seed.index % 3) * 0.18) + seed.index * 0.67) * (34 + (seed.index % 4) * 9);
  const y = startY + direction * progress * 620 * aggression + Math.sin(progress * Math.PI * 2 + seed.index) * 18;
  return { x: clamp(x, 55, 945), y: clamp(y, 55, 945) };
}

function poseAt(seed: ShipSeed, t: number): Pose {
  const point = rawPosition(seed, t);
  const before = rawPosition(seed, Math.max(0, t - 1));
  const after = rawPosition(seed, Math.min(DURATION, t + 1));
  const dx = after.x - before.x;
  const dy = after.y - before.y;
  const course = ((Math.atan2(dx, -dy) * 180) / Math.PI + 360) % 360;
  const rudderSlip = Math.sin(t / 28 + seed.index * 0.9) * (seed.shipClass === 'battleship' ? 4 : 8);
  return {
    ...point,
    course,
    yaw: (course + rudderSlip + 360) % 360,
    speed: seed.shipClass === 'carrier' ? 18 : seed.shipClass === 'battleship' ? 24 : seed.shipClass === 'cruiser' ? 30 : 36,
  };
}

function poseTrack(seed: ShipSeed): TimedValue<Pose>[] {
  return Array.from({ length: DURATION / 15 + 1 }, (_, index) => {
    const t = index * 15;
    return { t, value: poseAt(seed, t) };
  });
}

function knowledgeTrack(seed: ShipSeed): TimedValue<ShipKnowledge>[] {
  if (seed.teamId === 'allies') return [{ t: 0, value: 'spotted' }];
  const offset = (seed.index % 4) * 11;
  return [
    { t: 0, value: 'hidden' },
    { t: 28 + offset, value: 'spotted' },
    { t: 118 + offset, value: 'last-known' },
    { t: 143 + offset, value: 'hidden' },
    { t: 166 + offset, value: 'spotted' },
    { t: 278 + offset, value: 'last-known' },
    { t: 306 + offset, value: 'spotted' },
  ];
}

function detectedTrack(seed: ShipSeed): TimedValue<boolean>[] {
  const offset = (seed.index % 5) * 8;
  return [
    { t: 0, value: false },
    { t: 44 + offset, value: true },
    { t: 126 + offset, value: false },
    { t: 173 + offset, value: true },
    { t: 267 + offset, value: false },
    { t: 322 + offset, value: true },
  ];
}

const damageEvents: DamageEvent[] = [];

function healthTrack(seed: ShipSeed): TimedValue<number>[] {
  const times = [82 + (seed.index % 4) * 9, 151 + (seed.index % 3) * 13, 224 + (seed.index % 5) * 8, 318 + (seed.index % 4) * 7];
  let health = seed.maxHealth;
  const result: TimedValue<number>[] = [{ t: 0, value: health }];
  times.forEach((t, hitIndex) => {
    let amount = Math.round(seed.maxHealth * (0.08 + ((seed.index + hitIndex) % 4) * 0.045));
    if ((seed.index === 2 || seed.index === 8) && hitIndex === 3) amount = health;
    health = Math.max(0, health - amount);
    result.push({ t, value: health });
    const sourceTeam = seed.teamId === 'allies' ? 'enemies' : 'allies';
    const attackers = seeds.filter((candidate) => candidate.teamId === sourceTeam);
    const isFire = hitIndex === 3 && seed.index % 5 === 0;
    const isUnseen = hitIndex === 2 && seed.index % 3 === 0;
    damageEvents.push({
      id: `damage-${seed.id}-${hitIndex}`,
      t,
      targetId: seed.id,
      amount,
      ...(isFire
        ? { kind: 'fire' as const }
        : isUnseen
          ? { kind: 'other' as const }
          : {
            kind: 'shell' as const,
            attackerId: attackers[(seed.index + hitIndex * 3) % attackers.length].id,
            ammoType: hitIndex % 2 === 0 ? 'HE' : 'AP',
            quality: hitIndex % 3 === 0 ? 'citadel' : 'penetration',
            hits: 1 + (hitIndex % 3),
          }),
    });
  });
  return result;
}

const ships: ShipDefinition[] = seeds.map((seed) => ({
  id: seed.id,
  teamId: seed.teamId,
  playerName: seed.playerName,
  clan: seed.clan,
  shipName: seed.shipName,
  shipClass: seed.shipClass,
  maxHealth: seed.maxHealth,
  pose: poseTrack(seed),
  health: healthTrack(seed),
  knowledge: knowledgeTrack(seed),
  detectedByEnemy: detectedTrack(seed),
  submerged: [{ t: 0, value: false }],
}));

const ordnance: OrdnanceEvent[] = [];

for (let salvo = 0; salvo < 55; salvo += 1) {
  const start = 64 + salvo * 6.15;
  const sourceTeam = salvo % 2 === 0 ? 'allies' : 'enemies';
  const source = seeds.filter((seed) => seed.teamId === sourceTeam)[salvo % 12];
  const target = seeds.filter((seed) => seed.teamId !== sourceTeam)[(salvo * 5 + 1) % 12];
  const flight = 2.1 + (salvo % 5) * 0.24;
  const from = rawPosition(source, start);
  const to = rawPosition(target, start + flight);
  const length = Math.hypot(to.x - from.x, to.y - from.y) || 1;
  const perpendicular = { x: -(to.y - from.y) / length, y: (to.x - from.x) / length };

  for (let shell = 0; shell < 4; shell += 1) {
    const spread = (shell - 1.5) * 4.2 + Math.sin(salvo * 2.3 + shell) * 2;
    const destination = { x: to.x + perpendicular.x * spread, y: to.y + perpendicular.y * spread };
    ordnance.push({
      id: `shell-${salvo}-${shell}`,
      kind: 'shell', sourceId: source.id, targetId: target.id, teamId: sourceTeam,
      start: start + shell * 0.06, end: start + flight + shell * 0.06,
      trajectory: [
        { t: start + shell * 0.06, ...from },
        { t: start + flight * 0.52 + shell * 0.06, x: (from.x + destination.x) / 2, y: (from.y + destination.y) / 2 },
        { t: start + flight + shell * 0.06, ...destination },
      ],
      result: shell === 1 && salvo % 3 !== 0 ? 'hit' : 'miss',
    });
  }
}

for (let salvo = 0; salvo < 11; salvo += 1) {
  const start = 92 + salvo * 27;
  const sourceTeam = salvo % 2 === 0 ? 'allies' : 'enemies';
  const destroyers = seeds.filter((seed) => seed.teamId === sourceTeam && seed.shipClass === 'destroyer');
  const targets = seeds.filter((seed) => seed.teamId !== sourceTeam && seed.shipClass !== 'carrier');
  const source = destroyers[salvo % destroyers.length];
  const target = targets[(salvo * 3) % targets.length];
  const from = rawPosition(source, start);
  const baseTo = rawPosition(target, start + 31);
  const length = Math.hypot(baseTo.x - from.x, baseTo.y - from.y) || 1;
  const perpendicular = { x: -(baseTo.y - from.y) / length, y: (baseTo.x - from.x) / length };
  for (let torpedo = 0; torpedo < 5; torpedo += 1) {
    const spread = (torpedo - 2) * 16;
    const destination = { x: baseTo.x + perpendicular.x * spread, y: baseTo.y + perpendicular.y * spread };
    ordnance.push({
      id: `torpedo-${salvo}-${torpedo}`,
      kind: 'torpedo', sourceId: source.id, targetId: target.id, teamId: sourceTeam,
      start: start + torpedo * 0.18, end: start + 31 + torpedo * 0.18,
      trajectory: [
        { t: start + torpedo * 0.18, ...from },
        { t: start + 31 + torpedo * 0.18, ...destination },
      ],
      result: torpedo === 2 && salvo % 4 === 0 ? 'hit' : 'miss',
    });
  }
}

export const sampleScene: ReplayScene = {
  schema: 'rocks.tfd.replay-scene/v0',
  replay: {
    id: 'synthetic-northern-waters-001',
    title: 'Northern Waters · Ranked scrim',
    gameVersion: 'synthetic 15.5',
    duration: DURATION,
    perspectiveTeamId: 'allies',
    perspectiveEntityId: 'a-01',
    battleStart: 0,
    source: 'synthetic',
  },
  map: {
    name: 'Northern Waters',
    bounds: { minX: 0, minY: 0, maxX: 1000, maxY: 1000 },
  },
  teams: [
    { id: 'allies', name: 'TFD Fleet', color: '#2fd6a6', score: [{ t: 0, value: 300 }, { t: 120, value: 362 }, { t: 240, value: 518 }, { t: 360, value: 714 }, { t: 420, value: 826 }] },
    { id: 'enemies', name: 'Opposing Fleet', color: '#f2665c', score: [{ t: 0, value: 300 }, { t: 120, value: 388 }, { t: 240, value: 472 }, { t: 360, value: 641 }, { t: 420, value: 702 }] },
  ],
  ships,
  captureZones: [
    {
      id: 'cap-a', label: 'A', center: { x: 230, y: 350 }, radius: 78,
      owner: [{ t: 0, value: null }, { t: 112, value: 'allies' }, { t: 296, value: null }],
      progress: [{ t: 0, value: 0 }, { t: 72, value: 35 }, { t: 112, value: 100 }, { t: 296, value: 0 }],
    },
    {
      id: 'cap-b', label: 'B', center: { x: 500, y: 510 }, radius: 72,
      owner: [{ t: 0, value: null }, { t: 164, value: 'enemies' }, { t: 338, value: 'allies' }],
      progress: [{ t: 0, value: 0 }, { t: 128, value: 45 }, { t: 164, value: 100 }, { t: 306, value: 38 }, { t: 338, value: 100 }],
    },
    {
      id: 'cap-c', label: 'C', center: { x: 785, y: 675 }, radius: 78,
      owner: [{ t: 0, value: null }, { t: 136, value: 'enemies' }],
      progress: [{ t: 0, value: 0 }, { t: 90, value: 28 }, { t: 136, value: 100 }],
    },
  ],
  buffZones: [],
  ordnance,
  damage: damageEvents.sort((a, b) => a.t - b.t),
};
