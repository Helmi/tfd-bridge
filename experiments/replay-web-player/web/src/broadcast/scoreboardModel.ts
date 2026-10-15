import { shipIconColor } from '../divisions';
import { clock } from '../time';
import type { BroadcastFrameState } from './types';
import type { ShipDefinition } from '../types';

/** What the scoreboard reads from a frame. The DOM frame and the Pixi layer both build it. */
export type ScoreboardInput = Pick<BroadcastFrameState, 'scene' | 'time' | 'state' | 'selectedId' | 'ownerTeamId' | 'ally' | 'enemy'>;

/** `shipIconColor` returns the team colour as given: DOM passes 'inherit', Pixi passes a hex colour. */
export interface ScoreboardTeamColors { friendly: string; enemy: string }

export type ScoreboardSide = 'friendly' | 'enemy';

export interface ScoreboardShip {
  id: string;
  shipName: string;
  shipClass: ShipDefinition['shipClass'];
  dead: boolean;
  /** Selected pink, own division gold, else the team colour. */
  color: string;
}

export interface ScoreboardCap {
  id: string;
  label: string;
  /** Relative to the replay owner's team. */
  owner: ScoreboardSide | null;
  invader: ScoreboardSide | null;
}

export interface ScoreboardModel {
  friendlyScore: number;
  enemyScore: number;
  /** Class order, as in the fleet lists. */
  friendlyShips: ScoreboardShip[];
  /** Mirrored: the reverse of the fleet order. */
  enemyShips: ScoreboardShip[];
  /** Upper case. */
  mapLabel: string;
  clock: string;
  caps: ScoreboardCap[];
}

export function buildScoreboardModel(input: ScoreboardInput, colors: ScoreboardTeamColors): ScoreboardModel {
  const { scene, state, selectedId, ownerTeamId, ally, enemy } = input;
  const enemyTeamId = enemy[0]?.definition.teamId;
  const ship = (s: typeof ally[number], teamColor: string): ScoreboardShip => ({
    id: s.definition.id,
    shipName: s.definition.shipName,
    shipClass: s.definition.shipClass,
    dead: s.health <= 0,
    color: shipIconColor(s.definition, scene.ships, selectedId, teamColor),
  });
  const side = (team: string | null): ScoreboardSide | null => (team ? (team === ownerTeamId ? 'friendly' : 'enemy') : null);
  return {
    friendlyScore: state.scores[ownerTeamId] ?? 0,
    enemyScore: (enemyTeamId ? state.scores[enemyTeamId] : undefined) ?? 0,
    friendlyShips: ally.map(s => ship(s, colors.friendly)),
    enemyShips: [...enemy].reverse().map(s => ship(s, colors.enemy)),
    mapLabel: (scene.map.displayName ?? scene.map.name.split('/').pop()?.replace(/^\d+_(?:NE_)?/, '').replace(/_/g, ' ') ?? '').toUpperCase(),
    // The ending runs past the duration; the clock stays on the last second.
    clock: clock(Math.min(input.time, scene.replay.duration)),
    caps: state.captureZones.map(c => ({ id: c.definition.id, label: c.definition.label, owner: side(c.owner), invader: side(c.invader) })),
  };
}
