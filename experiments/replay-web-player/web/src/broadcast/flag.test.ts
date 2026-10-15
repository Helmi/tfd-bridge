import { describe, expect, it } from 'vitest';
import { pixiHudEnabled, syncHudFlagFromUrl } from './flag';

const memory = (initial: Record<string, string> = {}) => {
  const data = { ...initial };
  return { data, getItem: (key: string) => data[key] ?? null, setItem: (key: string, value: string) => { data[key] = value; } };
};

describe('pixiHudEnabled', () => {
  it('is off by default', () => expect(pixiHudEnabled(memory())).toBe(false));
  it('is on only for "pixi"', () => {
    expect(pixiHudEnabled(memory({ 'tfd.hud': 'pixi' }))).toBe(true);
    expect(pixiHudEnabled(memory({ 'tfd.hud': 'dom' }))).toBe(false);
    expect(pixiHudEnabled(memory({ 'tfd.hud': 'yes' }))).toBe(false);
  });
  it('is off without storage or when storage throws', () => {
    expect(pixiHudEnabled(undefined)).toBe(false);
    expect(pixiHudEnabled({ getItem: () => { throw new Error('blocked'); }, setItem: () => undefined })).toBe(false);
  });
});

describe('syncHudFlagFromUrl', () => {
  it('writes ?hud=pixi and ?hud=dom', () => {
    const store = memory();
    syncHudFlagFromUrl('?hud=pixi', store);
    expect(store.data['tfd.hud']).toBe('pixi');
    syncHudFlagFromUrl('?x=1&hud=dom', store);
    expect(store.data['tfd.hud']).toBe('dom');
    expect(pixiHudEnabled(store)).toBe(false);
  });
  it('ignores a missing or unknown value', () => {
    const store = memory({ 'tfd.hud': 'pixi' });
    syncHudFlagFromUrl('', store);
    syncHudFlagFromUrl('?hud=other', store);
    expect(store.data['tfd.hud']).toBe('pixi');
  });
  it('does not throw when storage blocks writes', () => {
    expect(() => syncHudFlagFromUrl('?hud=pixi', { getItem: () => null, setItem: () => { throw new Error('quota'); } })).not.toThrow();
  });
});
