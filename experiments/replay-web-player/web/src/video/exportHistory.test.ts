import { describe, expect, it, vi } from 'vitest';
import type { SavedRender } from './encodeMp4';
import {
  addExportHistory, buildExportEntry, EXPORT_HISTORY_KEY, exportAndRecord, formatExportLine, loadExportHistory, recordExport,
  type ExportHistoryEntry, type ExportInput,
} from './exportHistory';

const memory = (initial?: string) => {
  const map = new Map<string, string>(initial === undefined ? [] : [[EXPORT_HISTORY_KEY, initial]]);
  return {getItem: (k: string) => map.get(k) ?? null, setItem: (k: string, v: string) => {map.set(k, v);}};
};
const throwing = {getItem: () => {throw new Error('blocked');}, setItem: () => {throw new Error('full');}};

const input: ExportInput = {
  at: '2026-11-02T10:00:00.000Z', ship: 'Lenin', map: 'Land of Fire', battleSeconds: 760,
  settings: {resolution: '1080', fps: 30, codec: 'av1', encoder: 'gpu', bitrate: 8_000_000, speed: 20},
};
const saved = (patch: Partial<SavedRender> = {}): SavedRender => ({
  downloadName: 'x.mp4', elapsedMs: 8600, frames: 1263, bytes: 52_000_000,
  encode: {codec: 'av1', hardware: true, renderMsPerFrame: 5, waitMsPerFrame: 1, fps: 139.6}, ...patch,
});
const entry = (n: number): ExportHistoryEntry => ({...buildExportEntry(input, saved()), at: `2026-11-02T10:0${n}:00.000Z`});

describe('buildExportEntry', () => {
  it('copies the settings and derives video time, render time and speed', () => {
    expect(buildExportEntry(input, saved())).toEqual({
      at: input.at, ship: 'Lenin', map: 'Land of Fire', battleSeconds: 760, resolution: '1080', fps: 30, codec: 'av1',
      encoder: 'gpu', hardware: true, speed: 20, videoSeconds: 1263 / 30, renderMs: 8600, renderFps: 139.6, bytes: 52_000_000,
    });
  });
  it('measures render fps from frames and time when the encoder reports none', () => {
    const e = buildExportEntry(input, saved({encode: undefined, frames: 1000, elapsedMs: 8000}));
    expect(e.renderFps).toBe(125);
    expect(e.hardware).toBeUndefined();
    expect(buildExportEntry(input, saved({encode: {codec: 'av1', hardware: false, renderMsPerFrame: 0, waitMsPerFrame: 0, fps: 0}, frames: 100, elapsedMs: 1000})).renderFps).toBe(100);
  });
  it('leaves render fps out when there is no time to divide by', () => {
    expect(buildExportEntry(input, saved({encode: undefined, elapsedMs: 0})).renderFps).toBeUndefined();
  });
  it('uses the H.264 default when the settings carry no codec', () => {
    const {codec: _codec, encoder: _encoder, ...bare} = input.settings;
    const e = buildExportEntry({...input, settings: bare}, saved());
    expect(e.codec).toBe('h264');
    expect(e.encoder).toBeUndefined();
  });
});

describe('formatExportLine', () => {
  it('formats a GPU export', () => {
    expect(formatExportLine({...buildExportEntry(input, saved({frames: 1263, elapsedMs: 8600})), videoSeconds: 42.1})).toEqual({
      main: 'Lenin · Land of Fire',
      rest: '1080p30 · AV1 GPU · 20× · 12:40 battle → 0:43 video · done in 9 s (140 fps) · 52 MB',
    });
  });
  it('formats a CPU export with 1440p, 4K and GB sizes', () => {
    const cpu = buildExportEntry({...input, settings: {...input.settings, resolution: '1440', fps: 60, codec: 'h264', encoder: 'cpu', speed: 5}},
      saved({encode: {codec: 'h264', hardware: false, renderMsPerFrame: 0, waitMsPerFrame: 0, fps: 12.4}, elapsedMs: 150_400, bytes: 1_250_000_000, frames: 7200}));
    expect(formatExportLine(cpu).rest).toBe('1440p60 · H.264 CPU · 5× · 12:40 battle → 2:00 video · done in 2:30 (12 fps) · 1.3 GB');
    expect(formatExportLine({...cpu, resolution: '2160'}).rest.startsWith('4K60 · ')).toBe(true);
  });
  it('takes GPU/CPU from the encoder when hardware is unknown and omits it when neither is known', () => {
    const base = buildExportEntry(input, saved({encode: undefined}));
    expect(formatExportLine(base).rest).toContain('AV1 GPU · ');
    expect(formatExportLine({...base, encoder: 'cpu'}).rest).toContain('AV1 CPU · ');
    expect(formatExportLine({...base, encoder: undefined}).rest).toContain('1080p30 · AV1 · 20×');
  });
  it('prefers hardware over the requested encoder', () => {
    expect(formatExportLine({...entry(1), encoder: 'gpu', hardware: false}).rest).toContain('AV1 CPU');
  });
  it('rounds times and rates as specified', () => {
    const line = (patch: Partial<ExportHistoryEntry>) => formatExportLine({...entry(1), ...patch}).rest;
    expect(line({videoSeconds: 42.1})).toContain('→ 0:43 video');
    expect(line({videoSeconds: 60})).toContain('→ 1:00 video');
    expect(line({battleSeconds: 759.2})).toContain('12:40 battle');
    expect(line({renderMs: 8600})).toContain('done in 9 s');
    expect(line({renderMs: 100})).toContain('done in 1 s');
    expect(line({renderMs: 119_400})).toContain('done in 119 s');
    expect(line({renderMs: 119_600})).toContain('done in 2:00');
    expect(line({renderMs: 125_500})).toContain('done in 2:06');
    expect(line({renderFps: 139.6})).toContain('(140 fps)');
  });
  it('omits the fps part when the rate is unknown', () => {
    const {renderFps: _fps, ...rest} = entry(1);
    expect(formatExportLine(rest).rest).toContain('done in 9 s · 52 MB');
  });
});

