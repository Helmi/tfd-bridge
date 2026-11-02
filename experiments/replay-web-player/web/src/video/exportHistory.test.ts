import { describe, expect, it, vi } from 'vitest';
import type { SavedRender } from './encodeMp4';
import {
  addExportHistory, buildExportEntry, EXPORT_HISTORY_KEY, EXPORT_HISTORY_MAX, exportAndRecord, formatExportRow, formatExportTime, loadExportHistory, median,
  recordExport, sizeFactor,
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
      at: input.at, ship: 'Lenin', map: 'Land of Fire', battleSeconds: 760, resolution: '1080', fps: 30, bitrate: 8_000_000, codec: 'av1',
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

describe('formatExportRow', () => {
  const row = (patch: Partial<ExportHistoryEntry> = {}) => formatExportRow({...entry(1), ...patch});
  it('formats a GPU export', () => {
    expect(formatExportRow({...buildExportEntry(input, saved({frames: 1263, elapsedMs: 8600})), videoSeconds: 42.1})).toEqual({
      when: formatExportTime(input.at).replace(',', ''), ship: 'Lenin', map: 'Land of Fire', format: '1080p30 · AV1', encoder: 'GPU',
      bitrate: '8 Mbps', speed: '20×', video: '0:43', render: '9 s', size: '50 MB',
    });
    expect(row().when).not.toContain(',');
  });
  it('formats a CPU export with 1440p, 4K and GB sizes', () => {
    const cpu = buildExportEntry({...input, settings: {...input.settings, resolution: '1440', fps: 60, codec: 'h264', encoder: 'cpu', speed: 5}},
      saved({encode: {codec: 'h264', hardware: false, renderMsPerFrame: 0, waitMsPerFrame: 0, fps: 12.4}, elapsedMs: 150_400, bytes: 1_250_000_000, frames: 7200}));
    expect(formatExportRow(cpu)).toMatchObject({format: '1440p60 · H.264', encoder: 'CPU', speed: '5×', video: '2:00', render: '2:30', size: '1.2 GB'});
    expect(formatExportRow({...cpu, resolution: '2160'}).format).toBe('4K60 · H.264');
  });
  it('takes GPU/CPU from the encoder when hardware is unknown and shows a dash when neither is known', () => {
    const base = buildExportEntry(input, saved({encode: undefined}));
    expect(formatExportRow(base).encoder).toBe('GPU');
    expect(formatExportRow({...base, encoder: 'cpu'}).encoder).toBe('CPU');
    expect(formatExportRow({...base, encoder: undefined}).encoder).toBe('—');
  });
  it('shows a dash for old entries without bitrate and rounds it to one decimal', () => {
    const {bitrate: _b, ...old} = entry(1);
    expect(formatExportRow(old).bitrate).toBe('—');
    expect(row({bitrate: 12_500_000}).bitrate).toBe('12.5 Mbps');
  });
  it('prefers hardware over the requested encoder', () => {
    expect(row({encoder: 'gpu', hardware: false}).encoder).toBe('CPU');
  });
  it('rounds times as specified', () => {
    expect(row({videoSeconds: 42.1}).video).toBe('0:43');
    expect(row({videoSeconds: 60}).video).toBe('1:00');
    expect(row({renderMs: 8600}).render).toBe('9 s');
    expect(row({renderMs: 100}).render).toBe('1 s');
    expect(row({renderMs: 119_400}).render).toBe('119 s');
    expect(row({renderMs: 119_600}).render).toBe('2:00');
    expect(row({renderMs: 125_500}).render).toBe('2:06');
  });
});

describe('export history storage', () => {
  it('is empty when nothing is stored or storage is blocked', () => {
    expect(loadExportHistory(memory())).toEqual([]);
    expect(loadExportHistory(throwing)).toEqual([]);
    expect(loadExportHistory(undefined)).toEqual([]);
  });
  it('keeps newest first and at most 50', () => {
    const storage = memory();
    expect(EXPORT_HISTORY_MAX).toBe(50);
    for (let n = 0; n < 53; n++) expect(addExportHistory({...entry(1), at: `t${n}`}, storage)).toBe(true);
    const loaded = loadExportHistory(storage);
    expect(loaded).toHaveLength(50);
    expect(loaded[0].at).toBe('t52');
    expect(loaded[49].at).toBe('t3');
    expect(JSON.parse(storage.getItem(EXPORT_HISTORY_KEY)!)).toHaveLength(50);
  });
  it.each(['not json', 'null', '42', '"text"', '{"a":1}'])('treats %s as empty and still adds', raw => {
    const storage = memory(raw);
    expect(loadExportHistory(storage)).toEqual([]);
    expect(addExportHistory(entry(1), storage)).toBe(true);
    expect(loadExportHistory(storage)).toEqual([entry(1)]);
  });
  it('drops bad entries one by one and keeps the good ones', () => {
    const good = [entry(1), entry(2)];
    const bad = [null, 7, 'x', {}, {...entry(3), fps: 'fast'}, {...entry(3), resolution: '720'}, {...entry(3), codec: 'vp9'}, {...entry(3), encoder: 'tpu'}, {...entry(3), renderFps: 'x'}, {...entry(3), bytes: null}, {...entry(3), bitrate: 0}, {...entry(3), bitrate: -8}, {...entry(3), bitrate: null}, {...entry(3), bitrate: '8'}];
    const storage = memory(JSON.stringify([bad[0], good[0], ...bad.slice(1), good[1]]));
    expect(loadExportHistory(storage)).toEqual(good);
  });
  it('accepts entries without the optional fields', () => {
    const {encoder: _e, hardware: _h, renderFps: _r, bitrate: _b, ...minimal} = entry(1);
    expect(loadExportHistory(memory(JSON.stringify([minimal])))).toEqual([minimal]);
  });
  it('reports a failed write instead of throwing', () => {
    expect(addExportHistory(entry(1), throwing)).toBe(false);
  });
});

describe('formatExportTime', () => {
  it('shows day, short month and time in local time', () => {
    const at = new Date(2026, 10, 2, 10, 25).toISOString();
    expect(formatExportTime(at)).toBe('2 Nov, 10:25');
  });
});

describe('median', () => {
  it('takes the middle value, or the mean of the two middle values for an even count', () => {
    expect(median([3, 1, 2])).toBe(2);
    expect(median([4, 1, 3, 2])).toBe(2.5);
    expect(median([0.7])).toBe(0.7);
  });
});

describe('sizeFactor', () => {
  // bytes = ratio × videoSeconds × bitrate / 8, with 100 s at 8 Mbps = 100 MB at ratio 1.
  const at = (ratio: number, patch: Partial<ExportHistoryEntry> = {}): ExportHistoryEntry => ({
    ...entry(1), videoSeconds: 100, bitrate: 8_000_000, bytes: ratio * 100_000_000, ...patch,
  });
  it('uses the defaults per codec and encoder without history', () => {
    const defaults = {h264: {gpu: 0.8, cpu: 0.9}, av1: {gpu: 1.0, cpu: 0.5}, hevc: {gpu: 0.8, cpu: 0.8}} as const;
    for (const codec of ['h264', 'av1', 'hevc'] as const) for (const encoder of ['gpu', 'cpu'] as const) {
      expect(sizeFactor(codec, encoder, [])).toBe(defaults[codec][encoder]);
    }
  });
  it('takes the median of matching entries', () => {
    expect(sizeFactor('av1', 'gpu', [at(0.9), at(0.7), at(0.95)])).toBeCloseTo(0.9);
    expect(sizeFactor('av1', 'gpu', [at(0.6), at(0.8)])).toBeCloseTo(0.7);
  });
  it('needs at least two matching entries', () => {
    expect(sizeFactor('av1', 'gpu', [at(0.6)])).toBe(1.0);
    expect(sizeFactor('av1', 'gpu', [at(0.6), at(0.6, {codec: 'h264'})])).toBe(1.0);
    expect(sizeFactor('av1', 'gpu', [at(0.6), at(0.6, {encoder: 'cpu', hardware: false})])).toBe(1.0);
  });
  it('matches the encoder by hardware first, then by the requested encoder, and skips entries with neither', () => {
    expect(sizeFactor('av1', 'cpu', [at(0.4, {hardware: false, encoder: 'gpu'}), at(0.42, {hardware: false, encoder: 'gpu'})])).toBeCloseTo(0.41);
    expect(sizeFactor('av1', 'gpu', [at(0.6, {hardware: undefined}), at(0.7, {hardware: undefined})])).toBeCloseTo(0.65);
    expect(sizeFactor('av1', 'gpu', [at(0.6, {hardware: undefined, encoder: undefined}), at(0.7, {hardware: undefined, encoder: undefined})])).toBe(1.0);
  });
  it('ignores old entries without a bitrate', () => {
    const {bitrate: _b, ...old} = at(0.4);
    expect(sizeFactor('av1', 'gpu', [old as ExportHistoryEntry, old as ExportHistoryEntry, at(0.7)])).toBe(1.0);
    expect(sizeFactor('av1', 'gpu', [old as ExportHistoryEntry, at(0.7), at(0.8)])).toBeCloseTo(0.75);
  });
  it('clamps to 0.3 … 1.1', () => {
    expect(sizeFactor('av1', 'cpu', [at(0.1, {hardware: false}), at(0.2, {hardware: false})])).toBe(0.3);
    expect(sizeFactor('av1', 'gpu', [at(1.5), at(1.4)])).toBe(1.1);
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
