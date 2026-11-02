import { describe, expect, it, vi } from 'vitest';
import { ENDING_SECONDS } from '../engine/ending';
import { batchBattleSeconds, OLD_BRIDGE_REASON, runBatch, type BatchReplay } from './batchRender';
import { DEFAULT_LOCAL_VIDEO, type LocalVideoSettings } from './renderSettings';
import type { renderInBridge } from './bridgeRender';

// Real summaries: id is the replays-folder-relative name, filename only the basename.
const replay = (name: string, playedAt: string): BatchReplay => ({ id: `15.9.0.0/${name}.wowsreplay`, filename: `${name}.wowsreplay`, shipName: name, mapName: 'Map', playedAt });
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
    expect(calls).toEqual(['15.9.0.0/old.wowsreplay', '15.9.0.0/mid.wowsreplay', '15.9.0.0/new.wowsreplay']);
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
      if (name === '15.9.0.0/old.wowsreplay') resolve(saved);
      else { setTimeout(() => controller.abort(), 1); signal.addEventListener('abort', () => reject(new Error('Render cancelled'))); }
    });
    const results = await runBatch({ replays, settings: DEFAULT_LOCAL_VIDEO, signal: controller.signal, render });
    expect(started).toEqual(['15.9.0.0/old.wowsreplay', '15.9.0.0/mid.wowsreplay']);
    expect(results.map(r => r.status)).toEqual(['saved', 'skipped', 'skipped']);
  });

  it('returns the SavedRender of saved battles and reports only those through onSaved', async () => {
    const outcomes = [Promise.reject(new Error('boom')), Promise.resolve(undefined), Promise.resolve(saved)];
    const render = (() => outcomes.shift()!) as unknown as typeof renderInBridge;
    const onSaved: Array<[string, unknown]> = [];
    const results = await runBatch({ replays, settings: DEFAULT_LOCAL_VIDEO, signal: new AbortController().signal, render, onSaved: (r, s) => onSaved.push([r.shipName, s]) });
    expect(results[2]).toEqual({ replay: expect.objectContaining({ shipName: 'new' }), status: 'saved', saved });
    expect(onSaved).toEqual([['new', saved]]);
  });

  it('records nothing for skipped battles', async () => {
    const controller = new AbortController();
    controller.abort();
    const onSaved = vi.fn();
    await runBatch({ replays, settings: DEFAULT_LOCAL_VIDEO, signal: controller.signal, render: vi.fn() as unknown as typeof renderInBridge, onSaved });
    expect(onSaved).not.toHaveBeenCalled();
  });
});

describe('batchBattleSeconds', () => {
  const settings: LocalVideoSettings = { ...DEFAULT_LOCAL_VIDEO, fps: 30, speed: 10 };
  const render = { ...saved, frames: 30 * (60 + ENDING_SECONDS) };
  it('prefers the duration from the game', () => {
    expect(batchBattleSeconds({ ...replay('a', '2026-01-01T10:00:00Z'), durationSeconds: 1010 }, render, settings)).toBe(1010);
  });
  it('derives the battle length from the frames otherwise', () => {
    expect(batchBattleSeconds(replay('a', '2026-01-01T10:00:00Z'), render, settings)).toBeCloseTo(600);
  });
  it('never goes negative', () => {
    expect(batchBattleSeconds(replay('a', '2026-01-01T10:00:00Z'), { ...saved, frames: 1 }, settings)).toBe(0);
  });
});
