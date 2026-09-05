export type Seconds = number;
export type EntityId = string;
export type TeamId = string;

export interface TimedValue<T> {
  t: Seconds;
  value: T;
}

export interface WorldPoint {
  x: number;
  y: number;
}

/** Degrees clockwise from map north. */
export interface Pose extends WorldPoint {
  yaw: number;
  course: number;
  speed?: number;
}

export type ShipKnowledge = 'spotted' | 'last-known' | 'hidden';

export interface ShipDefinition {
  id: EntityId;
  teamId: TeamId;
  relation?: 'self' | 'ally' | 'enemy';
  divisionId?: string;
  divisionLabel?: string;
  playerName: string;
  clan?: string;
  shipName: string;
  shipClass: 'destroyer' | 'cruiser' | 'battleship' | 'carrier' | 'submarine';
  maxHealth: number;
  pose: TimedValue<Pose>[];
  health: TimedValue<number>[];
  knowledge: TimedValue<ShipKnowledge>[];
  detectedByEnemy?: TimedValue<boolean>[];
  submerged?: TimedValue<boolean>[];
}

export interface TeamDefinition {
  id: TeamId;
  name: string;
  color: string;
  score: TimedValue<number>[];
}

export interface CaptureZone {
  id: string;
  label: string;
  center: WorldPoint;
  radius: number;
  centerTrack?: TimedValue<WorldPoint>[];
  radiusTrack?: TimedValue<number>[];
  enabled?: TimedValue<boolean>[];
  owner: TimedValue<TeamId | null>[];
  invader?: TimedValue<TeamId | null>[];
  progress: TimedValue<number>[];
  contested?: TimedValue<boolean>[];
  hasInvaders?: TimedValue<boolean>[];
}

/**
 * A one-shot Arms Race powerup. Marker names deliberately remain opaque game
 * data (for example `reload_inactive`) so new powerup types do not require a
 * scene-schema change.
 */
export interface BuffZone {
  id: string;
  center: WorldPoint;
  radius: number;
  spawnsAt: Seconds;
  activatesAt?: Seconds;
  centerTrack?: TimedValue<WorldPoint>[];
  radiusTrack?: TimedValue<number>[];
  active: TimedValue<boolean>[];
  teamId?: TimedValue<TeamId | null>[];
  markerName?: TimedValue<string | null>[];
}

/** A smoke screen: puffs accumulate while laid, the cloud dissipates at once. */
export interface SmokeScreen {
  id: string;
  /** Radius of one puff, in normalized map units. */
  radius: number;
  puffs: TimedValue<WorldPoint[]>[];
  active: TimedValue<boolean>[];
}

/** One squadron generation (CV squadron, catapult fighter, spotter, airstrike). */
export interface PlaneTrack {
  id: string;
  ownerId: EntityId;
  teamId: TeamId;
  /** Opaque game species, e.g. fighter | bomber | dive | scout. */
  kind?: string;
  /** controllable | consumable | airsupport */
  category?: string;
  /** Game plane-icon subdirectory + base name (e.g. controllable/torpedo_regular). */
  iconDir?: string;
  iconBase?: string;
  /** Resolved from the owner ship, selects the own/ally/enemy icon variant. */
  relation?: 'self' | 'ally' | 'enemy';
  pose: TimedValue<WorldPoint>[];
  active: TimedValue<boolean>[];
}

/** A stationary fighter-patrol circle with an add/remove lifetime. */
export interface FighterWard {
  id: string;
  ownerId: EntityId;
  teamId: TeamId | null;
  center: WorldPoint;
  radius: number;
  addedAt: Seconds;
  removedAt?: Seconds;
}

export interface ConsumableActivation {
  id: string;
  shipId: EntityId;
  /** Game consumable name, e.g. RepairParty, Radar, HydroacousticSearch. */
  name: string;
  start: Seconds;
  end: Seconds;
}

export interface ChatMessage {
  id: string;
  t: Seconds;
  senderId?: EntityId;
  senderName: string;
  channel: string;
  message: string;
}

/** An Arms Race style pickup resolved to the collecting ship. */
export interface PickupEvent {
  t: Seconds;
  ownerId: EntityId;
  teamId: TeamId;
  zoneId?: string;
  markerName?: string;
}

export interface TrajectoryPoint extends WorldPoint {
  t: Seconds;
}

export interface OrdnanceEvent {
  id: string;
  kind: 'shell' | 'torpedo';
  sourceId: EntityId;
  targetId?: EntityId;
  teamId: TeamId;
  /** AP | HE | SAP for shells, when the projectile param resolved. */
  ammoType?: string;
  start: Seconds;
  end: Seconds;
  trajectory: TrajectoryPoint[];
  armedAt?: Seconds;
  result?: 'hit' | 'miss' | 'destroyed';
}

