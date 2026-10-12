import { describe, expect, it } from 'vitest';
import { OLD_BRIDGE_REASON, runBatch, type BatchReplay } from './batchRender';
import { DEFAULT_LOCAL_VIDEO } from './renderSettings';
import type { renderInBridge } from './bridgeRender';

const replay = (id: string, playedAt: string): BatchReplay => ({ id, filename: `${id}.wowsreplay`, shipName: id, mapName: 'Map', playedAt });
const saved = { downloadName: 'x.mp4', elapsedMs: 1, frames: 1, bytes: 1 };
const replays = [replay('new', '2026-01-03T10:00:00Z'), replay('old', '2026-01-01T10:00:00Z'), replay('mid', '2026-01-02T10:00:00Z')];

describe('runBatch', () => {
  it('renders sequentially, oldest first, never two at once, with the shared settings', async () => {
    const calls: string[] = [];
    let running = 0;
    let maxRunning = 0;
    const render: typeof renderInBridge = async (name, _arena, _progress, _signal, options) => {
      running++; maxRunning = Math.max(maxRunning, running);
      expect(options).toBe(DEFAULT_LOCAL_VIDEO);
      calls.push(name);
      await new Promise(resolve => setTimeout(resolve, 2));
      running--;
      return saved;
    };
    const results = await runBatch({ replays, settings: DEFAULT_LOCAL_VIDEO, signal: new AbortController().signal, render });
    expect(calls).toEqual(['old.wowsreplay', 'mid.wowsreplay', 'new.wowsreplay']);
    expect(maxRunning).toBe(1);
    expect(results.map(r => r.status)).toEqual(['saved', 'saved', 'saved']);
  });

  it('continues after a failure and counts an undefined result as failed', async () => {
    const outcomes = [Promise.reject(new Error('boom')), Promise.resolve(undefined), Promise.resolve(saved)];
    const render = (() => outcomes.shift()!) as unknown as typeof renderInBridge;
    const results = await runBatch({ replays, settings: DEFAULT_LOCAL_VIDEO, signal: new AbortController().signal, render });
    expect(results.map(r => r.status)).toEqual(['failed', 'failed', 'saved']);
    expect(results[0]).toMatchObject({ error: 'boom' });
    expect(results[1]).toMatchObject({ error: OLD_BRIDGE_REASON });
  });

  it('aborts the current render and skips the rest on cancel', async () => {
    const controller = new AbortController();
    const started: string[] = [];
    const render: typeof renderInBridge = (name, _arena, _progress, signal) => new Promise((resolve, reject) => {
      started.push(name);
      if (name === 'old.wowsreplay') resolve(saved);
      else { setTimeout(() => controller.abort(), 1); signal.addEventListener('abort', () => reject(new Error('Render cancelled'))); }
    });
    const results = await runBatch({ replays, settings: DEFAULT_LOCAL_VIDEO, signal: controller.signal, render });
    expect(started).toEqual(['old.wowsreplay', 'mid.wowsreplay']);
    expect(results.map(r => r.status)).toEqual(['saved', 'skipped', 'skipped']);
  });
});
