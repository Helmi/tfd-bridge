import { describe, expect, it } from 'vitest';
import { DEFAULT_LOCAL_VIDEO, loadVideoDefault, saveVideoDefault, VIDEO_DEFAULT_KEY } from './renderSettings';

const memory = (initial?: string) => {
  const map = new Map<string, string>(initial === undefined ? [] : [[VIDEO_DEFAULT_KEY, initial]]);
  return {getItem: (k: string) => map.get(k) ?? null, setItem: (k: string, v: string) => {map.set(k, v);}};
};

describe('built-in default', () => {
  it('is 1080p30, AV1 on the GPU, 8 Mbps, 10× with anti-aliasing', () => {
    expect(DEFAULT_LOCAL_VIDEO).toEqual({resolution:'1080', fps:30, codec:'av1', encoder:'gpu', bitrate:8_000_000, speed:10, mapSupersampling:true});
  });
});

describe('local video default', () => {
  it('uses the built-in default when nothing is stored', () => {
    expect(loadVideoDefault(memory())).toEqual(DEFAULT_LOCAL_VIDEO);
  });
  it('round-trips a saved selection', () => {
    const storage = memory();
    const chosen = {resolution:'2160' as const, fps:60 as const, codec:'h264' as const, encoder:'cpu' as const, bitrate:84_000_000, speed:25 as const};
    expect(saveVideoDefault(chosen, storage)).toBe(true);
    expect(loadVideoDefault(storage)).toEqual({...chosen, mapSupersampling:true});
  });
  it('keeps anti-aliasing on whatever was saved', () => {
    const storage = memory();
    saveVideoDefault({...DEFAULT_LOCAL_VIDEO, mapSupersampling:false}, storage);
    expect(loadVideoDefault(storage).mapSupersampling).toBe(true);
  });
  it.each(['not json', 'null', '42', '"text"', '[]'])('falls back to the built-in default for %s', raw => {
    expect(loadVideoDefault(memory(raw))).toEqual(DEFAULT_LOCAL_VIDEO);
  });
  it('falls back field by field for invalid or old values', () => {
    const stored = {resolution:'720', fps:24, codec:'av1', encoder:'npu', bitrate:80_000_000, speed:15};
    expect(loadVideoDefault(memory(JSON.stringify(stored)))).toEqual({...DEFAULT_LOCAL_VIDEO, codec:'av1', speed:15});
    expect(loadVideoDefault(memory(JSON.stringify({fps:60, bitrate:'12'})))).toEqual({...DEFAULT_LOCAL_VIDEO, fps:60});
  });
  it('survives unavailable storage', () => {
    const broken = {getItem: () => {throw new Error('blocked');}, setItem: () => {throw new Error('blocked');}};
    expect(loadVideoDefault(broken)).toEqual(DEFAULT_LOCAL_VIDEO);
    expect(saveVideoDefault(DEFAULT_LOCAL_VIDEO, broken)).toBe(false);
  });
});