export interface DamageEvent {
  id: string;
  t: Seconds;
  targetId: EntityId;
  amount: number;
  /**
   * Best-effort attribution. `shell` is reliable (from the replay's shot-hit
   * records); `fire` is inferred from the recurring equal ticks; `other`
   * covers damage the single-perspective replay can't attribute (unseen
   * torpedoes, flooding).
   */
  kind: 'shell' | 'fire' | 'other';
  attackerId?: EntityId;
  /** AP | HE | SAP for gun hits, when the shell matched a salvo. */
  ammoType?: string;
  /** Dominant penetration quality, e.g. citadel | penetration | overpen. */
  quality?: string;
  /** Number of contributing shell hits in the salvo. */
  hits?: number;
}

/** Renderer-neutral output expected from replay decoding. */
export interface ReplayScene {
  schema: 'rocks.tfd.replay-scene/v0';
  replay: {
    id: string;
    title: string;
    gameVersion: string;
    /** Short "major.minor" client version, e.g. "15.5". */
    gameVersionShort?: string;
    /** Raw WoWS `matchGroup` (e.g. "pvp", "ranked"); map to a label for display. */
    battleType?: string;
    /** Raw WoWS wall-clock string, e.g. "16.02.2026 19:05:19". */
    dateTime?: string;
    /** False when the recording ended before the battle did (early exit). */
    complete?: boolean;
    duration: Seconds;
    perspectiveTeamId: TeamId;
    perspectiveEntityId?: EntityId;
    battleStart?: Seconds;
    source: 'synthetic' | 'decoded-replay';
  };
  map: {
    name: string;
    bounds: { minX: number; minY: number; maxX: number; maxY: number };
    image?: { href: string; sha256?: string };
    /** Original WoWS world-space size, retained as metadata when tracks are normalized. */
    spaceSize?: number | { width: number; height: number };
  };
  teams: TeamDefinition[];
  ships: ShipDefinition[];
  captureZones: CaptureZone[];
  buffZones: BuffZone[];
  smokeScreens?: SmokeScreen[];
  planes?: PlaneTrack[];
  wards?: FighterWard[];
  consumables?: ConsumableActivation[];
  chat?: ChatMessage[];
  pickups?: PickupEvent[];
  assets?: {
    powerupIcons?: Record<string, { href: string }>;
    planeIcons?: Record<string, { href: string }>;
  };
  ordnance: OrdnanceEvent[];
  damage: DamageEvent[];
}

export interface EvaluatedShip {
  definition: ShipDefinition;
  pose: Pose;
  displayPose: Pose;
  health: number;
  knowledge: ShipKnowledge;
  detectedByEnemy: boolean;
  submerged: boolean;
  destroyed: boolean;
}

export interface EvaluatedOrdnance {
  event: OrdnanceEvent;
  position: WorldPoint;
  previousPosition: WorldPoint;
  heading: number;
  progress: number;
  armed: boolean;
}

export interface EvaluatedCaptureZone {
  definition: CaptureZone;
  center: WorldPoint;
  radius: number;
  enabled: boolean;
  owner: TeamId | null;
  invader: TeamId | null;
  progress: number;
  contested: boolean;
  hasInvaders: boolean;
}

export interface EvaluatedBuffZone {
  definition: BuffZone;
  center: WorldPoint;
  radius: number;
  active: boolean;
  collectible: boolean;
  activationProgress: number;
  teamId: TeamId | null;
  markerName: string | null;
}

export interface EvaluatedSmokeScreen {
  definition: SmokeScreen;
  puffs: WorldPoint[];
  radius: number;
  active: boolean;
}

export interface EvaluatedPlane {
  definition: PlaneTrack;
  position: WorldPoint;
  heading: number;
  active: boolean;
}

export interface EvaluatedWard {
  definition: FighterWard;
  active: boolean;
}

export interface EvaluatedConsumable {
  definition: ConsumableActivation;
  remaining: Seconds;
}

export interface SceneState {
  t: Seconds;
  ships: EvaluatedShip[];
  ordnance: EvaluatedOrdnance[];
  captureZones: EvaluatedCaptureZone[];
  buffZones: EvaluatedBuffZone[];
  smoke: EvaluatedSmokeScreen[];
  planes: EvaluatedPlane[];
  wards: EvaluatedWard[];
  /** Consumable activations running at the requested time. */
  consumables: EvaluatedConsumable[];
  scores: Record<TeamId, number>;
}

/**
 * Proposed Rust-exporter wire contract. Times are integer milliseconds;
 * loadReplayScene() normalizes this into the renderer's seconds-based model.
 */
