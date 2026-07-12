import type {
  BuffZone,
  CaptureZone,
  ChatMessage,
  ConsumableActivation,
  DamageEvent,
  FighterWard,
  OrdnanceEvent,
  PickupEvent,
  PlaneTrack,
  ReplayScene,
  ReplaySceneV1,
  ShipKnowledge,
  SmokeScreen,
  TimedValue,
  WorldPoint,
} from '../types';

type JsonObject = Record<string, unknown>;

export interface LoadSceneOptions {
  /** URL of scene.json; relative assets such as ./map.png resolve against it. */
  baseUrl?: string;
}

function isObject(value: unknown): value is JsonObject {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function assertV1(value: unknown): asserts value is ReplaySceneV1 {
  if (!isObject(value) || value.schema !== 'tfd-replay-scene' || value.version !== 1) {
    throw new Error('Unsupported replay scene: expected tfd-replay-scene version 1');
  }
  for (const field of ['replay', 'map', 'tracks', 'events']) {
    if (!isObject(value[field])) throw new Error(`Invalid replay scene: ${field} must be an object`);
  }
  if (!Array.isArray(value.teams) || !Array.isArray(value.entities)) {
    throw new Error('Invalid replay scene: teams and entities must be arrays');
  }
}

function compactTrack<T>(values: TimedValue<T>[], equal: (left: T, right: T) => boolean = Object.is): TimedValue<T>[] {
  const atDistinctTimes: TimedValue<T>[] = [];
  for (const sample of [...values].sort((a, b) => a.t - b.t)) {
    const previous = atDistinctTimes[atDistinctTimes.length - 1];
    if (previous?.t === sample.t) previous.value = sample.value;
    else atDistinctTimes.push({ ...sample });
  }
  return atDistinctTimes.filter((sample, index) => index === 0 || !equal(sample.value, atDistinctTimes[index - 1].value));
}

function samplesAtDistinctTimes<T extends { t: number }>(values: T[]): T[] {
  const result: T[] = [];
  for (const sample of [...values].sort((left, right) => left.t - right.t)) {
    const previous = result[result.length - 1];
    if (previous?.t === sample.t) result[result.length - 1] = sample;
    else result.push(sample);
  }
  return result;
}

function startTrackAt<T>(track: TimedValue<T>[], initial: T): TimedValue<T>[] {
  if (!(track[0]?.t > 0)) return track;
  if (Object.is(track[0].value, initial)) return [{ ...track[0], t: 0 }, ...track.slice(1)];
  return [{ t: 0, value: initial }, ...track];
}

function seconds(milliseconds: number): number {
  return milliseconds / 1000;
}

function normalizedMapSize(size: ReplaySceneV1['map']['spaceSize']): { width: number; height: number } {
  return typeof size === 'number' ? { width: size, height: size } : size;
}

function usesNormalizedCoordinates(payload: ReplaySceneV1): boolean {
  if (payload.map.coordinateSpace) return payload.map.coordinateSpace === 'normalized';
  const firstTrack = Object.values(payload.tracks.ships).find((track) => track.length)?.[0];
  return Boolean(firstTrack && firstTrack.x >= -0.1 && firstTrack.x <= 1.1 && firstTrack.y >= -0.1 && firstTrack.y <= 1.1);
}

function resolveAsset(href: string | undefined, baseUrl: string | undefined): string | undefined {
  if (!href || !baseUrl) return href;
  try {
    const base = new URL(baseUrl);
    const resolved = new URL(href, base);
    const isRelative = !/^[a-z][a-z\d+.-]*:/i.test(href) && !href.startsWith('/') && !href.startsWith('//');
    // The experiment rewrites map.png for each selected replay. Carry the
    // scene's cache key onto relative assets so Pixi cannot reuse the prior
    // replay's texture under the same URL.
    if (isRelative && base.search && !resolved.search) resolved.search = base.search;
    return resolved.href;
  } catch {
    return href;
  }
}

function perspective(payload: ReplaySceneV1): { teamId: string; entityId?: string } {
  if (typeof payload.replay.perspective !== 'string') return payload.replay.perspective;
  const entity = payload.entities.find((candidate) => candidate.relation === 'self')
    ?? payload.entities.find((candidate) => candidate.playerName === payload.replay.perspective);
  return { teamId: entity?.teamId ?? payload.teams[0]?.id ?? 'unknown', entityId: entity?.id };
}

function normalizeOwner(owner: string | null | undefined): string | null {
  return !owner || owner === '-1' ? null : owner;
}

function normalizeTeamId(teamId: string | number | null | undefined): string | null {
  return teamId === undefined || teamId === null || String(teamId) === '-1' ? null : String(teamId);
}

function normalizeCaptureProgress(progress: number): number {
  const percentage = Math.abs(progress) <= 1.000_01 ? progress * 100 : progress;
  return Math.max(0, Math.min(100, percentage));
}

function captureZones(payload: ReplaySceneV1): CaptureZone[] {
  const groups = new Map<string, ReplaySceneV1['tracks']['caps']>();
  for (const sample of payload.tracks.caps) {
    const group = groups.get(sample.id) ?? [];
    group.push(sample);
    groups.set(sample.id, group);
  }
  return [...groups.values()].map((samples) => {
    samples.sort((a, b) => a.t - b.t);
    const geometry = samples.filter((sample) => sample.x !== undefined && sample.y !== undefined);
    const located = geometry[0] ?? samples[0];
    const centers = compactTrack(
      geometry.map((sample) => ({ t: seconds(sample.t), value: { x: sample.x!, y: sample.y! } })),
      (left, right) => left.x === right.x && left.y === right.y,
    );
    const radii = compactTrack(samples.map((sample) => ({ t: seconds(sample.t), value: sample.radius })));
    return {
      id: located.id,
      label: located.label ?? located.id,
      center: { x: located.x ?? 0.5, y: located.y ?? 0.5 },
      radius: located.radius,
      centerTrack: centers.length ? centers : undefined,
      radiusTrack: radii.length ? radii : undefined,
      enabled: startTrackAt(compactTrack(samples.map((sample) => ({ t: seconds(sample.t), value: sample.enabled ?? true }))), false),
      owner: startTrackAt(compactTrack(samples.map((sample) => ({ t: seconds(sample.t), value: normalizeOwner(sample.ownerTeamId ?? sample.ownerId) }))), null),
      invader: startTrackAt(compactTrack(samples.map((sample) => ({ t: seconds(sample.t), value: normalizeOwner(sample.invaderTeamId) }))), null),
      progress: startTrackAt(compactTrack(samples.map((sample) => ({ t: seconds(sample.t), value: normalizeCaptureProgress(sample.progress) }))), 0),
      contested: startTrackAt(compactTrack(samples.map((sample) => ({ t: seconds(sample.t), value: sample.contested ?? false }))), false),
      hasInvaders: startTrackAt(compactTrack(samples.map((sample) => ({ t: seconds(sample.t), value: sample.hasInvaders ?? false }))), false),
    };
  }).sort((left, right) => left.label.localeCompare(right.label));
}

function buffZones(payload: ReplaySceneV1): BuffZone[] {
  const groups = new Map<string, NonNullable<ReplaySceneV1['tracks']['buffs']>>();
  for (const sample of payload.tracks.buffs ?? []) {
    const group = groups.get(sample.id) ?? [];
    group.push(sample);
    groups.set(sample.id, group);
  }

  return [...groups.values()].map((samples) => {
    samples.sort((left, right) => left.t - right.t);
    const geometry = samples.filter((sample) => sample.x !== undefined && sample.y !== undefined);
    const located = geometry[0] ?? samples[0];
    const spawned = samples.find((sample) => sample.active ?? true) ?? located;
    // Drop metadata may arrive after the zone entity. Older exports preserve
    // that first unknown value as null; it must not become an activation at t=0.
    const activation = samples.find((sample) => sample.activationAt != null)?.activationAt;
    const centers = compactTrack(
      geometry.map((sample) => ({ t: seconds(sample.t), value: { x: sample.x!, y: sample.y! } })),
      (left, right) => left.x === right.x && left.y === right.y,
    );
    const radii = compactTrack(samples.map((sample) => ({ t: seconds(sample.t), value: sample.radius })));
    const teams = startTrackAt(compactTrack(samples.map((sample) => ({
      t: seconds(sample.t),
      value: normalizeTeamId(sample.teamId),
    }))), null);
    const markers = startTrackAt(compactTrack(samples
      .filter((sample) => sample.markerName !== undefined)
      .map((sample) => ({ t: seconds(sample.t), value: sample.markerName ?? null }))), null);

    return {
      id: located.id,
      center: { x: located.x ?? 0.5, y: located.y ?? 0.5 },
      radius: located.radius,
      spawnsAt: seconds(spawned.t),
      activatesAt: activation == null ? undefined : seconds(activation),
      centerTrack: centers.length ? centers : undefined,
      radiusTrack: radii.length ? radii : undefined,
      active: startTrackAt(compactTrack(samples.map((sample) => ({
        t: seconds(sample.t),
        value: sample.active ?? true,
      }))), false),
      teamId: teams.length ? teams : undefined,
      markerName: markers.length ? markers : undefined,
    };
  }).sort((left, right) => left.id.localeCompare(right.id));
}

function smokeScreens(payload: ReplaySceneV1): SmokeScreen[] {
  const groups = new Map<string, NonNullable<ReplaySceneV1['tracks']['smoke']>>();
  for (const sample of payload.tracks.smoke ?? []) {
    const group = groups.get(sample.id) ?? [];
    group.push(sample);
    groups.set(sample.id, group);
  }
  return [...groups.values()].map((samples) => {
    samples.sort((left, right) => left.t - right.t);
    return {
      id: samples[0].id,
      radius: samples[0].radius,
      puffs: compactTrack(
        samples.map((sample) => ({ t: seconds(sample.t), value: sample.points })),
        (left, right) => left.length === right.length
          && left.every((point, index) => point.x === right[index].x && point.y === right[index].y),
      ),
      active: startTrackAt(compactTrack(samples.map((sample) => ({ t: seconds(sample.t), value: sample.active }))), false),
    };
  }).sort((left, right) => left.id.localeCompare(right.id));
}

function planeTracks(payload: ReplaySceneV1): PlaneTrack[] {
  const relationOf = new Map(payload.entities.map((entity) => [entity.id, entity.relation]));
  const perspectiveTeam = perspective(payload).teamId;
  const groups = new Map<string, NonNullable<ReplaySceneV1['tracks']['planes']>>();
  for (const sample of payload.tracks.planes ?? []) {
    const group = groups.get(sample.id) ?? [];
    group.push(sample);
    groups.set(sample.id, group);
  }
  return [...groups.values()].map((samples) => {
    samples.sort((left, right) => left.t - right.t);
    const descriptor = payload.aviation?.[samples[0].id];
    const ownerId = descriptor?.ownerId ?? 'unknown';
    const teamId = descriptor?.teamId ?? 'unknown';
    const relation = relationOf.get(ownerId) ?? (teamId === perspectiveTeam ? 'ally' : 'enemy');
    return {
      id: samples[0].id,
      ownerId,
      teamId,
      kind: descriptor?.kind,
      category: descriptor?.category,
      iconDir: descriptor?.iconDir,
      iconBase: descriptor?.iconBase,
      relation,
      pose: compactTrack(
        samples.map((sample) => ({ t: seconds(sample.t), value: { x: sample.x, y: sample.y } })),
        (left, right) => left.x === right.x && left.y === right.y,
      ),
      active: startTrackAt(compactTrack(samples.map((sample) => ({ t: seconds(sample.t), value: sample.active }))), false),
    };
  }).sort((left, right) => left.id.localeCompare(right.id));
}

function fighterWards(payload: ReplaySceneV1): FighterWard[] {
  const entityTeam = Object.fromEntries(payload.entities.map((entity) => [entity.id, entity.teamId]));
  return (payload.wards ?? []).map((ward) => ({
    id: ward.id,
    ownerId: ward.ownerId,
    teamId: entityTeam[ward.ownerId] ?? null,
    center: { x: ward.x, y: ward.y },
    radius: ward.radius,
    addedAt: seconds(ward.addedAt),
    removedAt: ward.removedAt === undefined ? undefined : seconds(ward.removedAt),
  }));
}

function consumableActivations(payload: ReplaySceneV1): ConsumableActivation[] {
  return (payload.events.consumables ?? []).map((event, index) => ({
    id: `consumable:${event.shipId}:${event.name}:${event.t}:${index}`,
    shipId: event.shipId,
    name: event.name,
    start: seconds(event.t),
    end: seconds(event.t + Math.max(0, event.durationMs)),
  })).sort((left, right) => left.start - right.start);
}

function chatMessages(payload: ReplaySceneV1): ChatMessage[] {
  return (payload.events.chat ?? []).map((event, index) => ({
    id: `chat:${index}`,
    t: seconds(event.t),
    senderId: event.senderId || undefined,
    senderName: event.senderName,
    channel: event.channel,
    message: event.message,
  })).sort((left, right) => left.t - right.t);
}

function pickupEvents(payload: ReplaySceneV1): PickupEvent[] {
  return (payload.events.pickups ?? []).map((event) => ({
    t: seconds(event.t),
    ownerId: event.ownerId,
    teamId: event.teamId,
    zoneId: event.zoneId,
    markerName: event.markerName,
  })).sort((left, right) => left.t - right.t);
}

function extrapolatedTorpedoEnd(
  last: NonNullable<ReplaySceneV1['events']['torpedoes'][number]['samples']>[number],
  endMs: number,
  worldSize: number,
): WorldPoint & { t: number } {
  const elapsed = Math.max(0, endMs - last.t) / 1000;
  const distance = ((last.speed ?? 0) * elapsed) / Math.max(1, worldSize);
  const heading = ((last.headingDeg ?? 0) * Math.PI) / 180;
  return {
    t: endMs,
    x: last.x + Math.sin(heading) * distance,
    y: last.y - Math.cos(heading) * distance,
  };
}

function ordnanceEvents(payload: ReplaySceneV1): OrdnanceEvent[] {
  const entityTeam = Object.fromEntries(payload.entities.map((entity) => [entity.id, entity.teamId]));
  const worldSize = normalizedMapSize(payload.map.spaceSize).width;
  const shells = payload.events.salvos.flatMap((salvo) => {
    const sourceId = salvo.ownerId ?? salvo.sourceId ?? 'unknown';
    return salvo.projectiles.flatMap((projectile) => {
      const startMs = projectile.startMs ?? salvo.t ?? 0;
      const endMs = projectile.endMs ?? startMs + (projectile.flightMs ?? 0);
      const rawPath = projectile.path ?? (
        projectile.origin && projectile.target
          ? [{ t: startMs, ...projectile.origin }, { t: endMs, ...projectile.target }]
          : []
      );
      if (rawPath.length < 2 || endMs <= startMs) return [];
      return [{
        id: `${salvo.id}:${projectile.id}`,
        kind: 'shell' as const,
        sourceId,
        targetId: salvo.targetId,
        teamId: entityTeam[sourceId] ?? 'unknown',
        ammoType: salvo.ammoType,
        start: seconds(startMs),
        end: seconds(endMs),
        trajectory: rawPath.map((point) => ({ ...point, t: seconds(point.t) })),
        result: projectile.result,
      }];
    });
  });

  const torpedoes = payload.events.torpedoes.flatMap((torpedo) => {
    const sourceId = torpedo.ownerId ?? torpedo.sourceId ?? 'unknown';
    const rawSamples = [...(torpedo.samples ?? torpedo.path ?? [])].sort((a, b) => a.t - b.t);
    const startMs = torpedo.launchedAt ?? torpedo.startMs ?? rawSamples[0]?.t;
    const endMs = torpedo.endedAt ?? torpedo.endMs ?? rawSamples[rawSamples.length - 1]?.t;
    if (startMs === undefined || endMs === undefined || !rawSamples.length || endMs <= startMs) return [];
    const last = rawSamples[rawSamples.length - 1];
    const sampledLast = torpedo.samples?.[torpedo.samples.length - 1];
    const path = last.t < endMs && sampledLast
      ? [...rawSamples, extrapolatedTorpedoEnd(sampledLast, endMs, worldSize)]
      : rawSamples;
    const firstArmed = torpedo.samples?.find((sample) => sample.armed);
    return [{
      id: torpedo.id,
      kind: 'torpedo' as const,
      sourceId,
      targetId: torpedo.targetId,
      teamId: entityTeam[sourceId] ?? 'unknown',
      start: seconds(startMs),
      end: seconds(endMs),
      trajectory: path.map((point) => ({ x: point.x, y: point.y, t: seconds(point.t) })),
      armedAt: firstArmed ? seconds(firstArmed.t) : undefined,
      result: torpedo.result,
    }];
  });
  return [...shells, ...torpedoes];
}

const PENETRATING_QUALITY = new Set(['penetration', 'citadel', 'overpen']);

// A drop that repeats at ~the same magnitude close in time is a damage-over-
// time tick (fire/flooding), not a one-off hit.
function isRecurringTick(drops: Array<{ t: number; amount: number }>, index: number): boolean {
  const drop = drops[index];
  const tolerance = Math.max(20, drop.amount * 0.08);
  return drops.some((other, otherIndex) =>
    otherIndex !== index
    && Math.abs(other.t - drop.t) <= 4000
    && Math.abs(other.amount - drop.amount) <= tolerance);
}

/**
 * Attribute each HP loss to a cause. Gun hits are matched to the replay's
 * shot-hit records (attacker + shell type + penetration quality) within a
 * tight time window; recurring equal ticks are labelled fire; everything else
 * (unseen torpedoes, flooding) stays unattributed, honest to the single
 * perspective.
 */
function attributedDamage(payload: ReplaySceneV1): DamageEvent[] {
  const hitsByVictim = new Map<string, ReplaySceneV1['events']['hits']>();
  for (const hit of payload.events.hits ?? []) {
    const list = hitsByVictim.get(hit.victimId) ?? [];
    list.push(hit);
    hitsByVictim.set(hit.victimId, list);
  }

  const damage: DamageEvent[] = [];
  for (const entity of payload.entities) {
    const samples = samplesAtDistinctTimes(payload.tracks.ships[entity.id] ?? []);
    const drops: Array<{ t: number; amount: number }> = [];
    let previousHp = samples[0]?.hp ?? entity.maxHp;
    for (const sample of samples.slice(1)) {
      if (!sample.hpStale && sample.hp < previousHp - 1) {
        drops.push({ t: sample.t, amount: previousHp - sample.hp });
      }
      if (!sample.hpStale) previousHp = sample.hp;
    }

    const victimHits = hitsByVictim.get(entity.id) ?? [];
    drops.forEach((drop, index) => {
      const base = { id: `dmg:${entity.id}:${drop.t}`, t: seconds(drop.t), targetId: entity.id, amount: drop.amount };
      const penetrating = (victimHits ?? []).filter((hit) =>
        Math.abs(hit.t - drop.t) <= 500 && PENETRATING_QUALITY.has(hit.quality));
      if (penetrating.length) {
        const byAttacker = new Map<string, { ammoType?: string; count: number; qualities: Set<string> }>();
        for (const hit of penetrating) {
          const entry = byAttacker.get(hit.attackerId) ?? { ammoType: undefined, count: 0, qualities: new Set() };
          entry.count += 1;
          entry.qualities.add(hit.quality);
          if (hit.ammoType) entry.ammoType = hit.ammoType;
          byAttacker.set(hit.attackerId, entry);
        }
        const [attackerId, info] = [...byAttacker.entries()].sort((left, right) => right[1].count - left[1].count)[0];
        const quality = info.qualities.has('citadel') ? 'citadel'
          : info.qualities.has('penetration') ? 'penetration' : 'overpen';
        damage.push({ ...base, kind: 'shell', attackerId, ammoType: info.ammoType, quality, hits: penetrating.length });
      } else if (isRecurringTick(drops, index)) {
        damage.push({ ...base, kind: 'fire' });
      } else {
        damage.push({ ...base, kind: 'other' });
      }
    });
  }
  return damage.sort((left, right) => left.t - right.t);
}

function motionCourse(
  track: ReplaySceneV1['tracks']['ships'][string],
  index: number,
): number {
  const sample = track[index];
  if (sample.courseDeg !== undefined) return sample.courseDeg;
  const from = track[Math.max(0, index - 1)];
  const to = track[Math.min(track.length - 1, index + 1)];
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  const elapsed = (to.t - from.t) / 1000;
  if (elapsed <= 0 || Math.hypot(dx, dy) < 1e-8) return sample.headingDeg;
  return ((Math.atan2(dx, -dy) * 180) / Math.PI + 360) % 360;
}

export function loadReplayScene(input: unknown, options: LoadSceneOptions = {}): ReplayScene {
  if (isObject(input) && input.schema === 'rocks.tfd.replay-scene/v0') return input as unknown as ReplayScene;
  assertV1(input);
  const size = normalizedMapSize(input.map.spaceSize);
  const normalized = usesNormalizedCoordinates(input);
  const view = perspective(input);
  const powerupIcons = Object.fromEntries(Object.entries(input.assets?.powerupIcons ?? {}).map(([markerName, href]) => [
    markerName,
    { href: resolveAsset(href, options.baseUrl) ?? href },
  ]));
  const planeIcons = Object.fromEntries(Object.entries(input.assets?.planeIcons ?? {}).map(([key, href]) => [
    key,
    { href: resolveAsset(href, options.baseUrl) ?? href },
  ]));

  return {
    schema: 'rocks.tfd.replay-scene/v0',
    replay: {
      id: input.replay.id,
      title: input.replay.name,
      gameVersion: input.replay.gameBuild,
      duration: seconds(input.replay.durationMs),
      perspectiveTeamId: view.teamId,
      perspectiveEntityId: view.entityId,
      battleStart: seconds(input.replay.battleStartMs),
      source: 'decoded-replay',
    },
    map: {
      name: input.map.name,
      bounds: { minX: 0, minY: 0, maxX: normalized ? 1 : size.width, maxY: normalized ? 1 : size.height },
      image: input.map.imageUrl ? { href: resolveAsset(input.map.imageUrl, options.baseUrl)! } : undefined,
      spaceSize: input.map.spaceSize,
    },
    teams: input.teams.map((team) => ({
      ...team,
      score: startTrackAt(compactTrack(input.tracks.scores.map((sample) => ({ t: seconds(sample.t), value: sample.teams[team.id] ?? 0 }))), 0),
    })),
    ships: input.entities.map((entity) => {
      const track = samplesAtDistinctTimes(input.tracks.ships[entity.id] ?? []);
      if (!track?.length) throw new Error(`Invalid replay scene: entity ${entity.id} has no ship track`);
      return {
        id: entity.id,
        teamId: entity.teamId,
        relation: entity.relation,
        playerName: entity.playerName,
        clan: entity.clan,
        shipName: entity.shipName,
        shipClass: entity.species === 'aircarrier' ? 'carrier' : entity.species,
        maxHealth: entity.maxHp,
        pose: track.map((sample, index) => {
          return {
            t: seconds(sample.t),
            value: {
              x: sample.x,
              y: sample.y,
              yaw: sample.headingDeg,
              course: motionCourse(track, index),
              speed: sample.speedKnots,
            },
          };
        }),
        health: compactTrack(track.map((sample) => ({ t: seconds(sample.t), value: sample.hp }))),
        knowledge: compactTrack(track.map((sample): TimedValue<ShipKnowledge> => ({
          t: seconds(sample.t),
          value: sample.visible ? 'spotted' : sample.lastKnown ? 'last-known' : 'hidden',
        }))),
        detectedByEnemy: compactTrack(track.map((sample) => ({ t: seconds(sample.t), value: sample.detectedByEnemy }))),
        submerged: compactTrack(track.map((sample) => ({ t: seconds(sample.t), value: sample.submerged }))),
      };
    }),
    captureZones: captureZones(input),
    buffZones: buffZones(input),
    smokeScreens: smokeScreens(input),
    planes: planeTracks(input),
    wards: fighterWards(input),
    consumables: consumableActivations(input),
    chat: chatMessages(input),
    pickups: pickupEvents(input),
    assets: (Object.keys(powerupIcons).length || Object.keys(planeIcons).length)
      ? {
        ...(Object.keys(powerupIcons).length ? { powerupIcons } : {}),
        ...(Object.keys(planeIcons).length ? { planeIcons } : {}),
      }
      : undefined,
    ordnance: ordnanceEvents(input),
    damage: attributedDamage(input),
  };
}
