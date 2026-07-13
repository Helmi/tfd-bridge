import type { LocalReplaySummary } from '../components/ReplayPicker';
import type { ReplayScene } from '../types';
import { loadReplayScene } from './importScene';

/**
 * Raw entry from `GET /player/api/replays`: bridge-core's `ReplayEntry` shape
 * (`name`, `size`, `modified_ms`) plus the optional per-replay header metadata
 * the bridge reads cheaply from each file (absent for the live tempArenaInfo).
 */
export interface BridgeReplayEntry {
  name: string;
  size: number;
  modified_ms: number;
  /** Raw WoWS `matchGroup`, e.g. "pvp", "ranked". */
  battleType?: string;
  /** Short "major.minor" client version, e.g. "15.5". */
  gameVersionShort?: string;
  /** Raw WoWS `dateTime`, e.g. "16.02.2026 19:05:19". */
  dateTime?: string;
  /** False when the recording ended before the battle did (early exit). */
  complete?: boolean;
}

interface BridgeReplayListResponse {
  generation: number;
  replays: BridgeReplayEntry[];
}

function titleCase(value: string): string {
  return value
    .replace(/[-_]+/g, ' ')
    .replace(/\b\w/g, (letter) => letter.toUpperCase());
}

/**
 * Ports `replaySummary()` from vite.config.ts (the dev-server experiment's
 * filename parser) to run client-side: the bridge only exposes
 * `{name, size, modified_ms}` per entry, so the ship/map/played-at metadata
 * the picker already renders has to be derived here from the WoWS replay
 * filename convention instead of from a Node-side directory walk.
 */
export function replayEntryToSummary(entry: BridgeReplayEntry): LocalReplaySummary {
  const segments = entry.name.split('/');
  const filename = segments[segments.length - 1] ?? entry.name;
  const match = filename.match(/^(\d{8})_(\d{6})_([A-Z0-9]+)-(.+?)_(\d+)_([^.]*)\.wowsreplay$/i);
  const typeCode = match?.[3]?.slice(2, 4).toUpperCase();
  const shipClass: LocalReplaySummary['shipClass'] = typeCode === 'SD' ? 'destroyer'
    : typeCode === 'SC' ? 'cruiser'
      : typeCode === 'SA' ? 'carrier'
        : typeCode === 'SS' ? 'submarine'
          : 'battleship';
  const modified = new Date(entry.modified_ms);
  const date = match?.[1];
  const time = match?.[2];
  const playedAt = date && time
    ? new Date(
      Number(date.slice(0, 4)), Number(date.slice(4, 6)) - 1, Number(date.slice(6, 8)),
      Number(time.slice(0, 2)), Number(time.slice(2, 4)), Number(time.slice(4, 6)),
    ).toISOString()
    : modified.toISOString();

  return {
    id: entry.name,
    filename,
    shipName: titleCase(match?.[4] ?? filename.replace(/\.wowsreplay$/i, '')),
    shipClass,
    mapName: titleCase((match?.[6] ?? 'Unknown map').replace(/^(?:NE|OC)_/i, '')),
    playedAt,
    modifiedAt: modified.toISOString(),
    size: entry.size,
    // Bridge-provided header metadata (undefined for the vite-dev fallback).
    battleType: entry.battleType,
    gameVersion: entry.gameVersionShort,
    complete: entry.complete,
  };
}

/**
 * `GET /player/api/replays` (the player-scoped enriched list: the same entries
 * as `/v1/replays` plus per-replay battle type, short version and a
 * complete/incomplete flag), filtered to `*.wowsreplay` (the list also includes
 * `tempArenaInfo.json`, the live in-progress battle) and mapped to the picker's
 * summary shape, newest battle first.
 */
export async function listBridgeReplays(signal?: AbortSignal): Promise<LocalReplaySummary[]> {
  const response = await fetch('/player/api/replays', { cache: 'no-store', signal });
  if (!response.ok) throw new Error(`Bridge replay list failed (HTTP ${response.status}).`);
  const payload = await response.json() as BridgeReplayListResponse;
  return payload.replays
    .filter((entry) => entry.name.toLowerCase().endsWith('.wowsreplay'))
    .map(replayEntryToSummary)
    .sort((left, right) => right.playedAt.localeCompare(left.playedAt));
}

// Falls back to a fixed loopback origin outside a browser (e.g. under
// vitest's node test environment) so relative scene assets still resolve
// against a valid absolute URL.
function currentOrigin(): string {
  return typeof window !== 'undefined' ? window.location.origin : 'http://127.0.0.1';
}

/**
 * `GET /v1/replays/{encodeURIComponent(name)}/scene` — the bridge decodes the
 * replay in-process (map + powerup icons inlined) and returns the scene JSON
 * directly; unlike the vite-dev experiment there is no POST-then-refetch dance.
 */
export async function fetchBridgeScene(id: string): Promise<ReplayScene> {
  const path = `/v1/replays/${encodeURIComponent(id)}/scene`;
  const response = await fetch(path, { cache: 'no-store' });
  if (!response.ok) throw new Error(`Replay scene could not be loaded (HTTP ${response.status}).`);
  return loadReplayScene(await response.json(), { baseUrl: `${currentOrigin()}${path}` });
}