export interface ReplaySceneV1 {
  schema: 'tfd-replay-scene';
  version: 1;
  replay: {
    id: string;
    name: string;
    gameBuild: string;
    gameVersionShort?: string;
    battleType?: string;
    dateTime?: string;
    complete?: boolean;
    durationMs: number;
    battleStartMs: number;
    perspective: string | { playerName?: string; teamId: TeamId; entityId?: EntityId };
  };
  map: {
    name: string;
    imageUrl?: string;
    spaceSize: number | { width: number; height: number };
    coordinateSpace?: 'normalized' | 'world';
  };
  assets?: {
    powerupIcons?: Record<string, string>;
    planeIcons?: Record<string, string>;
  };
  teams: Array<{ id: TeamId; name: string; color: string }>;
  entities: Array<{
    id: EntityId;
    teamId: TeamId;
    relation: 'self' | 'ally' | 'enemy';
    divisionId?: string;
    divisionLabel?: string;
    playerName: string;
    clan?: string;
    shipName: string;
    shipCode: string;
    species: ShipDefinition['shipClass'] | 'aircarrier';
    maxHp: number;
  }>;
  tracks: {
    ships: Record<EntityId, Array<{
      t: number;
      x: number;
      y: number;
      headingDeg: number;
      courseDeg?: number;
      speedKnots?: number;
      hp: number;
      maxHp: number;
      alive: boolean;
      visible: boolean;
      lastKnown: boolean;
      detectedByEnemy: boolean;
      submerged: boolean;
      hpStale?: boolean;
    }>>;
    scores: Array<{ t: number; teams: Record<TeamId, number> }>;
    caps: Array<{
      t: number;
      id: string;
      label?: string;
      x?: number;
      y?: number;
      radius: number;
      ownerId?: TeamId | null;
      ownerTeamId?: TeamId | '-1' | null;
      invaderTeamId?: TeamId | '-1' | null;
      progress: number;
      timeRemaining?: number;
      hasInvaders?: boolean;
      contested?: boolean;
      enabled?: boolean;
    }>;
    buffs?: Array<{
      t: number;
      id: string;
      x?: number;
      y?: number;
      radius: number;
      active?: boolean;
      activationAt?: number | null;
      teamId?: TeamId | number | '-1' | null;
      markerName?: string | null;
    }>;
    smoke?: Array<{
      t: number;
      id: string;
      active: boolean;
      radius: number;
      points: WorldPoint[];
    }>;
    planes?: Array<{
      t: number;
      id: string;
      x: number;
      y: number;
      active: boolean;
    }>;
  };
  events: {
    salvos: Array<{
      id: string;
      t?: number;
      ownerId?: EntityId;
      sourceId?: EntityId;
      targetId?: EntityId;
      ammoType?: string;
      projectiles: Array<{
        id: string;
        origin?: WorldPoint;
        target?: WorldPoint;
        flightMs?: number;
        speed?: number;
        startMs?: number;
        endMs?: number;
        path?: Array<{ t: number; x: number; y: number }>;
        result?: OrdnanceEvent['result'];
      }>;
    }>;
    torpedoes: Array<{
      id: string;
      ownerId?: EntityId;
      sourceId?: EntityId;
      targetId?: EntityId;
      launchedAt?: number;
      endedAt?: number;
      samples?: Array<{ t: number; x: number; y: number; headingDeg?: number; speed?: number; armed?: boolean }>;
      startMs?: number;
      endMs?: number;
      path?: Array<{ t: number; x: number; y: number }>;
      result?: OrdnanceEvent['result'];
    }>;
    kills: Array<{
      id?: string;
      t: number;
      killerId?: EntityId;
      victimId?: EntityId;
      sourceId?: EntityId;
      targetId?: EntityId;
      cause?: string;
    }>;
    hits?: Array<{
      t: number;
      attackerId: EntityId;
      victimId: EntityId;
      ammoType?: string;
      quality: string;
    }>;
    consumables?: Array<{
      t: number;
      shipId: EntityId;
      name: string;
      durationMs: number;
    }>;
    chat?: Array<{
      t: number;
      senderId?: EntityId;
      senderName: string;
      channel: string;
      message: string;
    }>;
    pickups?: Array<{
      t: number;
      ownerId: EntityId;
      teamId: TeamId;
      zoneId?: string;
      markerName?: string;
    }>;
  };
  aviation?: Record<string, {
    id: string;
    ownerId: EntityId;
    teamId: TeamId;
    kind?: string;
    category?: string;
    iconDir?: string;
    iconBase?: string;
  }>;
  wards?: Array<{
    id: string;
    ownerId: EntityId;
    x: number;
    y: number;
    radius: number;
    addedAt: number;
    removedAt?: number;
  }>;
}
