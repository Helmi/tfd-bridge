import type {
  EvaluatedOrdnance,
  EvaluatedPlane,
  Pose,
  ReplayScene,
  SceneState,
  ShipDefinition,
  TimedValue,
  TrajectoryPoint,
  WorldPoint,
} from '../types';

export function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function surrounding<T>(track: TimedValue<T>[], t: number): [TimedValue<T>, TimedValue<T>] {
  if (!track.length) throw new Error('Cannot evaluate an empty timeline');
  if (t <= track[0].t) return [track[0], track[0]];
  if (t >= track[track.length - 1].t) return [track[track.length - 1], track[track.length - 1]];

  let low = 0;
  let high = track.length - 1;
  while (low + 1 < high) {
    const mid = (low + high) >> 1;
    if (track[mid].t <= t) low = mid;
    else high = mid;
  }
  return [track[low], track[high]];
}

export function sampleStep<T>(track: TimedValue<T>[], t: number): T {
  return surrounding(track, t)[0].value;
}

export function lerpAngle(from: number, to: number, amount: number): number {
  const delta = ((to - from + 540) % 360) - 180;
  return (from + delta * amount + 360) % 360;
}

export function samplePose(track: TimedValue<Pose>[], t: number): Pose {
  const [from, to] = surrounding(track, t);
  if (from === to || from.t === to.t) return { ...from.value };
  const amount = clamp((t - from.t) / (to.t - from.t), 0, 1);
  return {
    x: from.value.x + (to.value.x - from.value.x) * amount,
    y: from.value.y + (to.value.y - from.value.y) * amount,
    yaw: lerpAngle(from.value.yaw, to.value.yaw, amount),
    course: lerpAngle(from.value.course, to.value.course, amount),
    speed: from.value.speed !== undefined && to.value.speed !== undefined
      ? from.value.speed + (to.value.speed - from.value.speed) * amount
      : from.value.speed ?? to.value.speed,
  };
}

function lastKnownSince(ship: ShipDefinition, t: number): number | undefined {
  let result: number | undefined;
  for (const sample of ship.knowledge) {
    if (sample.t > t) break;
    if (sample.value === 'last-known') result = sample.t;
  }
  return result;
}

function sampleTrajectory(track: TrajectoryPoint[], t: number): WorldPoint {
  const timed = track.map((point) => ({ t: point.t, value: point }));
  const [from, to] = surrounding(timed, t);
  if (from === to || from.t === to.t) return { x: from.value.x, y: from.value.y };
  const amount = clamp((t - from.t) / (to.t - from.t), 0, 1);
  return {
    x: from.value.x + (to.value.x - from.value.x) * amount,
    y: from.value.y + (to.value.y - from.value.y) * amount,
  };
}

function evaluateOrdnance(scene: ReplayScene, t: number): EvaluatedOrdnance[] {
  return scene.ordnance
    .filter((event) => event.start <= t && event.end >= t)
    .map((event) => {
      const position = sampleTrajectory(event.trajectory, t);
      const previousPosition = sampleTrajectory(event.trajectory, Math.max(event.start, t - 0.22));
      const heading = (Math.atan2(position.x - previousPosition.x, -(position.y - previousPosition.y)) * 180) / Math.PI;
      return {
        event,
        position,
        previousPosition,
        heading,
        progress: clamp((t - event.start) / (event.end - event.start), 0, 1),
        armed: event.armedAt === undefined || t >= event.armedAt,
      };
    });
}

function samplePoint(track: TimedValue<WorldPoint>[], t: number): WorldPoint {
  const [from, to] = surrounding(track, t);
  if (from === to || from.t === to.t) return { ...from.value };
  const amount = clamp((t - from.t) / (to.t - from.t), 0, 1);
  return {
    x: from.value.x + (to.value.x - from.value.x) * amount,
    y: from.value.y + (to.value.y - from.value.y) * amount,
  };
}

