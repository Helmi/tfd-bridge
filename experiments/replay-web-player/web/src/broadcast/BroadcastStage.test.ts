import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { sampleScene } from '../data/sampleScene';
import { evaluateScene } from '../engine/timeline';
import type { BroadcastLayer } from './types';

const fake = vi.hoisted(() => ({
  apps: [] as { init: unknown; stage: { addChild: (c: unknown) => void; children: { zIndex: number }[] }; renderer: { resolution: number; resize: (...a: number[]) => void }; render: () => void; destroy: (...a: unknown[]) => void }[],
  layers: [] as { layer: unknown; z: number }[],
  order: [] as string[],
  a11yLoaded: 0,
  a11y: { isActive: false, setAccessibilityEnabled: undefined as unknown as (enabled: boolean) => void },
}));

vi.mock('pixi.js/accessibility', () => { fake.a11yLoaded += 1; return {}; });

vi.mock('pixi.js', () => ({
  Application: class {
    stage = { children: [] as { zIndex: number }[], addChild(child: { zIndex: number }) { this.children.push(child); } };
    renderer = { resolution: 1, accessibility: fake.a11y, resize: vi.fn((_w: number, _h: number, resolution: number) => { this.renderer.resolution = resolution; }) };
    canvas = { width: 0, height: 0, getBoundingClientRect: () => ({ left: 0, top: 0, width: 960, height: 540 }) };
    init = vi.fn(async (options: { resolution: number }) => { this.renderer.resolution = options.resolution; });
    render = vi.fn(() => { fake.order.push('render'); });
    destroy = vi.fn();
    constructor() { fake.apps.push(this as never); }
  },
  Text: class {},
}));
vi.mock('./fonts', () => ({ loadBroadcastFonts: vi.fn(async () => undefined) }));
vi.mock('./icons', () => ({ loadClassIcons: vi.fn(async () => ({})), createClassIconSprite: vi.fn() }));
vi.mock('./createLayers', () => ({ createLayers: vi.fn(() => fake.layers) }));

import { BroadcastStage } from './BroadcastStage';
import { createLayers } from './createLayers';

const layer = (label: string, overrides: Partial<BroadcastLayer> = {}): BroadcastLayer => ({
  container: { label, zIndex: 0 } as never,
  update: vi.fn(() => { fake.order.push(`update:${label}`); }),
  destroy: vi.fn(),
  ...overrides,
});

beforeEach(() => {
  fake.apps.length = 0; fake.layers.length = 0; fake.order.length = 0;
  fake.a11y.isActive = false;
  fake.a11y.setAccessibilityEnabled = vi.fn((enabled: boolean) => { fake.a11y.isActive = enabled; fake.order.push('a11y'); });
});
afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });

describe('BroadcastStage.create', () => {
  it('registers the Pixi accessibility extension (roster rows need the tabindex overlay)', () => {
    expect(fake.a11yLoaded).toBe(1);
  });

  it('initialises Pixi at the requested resolution without an auto-render ticker', async () => {
    const stage = await BroadcastStage.create(sampleScene, { mode: 'interactive', scale: 1.5 });
    const options = (fake.apps[0].init as { mock: { calls: [Record<string, unknown>][] } }).mock.calls[0][0];
    expect(options).toMatchObject({ width: 1920, height: 1080, resolution: 1.5, autoStart: false, autoDensity: true });
    expect(options.accessibilityOptions).toEqual({ activateOnTab: true, enabledByDefault: true });
    expect(stage.resolution).toBe(1.5);
  });

  it('turns accessibility off in export mode', async () => {
    await BroadcastStage.create(sampleScene, { mode: 'export', scale: 2 });
    const options = (fake.apps[0].init as { mock: { calls: [Record<string, unknown>][] } }).mock.calls[0][0];
    expect(options.accessibilityOptions).toEqual({ activateOnTab: false, enabledByDefault: false });
  });

  it('adds layers with their z index and awaits every ready()', async () => {
    let resolved = false;
    const slow = layer('map', { ready: () => new Promise<void>(resolve => setTimeout(() => { resolved = true; resolve(); }, 5)) });
    fake.layers.push({ layer: slow, z: 10 }, { layer: layer('background'), z: 0 });
    await BroadcastStage.create(sampleScene, { mode: 'export' });
    expect(resolved).toBe(true);
    expect(fake.apps[0].stage.children.map(c => c.zIndex)).toEqual([10, 0]);
  });

  it('rejects with the layer name when ready() fails, and cleans up', async () => {
    const broken = layer('scoreboard', { ready: () => Promise.reject(new Error('texture 404')) });
    fake.layers.push({ layer: layer('background'), z: 0 }, { layer: broken, z: 20 });
    await expect(BroadcastStage.create(sampleScene, { mode: 'export' })).rejects.toThrow(/scoreboard.*texture 404/);
    expect(broken.destroy).toHaveBeenCalled();
    expect(fake.apps[0].destroy).toHaveBeenCalledTimes(1);
  });

  it('rejects with the layer name when ready() times out', async () => {
    vi.useFakeTimers();
    fake.layers.push({ layer: layer('map', { ready: () => new Promise<void>(() => undefined) }), z: 10 });
    const result = expect(BroadcastStage.create(sampleScene, { mode: 'export' })).rejects.toThrow(/Layer "map" did not become ready within 60 seconds/);
    await vi.advanceTimersByTimeAsync(60_000);
    await result;
    expect(fake.apps[0].destroy).toHaveBeenCalledTimes(1);
  });

  it('falls back to the z index when a layer has no label', async () => {
    fake.layers.push({ layer: layer('', { ready: () => Promise.reject(new Error('x')) }), z: 30 });
    await expect(BroadcastStage.create(sampleScene, { mode: 'export' })).rejects.toThrow(/layer@z30/);
  });
});

describe('BroadcastStage keyboard overlay', () => {
  const listeners = new Map<string, { fn: (event: unknown) => void; capture: unknown }>();
  beforeEach(() => {
    listeners.clear();
    vi.stubGlobal('addEventListener', (type: string, fn: (event: unknown) => void, capture: unknown) => listeners.set(type, { fn, capture }));
    vi.stubGlobal('removeEventListener', (type: string) => listeners.delete(type));
  });

  it('on Tab with an inactive overlay: activates it, then renders so the buttons get their positions', async () => {
    await BroadcastStage.create(sampleScene, { mode: 'interactive' });
    expect(listeners.get('keydown')?.capture).toBe(true);
    listeners.get('keydown')!.fn({ key: 'Tab' });
    expect(fake.order).toEqual(['a11y', 'render']);
    expect(fake.a11y.setAccessibilityEnabled).toHaveBeenCalledWith(true);
  });

  it('does nothing for other keys or while the overlay is already active', async () => {
    await BroadcastStage.create(sampleScene, { mode: 'interactive' });
    listeners.get('keydown')!.fn({ key: 'Enter' });
    fake.a11y.isActive = true;
    listeners.get('keydown')!.fn({ key: 'Tab' });
    expect(fake.order).toEqual([]);
  });

  it('adds no listener in export, and removes it on destroy', async () => {
    await BroadcastStage.create(sampleScene, { mode: 'export' });
    expect(listeners.has('keydown')).toBe(false);
    const stage = await BroadcastStage.create(sampleScene, { mode: 'interactive' });
    stage.destroy();
    expect(listeners.has('keydown')).toBe(false);
  });

  it('gives layers requestRender(): a render in interactive mode, nothing in export or after destroy', async () => {
    const stage = await BroadcastStage.create(sampleScene, { mode: 'interactive' });
    const ctx = vi.mocked(createLayers).mock.calls.at(-1)![0];
    ctx.requestRender();
    expect(fake.apps[0].render).toHaveBeenCalledTimes(1);
    stage.destroy();
    ctx.requestRender();
    expect(fake.apps[0].render).toHaveBeenCalledTimes(1);
    await BroadcastStage.create(sampleScene, { mode: 'export' });
    vi.mocked(createLayers).mock.calls.at(-1)![0].requestRender();
    expect(fake.apps[1].render).not.toHaveBeenCalled();
  });
});