describe('export history storage', () => {
  it('is empty when nothing is stored or storage is blocked', () => {
    expect(loadExportHistory(memory())).toEqual([]);
    expect(loadExportHistory(throwing)).toEqual([]);
    expect(loadExportHistory(undefined)).toEqual([]);
  });
  it('keeps newest first and at most five', () => {
    const storage = memory();
    for (let n = 0; n < 7; n++) expect(addExportHistory({...entry(1), at: `t${n}`}, storage)).toBe(true);
    expect(loadExportHistory(storage).map(e => e.at)).toEqual(['t6', 't5', 't4', 't3', 't2']);
    expect(JSON.parse(storage.getItem(EXPORT_HISTORY_KEY)!)).toHaveLength(5);
  });
  it.each(['not json', 'null', '42', '"text"', '{"a":1}'])('treats %s as empty and still adds', raw => {
    const storage = memory(raw);
    expect(loadExportHistory(storage)).toEqual([]);
    expect(addExportHistory(entry(1), storage)).toBe(true);
    expect(loadExportHistory(storage)).toEqual([entry(1)]);
  });
  it('drops bad entries one by one and keeps the good ones', () => {
    const good = [entry(1), entry(2)];
    const bad = [null, 7, 'x', {}, {...entry(3), fps: 'fast'}, {...entry(3), resolution: '720'}, {...entry(3), codec: 'vp9'}, {...entry(3), encoder: 'tpu'}, {...entry(3), renderFps: 'x'}, {...entry(3), bytes: null}];
    const storage = memory(JSON.stringify([bad[0], good[0], ...bad.slice(1), good[1]]));
    expect(loadExportHistory(storage)).toEqual(good);
  });
  it('accepts entries without the optional fields', () => {
    const {encoder: _e, hardware: _h, renderFps: _r, ...minimal} = entry(1);
    expect(loadExportHistory(memory(JSON.stringify([minimal])))).toEqual([minimal]);
  });
  it('reports a failed write instead of throwing', () => {
    expect(addExportHistory(entry(1), throwing)).toBe(false);
  });
});

describe('recordExport', () => {
  it('builds and stores the entry', () => {
    const storage = memory();
    recordExport(input, saved(), storage);
    expect(loadExportHistory(storage)).toEqual([buildExportEntry(input, saved())]);
  });
  it('swallows a throwing storage', () => {
    expect(() => recordExport(input, saved(), throwing)).not.toThrow();
  });
});

describe('exportAndRecord', () => {
  const bridgeSaved = saved({jobId: 'bridge'});
  const localSaved = saved({downloadName: 'local.mp4'});

  it('records the bridge result once and skips the in-page export', async () => {
    const record = vi.fn(); const local = vi.fn();
    await exportAndRecord({bridge: async () => bridgeSaved, local}, record);
    expect(record).toHaveBeenCalledTimes(1);
    expect(record).toHaveBeenCalledWith(bridgeSaved);
    expect(local).not.toHaveBeenCalled();
  });
  it('falls back to the in-page export when the bridge returns undefined and records that result', async () => {
    const record = vi.fn();
    await exportAndRecord({bridge: async () => undefined, local: async () => localSaved}, record);
    expect(record).toHaveBeenCalledTimes(1);
    expect(record).toHaveBeenCalledWith(localSaved);
  });
  it('uses the in-page export when there is no bridge path', async () => {
    const record = vi.fn();
    await exportAndRecord({local: async () => localSaved}, record);
    expect(record).toHaveBeenCalledExactlyOnceWith(localSaved);
  });
  it('records nothing when the bridge throws or aborts', async () => {
    const record = vi.fn(); const local = vi.fn();
    await expect(exportAndRecord({bridge: async () => {throw new Error('Render failed');}, local}, record)).rejects.toThrow('Render failed');
    const abort = new AbortController(); abort.abort();
    await expect(exportAndRecord({bridge: async () => abort.signal.throwIfAborted() as never, local}, record)).rejects.toBeDefined();
    expect(record).not.toHaveBeenCalled();
    expect(local).not.toHaveBeenCalled();
  });
  it('records nothing when the in-page export throws', async () => {
    const record = vi.fn();
    await expect(exportAndRecord({bridge: async () => undefined, local: async () => {throw new Error('cancelled');}}, record)).rejects.toThrow('cancelled');
    expect(record).not.toHaveBeenCalled();
  });
});
