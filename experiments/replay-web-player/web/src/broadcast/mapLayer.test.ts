import { beforeEach, describe, expect, it, vi } from 'vitest';
import { Container, RenderTexture, type Text, type TextStyleOptions } from 'pixi.js';
import { sampleScene } from '../data/sampleScene';
import { evaluateScene } from '../engine/timeline';
import type { EvaluatedBuffZone, EvaluatedShip, ReplayScene, SceneState } from '../types';
import type { BroadcastFrameState } from './types';

const fake = vi.hoisted(() => ({ load: vi.fn() }));

vi.mock('pixi.js', async importOriginal => ({ ...(await importOriginal<typeof import('pixi.js')>()), Assets: { load: fake.load } }));
vi.mock('../engine/enhancedMap', () => ({ acquireEnhancedMapTexture: vi.fn(async () => undefined) }));

import { createMapLayer, pickShip, type MapLayerDeps } from './mapLayer';
import { Texture } from 'pixi.js';

/** A container with the bits of `Text` the layer touches. Pixi cannot measure text without a canvas. */
class FakeText extends Container {
  text = '';
  style: Record<string, unknown>;
  anchor = { set: vi.fn() };
  constructor(style: TextStyleOptions) {
    super();
    this.style = { ...style };
    Object.defineProperty(this, 'width', { get: () => this.text.length * 7 });
    Object.defineProperty(this, 'height', { get: () => 14 });
  }
}

const bounds = { minX: 0, minY: 0, maxX: 1000, maxY: 1000 };

function ship(id: string, x: number, y: number, knowledge: EvaluatedShip['knowledge'] = 'spotted'): EvaluatedShip {
  const base = evaluateScene(sampleScene, 10).ships[0];
  return { ...base, knowledge, displayPose: { ...base.displayPose, x, y }, definition: { ...base.definition, id } };
}

describe('pickShip', () => {
  const ships = [ship('near', 500, 500), ship('far', 500, 560), ship('hidden', 502, 500, 'hidden'), ship('ghost', 100, 100, 'last-known')];

  it('picks the nearest ship within 3.5% of the map width', () => {
    expect(pickShip({ x: 505, y: 501 }, ships, bounds)).toBe('near');
    expect(pickShip({ x: 500, y: 540 }, ships, bounds)).toBe('far');
  });
  it('picks nothing beyond the radius', () => {
    expect(pickShip({ x: 500, y: 520 + 36 }, [ships[0]], bounds)).toBeUndefined();
    expect(pickShip({ x: 500, y: 500 + 35 }, [ships[0]], bounds)).toBe('near');
  });
  it('ignores hidden ships even when they are closest', () => {
    expect(pickShip({ x: 502, y: 500 }, ships, bounds)).toBe('near');
    expect(pickShip({ x: 502, y: 500 }, [ships[2]], bounds)).toBeUndefined();
  });
  it('includes last-known ships', () => {
    expect(pickShip({ x: 110, y: 100 }, ships, bounds)).toBe('ghost');
  });
});

function makeDeps(overrides: Partial<MapLayerDeps> = {}) {
  const texts: FakeText[] = [];
  const renderer = { render: vi.fn() };
  const deps: MapLayerDeps = {
    renderer: renderer as never,
    resolution: 1,
    mapSupersampling: false,
    toFrame: (x, y) => ({ x, y }),
    createText: style => { const text = new FakeText(style); texts.push(text); return text as unknown as Text; },
    onSelect: vi.fn(),
    ...overrides,
  };
  return { deps, texts, renderer };
}

function frameOf(scene: ReplayScene, state: SceneState, overrides: Partial<BroadcastFrameState> = {}): BroadcastFrameState {
  return {
    scene, time: state.t, state, selectedId: state.ships[0].definition.id, ownerId: state.ships[0].definition.id,
    ownerTeamId: state.ships[0].definition.teamId, ally: [], enemy: [], interactive: true, reducedMotion: false,
    selectShip: vi.fn(), ...overrides,
  };
}

const visibleShips = (state: SceneState) => state.ships.filter(s => s.knowledge !== 'hidden');

beforeEach(() => {
  vi.stubGlobal('window', { location: { href: 'http://localhost/' } });
  fake.load.mockReset();
  fake.load.mockResolvedValue(Texture.EMPTY);
});