describe('BroadcastStage frame state', () => {
  const owner = sampleScene.ships.find(s => s.id === sampleScene.replay.perspectiveEntityId) ?? sampleScene.ships.find(s => s.relation === 'self')!;

  it('calls layers in z order with owner, split and sorted fleets', async () => {
    const a = layer('a'); const b = layer('b');
    fake.layers.push({ layer: b, z: 20 }, { layer: a, z: 0 });
    const stage = await BroadcastStage.create(sampleScene, { mode: 'interactive' });
    stage.update(sampleScene, 100, owner.id);
    expect(fake.order).toEqual(['update:a', 'update:b']);
    const frame = vi.mocked(a.update).mock.calls[0][0];
    expect(frame.ownerId).toBe(owner.id);
    expect(frame.ownerTeamId).toBe(owner.teamId);
    expect(frame.ally.every(s => s.definition.teamId === owner.teamId)).toBe(true);
    expect(frame.enemy.every(s => s.definition.teamId !== owner.teamId)).toBe(true);
    expect(frame.ally.length + frame.enemy.length).toBe(sampleScene.ships.length);
    expect(frame.interactive).toBe(true);
  });

  it('keeps the raw time but evaluates the scene at the battle end', async () => {
    const a = layer('a');
    fake.layers.push({ layer: a, z: 0 });
    const stage = await BroadcastStage.create(sampleScene, { mode: 'export' });
    stage.update(sampleScene, sampleScene.replay.duration + 4, owner.id);
    const frame = vi.mocked(a.update).mock.calls[0][0];
    expect(frame.time).toBe(sampleScene.replay.duration + 4);
    expect(frame.state).toEqual(evaluateScene(sampleScene, sampleScene.replay.duration));
    expect(frame.interactive).toBe(false);
  });

  it('forwards selectShip in interactive mode and ignores it in export', async () => {
    const select = vi.fn();
    const frames: import('./types').BroadcastFrameState[] = [];
    const spy = (label: string) => layer(label, { update: f => { frames.push(f); } });
    fake.layers.push({ layer: spy('a'), z: 0 });
    const interactive = await BroadcastStage.create(sampleScene, { mode: 'interactive' });
    interactive.onSelectShip = select;
    interactive.update(sampleScene, 0, owner.id);
    frames[0].selectShip('x');
    expect(select).toHaveBeenCalledWith('x');

    fake.layers.length = 0;
    fake.layers.push({ layer: spy('b'), z: 0 });
    const exported = await BroadcastStage.create(sampleScene, { mode: 'export' });
    exported.onSelectShip = select;
    exported.update(sampleScene, 0, owner.id);
    frames[1].selectShip('y');
    expect(select).toHaveBeenCalledTimes(1);
  });

  it('render() draws the Pixi stage', async () => {
    const stage = await BroadcastStage.create(sampleScene, { mode: 'export' });
    stage.render();
    expect(fake.apps[0].render).toHaveBeenCalledTimes(1);
  });

  it('setResolution resizes only on change', async () => {
    const stage = await BroadcastStage.create(sampleScene, { mode: 'interactive', scale: 1 });
    stage.setResolution(1);
    expect(fake.apps[0].renderer.resize).not.toHaveBeenCalled();
    stage.setResolution(1.875);
    expect(fake.apps[0].renderer.resize).toHaveBeenCalledWith(1920, 1080, 1.875);
  });

  it('destroy destroys layers and the app once', async () => {
    const a = layer('a');
    fake.layers.push({ layer: a, z: 0 });
    const stage = await BroadcastStage.create(sampleScene, { mode: 'export' });
    stage.destroy();
    stage.destroy();
    expect(a.destroy).toHaveBeenCalledTimes(1);
    expect(fake.apps[0].destroy).toHaveBeenCalledTimes(1);
  });
});
