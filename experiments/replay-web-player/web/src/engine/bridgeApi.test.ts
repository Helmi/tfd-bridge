import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ReplaySceneV1 } from '../types';
import { fetchBridgeScene, listBridgeReplays, replayEntryToSummary } from './bridgeApi';

describe('replayEntryToSummary', () => {
  it('parses the WoWS replay filename convention, including a nested patch-folder id', () => {
    const summary = replayEntryToSummary({
      name: '13.1.0/20260615_201530_PBSD110-Daring_51_NE_north.wowsreplay',
      size: 2_048,
      modified_ms: Date.UTC(2026, 5, 15, 20, 20, 0),
    });
    expect(summary).toMatchObject({
      id: '13.1.0/20260615_201530_PBSD110-Daring_51_NE_north.wowsreplay',
      filename: '20260615_201530_PBSD110-Daring_51_NE_north.wowsreplay',
      shipName: 'Daring',
      shipClass: 'destroyer',
      mapName: 'North',
      size: 2_048,
    });
    expect(summary.playedAt).toBe(new Date(2026, 5, 15, 20, 15, 30).toISOString());
  });

  it('maps every ship-type code to its class', () => {
    const nameFor = (type: string) => `20260101_100000_PB${type}110-Ship_10_map.wowsreplay`;
    expect(replayEntryToSummary({ name: nameFor('SC'), size: 0, modified_ms: 0 }).shipClass).toBe('cruiser');
    expect(replayEntryToSummary({ name: nameFor('SA'), size: 0, modified_ms: 0 }).shipClass).toBe('carrier');
    expect(replayEntryToSummary({ name: nameFor('SS'), size: 0, modified_ms: 0 }).shipClass).toBe('submarine');
    expect(replayEntryToSummary({ name: nameFor('SB'), size: 0, modified_ms: 0 }).shipClass).toBe('battleship');
  });

  it('falls back to the filename and mtime when it does not match the naming convention', () => {
    const modifiedMs = Date.UTC(2026, 5, 1, 12, 0, 0);
    const summary = replayEntryToSummary({ name: 'weird-file.wowsreplay', size: 10, modified_ms: modifiedMs });
    expect(summary.shipName).toBe('Weird File');
    expect(summary.shipClass).toBe('battleship');
    expect(summary.playedAt).toBe(new Date(modifiedMs).toISOString());
  });
});

describe('listBridgeReplays', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('requests a page of /player/api/replays, defensively drops tempArenaInfo.json, preserves the bridge order, and carries meta + total', async () => {
    // The bridge already sorts newest-first and paginates; the client keeps that
    // order (no re-sort) so page boundaries stay stable. The mock is in bridge
    // order: carrier (newest) then cruiser.
    const fetchMock = vi.fn(async () => new Response(JSON.stringify({
      generation: 1,
      total: 42,
      offset: 0,
      replays: [
        { name: 'tempArenaInfo.json', size: 1, modified_ms: 0 },
        { name: '20260601_100000_PBSA110-Carrier_10_south.wowsreplay', size: 200, modified_ms: 2, battleType: 'pvp', gameVersionShort: '15.5', complete: true },
        { name: '20260101_100000_PBSC110-Cruiser_10_north.wowsreplay', size: 100, modified_ms: 1, battleType: 'ranked', gameVersionShort: '15.5', complete: false },
      ],
    }), { status: 200 }));
    vi.stubGlobal('fetch', fetchMock);

    const { replays, total } = await listBridgeReplays({ offset: 0, limit: 30 });
    expect(fetchMock).toHaveBeenCalledWith('/player/api/replays?offset=0&limit=30', expect.objectContaining({ cache: 'no-store' }));
    expect(total).toBe(42);
    expect(replays.map((replay) => replay.shipClass)).toEqual(['carrier', 'cruiser']);
    expect(replays[0]).toMatchObject({ battleType: 'pvp', gameVersion: '15.5', complete: true });
    expect(replays[1]).toMatchObject({ battleType: 'ranked', gameVersion: '15.5', complete: false });
  });

  it('throws on a non-OK response', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response('', { status: 500 })));
    await expect(listBridgeReplays()).rejects.toThrow(/HTTP 500/);
  });
});

// Minimal but fully-typed ReplaySceneV1: exercises the exporter's
// `--inline-assets` contract (a data: URL, not a relative ./map.png) with
// otherwise-empty tracks/events so the fixture stays load-bearing but small.
const minimalScenePayload: ReplaySceneV1 = {
  schema: 'tfd-replay-scene',
  version: 1,
  replay: {
    id: 'bridge-1',
    name: 'Bridge fixture',
    gameBuild: '15.5',
    durationMs: 60_000,
    battleStartMs: 1_700_000_000_000,
    perspective: { teamId: 'blue' },
  },
  map: { name: 'Fixture map', imageUrl: 'data:image/png;base64,AAAA', spaceSize: { width: 1_000, height: 1_000 } },
  teams: [],
  entities: [],
  tracks: { ships: {}, scores: [], caps: [] },
  events: { salvos: [], torpedoes: [], kills: [] },
};

describe('fetchBridgeScene', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('fetches the scene via a root-absolute encoded URL and loads the returned JSON object directly', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      expect(String(input)).toBe('/v1/replays/13.1.0%2Fbattle.wowsreplay/scene');
      return new Response(JSON.stringify(minimalScenePayload), { status: 200 });
    });
    vi.stubGlobal('fetch', fetchMock);

    const scene = await fetchBridgeScene('13.1.0/battle.wowsreplay');
    expect(scene.schema).toBe('rocks.tfd.replay-scene/v0');
    expect(scene.replay.duration).toBe(60);
    // The exporter's --inline-assets flag turns map.imageUrl into a data
    // URL, which resolveAsset() must pass through unchanged (it is already
    // absolute, so no per-replay asset server is required for it).
    expect(scene.map.image?.href).toBe('data:image/png;base64,AAAA');
  });

  it('throws when the bridge returns a non-OK status', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response('', { status: 404 })));
    await expect(fetchBridgeScene('missing.wowsreplay')).rejects.toThrow(/HTTP 404/);
  });
});