describe('label pool', () => {
  it('reuses the same Text instances across updates', () => {
    const { deps, texts } = makeDeps();
    const layer = createMapLayer(deps, sampleScene);
    layer.update(frameOf(sampleScene, evaluateScene(sampleScene, 10)));
    const first = [...texts];
    expect(first.length).toBeGreaterThan(0);
    const state = evaluateScene(sampleScene, 10);
    for (const shift of [5, 10, 15]) {
      const moved = { ...state, ships: state.ships.map(s => ({ ...s, displayPose: { ...s.displayPose, x: s.displayPose.x + shift } })) };
      layer.update(frameOf(sampleScene, moved));
    }
    expect(texts).toEqual(first);
    expect(texts.every(text => !text.destroyed)).toBe(true);
  });

  it('keeps one Text per ship, zone and power-up glyph and updates its string only when it changes', () => {
    const { deps, texts } = makeDeps();
    const layer = createMapLayer(deps, sampleScene);
    const state = evaluateScene(sampleScene, 10);
    const enabledZones = state.captureZones.filter(zone => zone.enabled).length;
    const buff: EvaluatedBuffZone = {
      definition: { id: 'drop-1' } as never, center: { x: 400, y: 400 }, radius: 40, active: true,
      collectible: true, activationProgress: 0, teamId: null, markerName: 'reload_inactive',
    };
    const withBuff = { ...state, buffZones: [buff] };
    layer.update(frameOf(sampleScene, withBuff));
    expect(texts).toHaveLength(visibleShips(state).length + enabledZones + 1);
    expect(texts.filter(text => text.text === '✦')).toHaveLength(1);
    const names = texts.map(text => text.text);
    layer.update(frameOf(sampleScene, withBuff));
    expect(texts).toHaveLength(names.length);
    expect(texts.map(text => text.text)).toEqual(names);
  });

  it('writes the style only when it changes', () => {
    const { deps, texts } = makeDeps();
    const layer = createMapLayer(deps, sampleScene);
    const state = evaluateScene(sampleScene, 10);
    layer.update(frameOf(sampleScene, state));
    const label = texts.find(text => text.text === state.ships[0].definition.shipName.toUpperCase())!;
    const style = label.style;
    let writes = 0;
    label.style = new Proxy(style, { set(target, key, value) { writes += 1; target[key as string] = value; return true; } });
    layer.update(frameOf(sampleScene, state));
    expect(writes).toBe(0);
    layer.update(frameOf(sampleScene, state, { selectedId: state.ships[0].definition.id }));
    expect(writes).toBe(0);
  });

  it('destroys the Text of a ship that disappears and creates a new one when it returns', () => {
    const { deps, texts } = makeDeps();
    const layer = createMapLayer(deps, sampleScene);
    const state = evaluateScene(sampleScene, 10);
    layer.update(frameOf(sampleScene, state));
    const target = state.ships[1];
    const name = target.definition.shipName.toUpperCase();
    const label = texts.find(text => text.text === name)!;
    const hidden = { ...state, ships: state.ships.map(s => s === target ? { ...s, knowledge: 'hidden' as const } : s) };
    layer.update(frameOf(sampleScene, hidden));
    expect(label.destroyed).toBe(true);
    expect(texts.filter(text => !text.destroyed)).toHaveLength(texts.length - 1);
    layer.update(frameOf(sampleScene, state));
    expect(texts.filter(text => text.text === name && !text.destroyed)).toHaveLength(1);
  });

  it('destroys every pooled Text with the layer', () => {
    const { deps, texts } = makeDeps();
    const layer = createMapLayer(deps, sampleScene);
    layer.update(frameOf(sampleScene, evaluateScene(sampleScene, 10)));
    layer.destroy();
    expect(texts.every(text => text.destroyed)).toBe(true);
    expect(layer.container.destroyed).toBe(true);
  });
});