function evaluatePlanes(scene: ReplayScene, t: number): EvaluatedPlane[] {
  return (scene.planes ?? []).map((definition) => {
    const position = samplePoint(definition.pose, t);
    const previous = samplePoint(definition.pose, t - 1.5);
    const dx = position.x - previous.x;
    const dy = position.y - previous.y;
    const heading = Math.hypot(dx, dy) < 1e-7 ? 0 : ((Math.atan2(dx, -dy) * 180) / Math.PI + 360) % 360;
    // Squadron tracks begin at first observation; mirror the enemy-ship rule
    // so a squadron cannot appear before the replay knows about it.
    const beforeFirst = t < definition.pose[0].t;
    return {
      definition,
      position,
      heading,
      active: !beforeFirst && t <= definition.pose[definition.pose.length - 1].t + 2 && sampleStep(definition.active, t),
    };
  });
}

export function evaluateScene(scene: ReplayScene, requestedTime: number): SceneState {
  const t = clamp(requestedTime, 0, scene.replay.duration);
  return {
    t,
    ships: scene.ships.map((definition) => {
      const pose = samplePose(definition.pose, t);
      // Enemy tracks begin when that ship is first observed. Sampling the first
      // value before its timestamp leaks a future position into the opening.
      const beforeFirstEnemyObservation = definition.relation === 'enemy' && t < definition.knowledge[0].t;
      const knowledge = beforeFirstEnemyObservation ? 'hidden' : sampleStep(definition.knowledge, t);
      const lastSeen = knowledge === 'last-known' ? lastKnownSince(definition, t) : undefined;
      const displayPose = lastSeen === undefined ? pose : samplePose(definition.pose, lastSeen);
      const health = sampleStep(definition.health, t);
      return {
        definition,
        pose,
        displayPose,
        health,
        knowledge,
        detectedByEnemy: definition.detectedByEnemy ? sampleStep(definition.detectedByEnemy, t) : false,
        submerged: definition.submerged ? sampleStep(definition.submerged, t) : false,
        destroyed: health <= 0,
      };
    }),
    ordnance: evaluateOrdnance(scene, t),
    captureZones: scene.captureZones.map((definition) => ({
      definition,
      center: definition.centerTrack ? sampleStep(definition.centerTrack, t) : definition.center,
      radius: definition.radiusTrack ? sampleStep(definition.radiusTrack, t) : definition.radius,
      enabled: definition.enabled ? sampleStep(definition.enabled, t) : true,
      owner: sampleStep(definition.owner, t),
      invader: definition.invader ? sampleStep(definition.invader, t) : null,
      progress: sampleStep(definition.progress, t),
      contested: definition.contested ? sampleStep(definition.contested, t) : false,
      hasInvaders: definition.hasInvaders ? sampleStep(definition.hasInvaders, t) : false,
    })),
    buffZones: (scene.buffZones ?? []).map((definition) => {
      const active = sampleStep(definition.active, t);
      const activationDuration = Math.max(0.001, (definition.activatesAt ?? definition.spawnsAt) - definition.spawnsAt);
      const activationProgress = definition.activatesAt === undefined
        ? 1
        : clamp((t - definition.spawnsAt) / activationDuration, 0, 1);
      return {
        definition,
        center: definition.centerTrack ? sampleStep(definition.centerTrack, t) : definition.center,
        radius: definition.radiusTrack ? sampleStep(definition.radiusTrack, t) : definition.radius,
        active,
        collectible: active && activationProgress >= 1,
        activationProgress,
        teamId: definition.teamId ? sampleStep(definition.teamId, t) : null,
        markerName: definition.markerName ? sampleStep(definition.markerName, t) : null,
      };
    }),
    smoke: (scene.smokeScreens ?? []).map((definition) => {
      const active = t >= definition.puffs[0].t && sampleStep(definition.active, t);
      return {
        definition,
        puffs: active ? sampleStep(definition.puffs, t) : [],
        radius: definition.radius,
        active,
      };
    }),
    planes: evaluatePlanes(scene, t),
    wards: (scene.wards ?? []).map((definition) => ({
      definition,
      active: t >= definition.addedAt && (definition.removedAt === undefined || t < definition.removedAt),
    })),
    consumables: (scene.consumables ?? [])
      .filter((definition) => t >= definition.start && t < definition.end)
      .map((definition) => ({ definition, remaining: definition.end - t })),
    scores: Object.fromEntries(scene.teams.map((team) => [team.id, sampleStep(team.score, t)])),
  };
}