describe('ready()', () => {
  it('resolves only after the map image and all marker textures are loaded', async () => {
    const pending: ((texture: Texture) => void)[] = [];
    fake.load.mockImplementation(() => new Promise<Texture>(resolve => { pending.push(resolve); }));
    const scene: ReplayScene = { ...sampleScene, map: { ...sampleScene.map, image: { href: 'map.png' } as never } };
    const layer = createMapLayer(makeDeps().deps, scene);
    let settled = false;
    const ready = layer.ready().then(() => { settled = true; });
    await vi.waitFor(() => expect(pending.length).toBeGreaterThanOrEqual(1));
    await Promise.resolve();
    expect(settled).toBe(false);
    // Release loads until nothing is waiting any more.
    for (let round = 0; round < 10 && !settled; round += 1) {
      const batch = pending.splice(0);
      expect(settled).toBe(false);
      batch.forEach(resolve => resolve(Texture.EMPTY));
      await new Promise(resolve => setTimeout(resolve, 0));
    }
    await ready;
    expect(settled).toBe(true);
    const urls = fake.load.mock.calls.map(call => String(call[0]));
    expect(urls).toContain('map.png');
    expect(urls.length).toBeGreaterThanOrEqual(6);
  });

  it('stays pending while a single texture is still loading', async () => {
    let release!: (texture: Texture) => void;
    fake.load.mockImplementation((url: string) => /destroyer/i.test(url)
      ? new Promise<Texture>(resolve => { release = resolve; })
      : Promise.resolve(Texture.EMPTY));
    const layer = createMapLayer(makeDeps().deps, sampleScene);
    let settled = false;
    const ready = layer.ready().then(() => { settled = true; });
    await new Promise(resolve => setTimeout(resolve, 5));
    expect(settled).toBe(false);
    release(Texture.EMPTY);
    await ready;
    expect(settled).toBe(true);
  });
});

describe('click selection', () => {
  it('selects the nearest ship through toFrame and frame.selectShip', () => {
    const { deps } = makeDeps();
    const layer = createMapLayer(deps, sampleScene);
    const state = evaluateScene(sampleScene, 10);
    const frame = frameOf(sampleScene, state);
    layer.update(frame);
    const target = visibleShips(state)[0];
    const x = 27 + (target.displayPose.x - bounds.minX) / (bounds.maxX - bounds.minX) * 1026;
    const y = 27 + (target.displayPose.y - bounds.minY) / (bounds.maxY - bounds.minY) * 1026;
    layer.container.emit('pointerdown', { clientX: x, clientY: y } as never);
    expect(frame.selectShip).toHaveBeenCalledWith(target.definition.id);
    expect(deps.onSelect).not.toHaveBeenCalled();
  });

  it('does not select on empty water and is not interactive in export frames', () => {
    const { deps } = makeDeps();
    const layer = createMapLayer(deps, sampleScene);
    const state = evaluateScene(sampleScene, 10);
    const frame = frameOf(sampleScene, state);
    layer.update(frame);
    layer.container.emit('pointerdown', { clientX: -500, clientY: -500 } as never);
    expect(frame.selectShip).not.toHaveBeenCalled();
    layer.update(frameOf(sampleScene, state, { interactive: false }));
    expect(layer.container.eventMode).toBe('none');
  });

  it('falls back to deps.onSelect when drawn without a frame', () => {
    const { deps } = makeDeps();
    const layer = createMapLayer(deps, sampleScene);
    const state = evaluateScene(sampleScene, 10);
    layer.draw({ scene: sampleScene, state, selectedId: state.ships[0].definition.id, presentation: 'interactive', width: 1080, height: 1080 });
    const target = visibleShips(state)[0];
    const x = 27 + target.displayPose.x / 1000 * 1026;
    const y = 27 + target.displayPose.y / 1000 * 1026;
    layer.container.emit('pointerdown', { clientX: x, clientY: y } as never);
    expect(deps.onSelect).toHaveBeenCalledWith(target.definition.id);
  });
});

describe('mapSupersampling', () => {
  it('draws the content into a 2x RenderTexture and shows it as a sprite', () => {
    const { deps, renderer } = makeDeps({ mapSupersampling: true, resolution: 1.5 });
    const layer = createMapLayer(deps, sampleScene);
    layer.update(frameOf(sampleScene, evaluateScene(sampleScene, 10)));
    expect(renderer.render).toHaveBeenCalledTimes(1);
    const options = renderer.render.mock.calls[0][0];
    expect(options.target).toBeInstanceOf(RenderTexture);
    expect(options.target.source.resolution).toBe(3);
    expect([options.target.width, options.target.height]).toEqual([1080, 1080]);
    expect(layer.container.children).toHaveLength(1);
    expect(layer.container.children[0].label).toBe('map-supersampled');
    expect((layer.container.children[0] as unknown as { texture: unknown }).texture).toBe(options.target);
    layer.update(frameOf(sampleScene, evaluateScene(sampleScene, 12)));
    expect(renderer.render.mock.calls[1][0].target).toBe(options.target);
  });

  it('draws straight into the layer when supersampling is off', () => {
    const { deps, renderer } = makeDeps();
    const layer = createMapLayer(deps, sampleScene);
    layer.update(frameOf(sampleScene, evaluateScene(sampleScene, 10)));
    expect(renderer.render).not.toHaveBeenCalled();
    expect(layer.container.label).toBe('map');
  });
});
