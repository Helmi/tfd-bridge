import { beforeEach, describe, expect, it, vi } from 'vitest';
import { sampleScene } from '../data/sampleScene';
import { evaluateScene } from '../engine/timeline';
import type { ReplayScene } from '../types';
import { allOf, FakeContainer, FakeGraphics, FakeSprite, FakeText, visibleTexts } from './pixiFake';
import type { BroadcastFrameState, StageContext } from './types';

const load = vi.hoisted(() => ({ calls: [] as string[], hold: false, resolvers: [] as ((value: { width: number; height: number }) => void)[] }));

vi.mock('pixi.js', async () => {
  const fake = await import('./pixiFake');
  return {
    Assets: {
      load: vi.fn((url: string) => {
        load.calls.push(url);
        return load.hold ? new Promise(resolve => { load.resolvers.push(resolve as never); }) : Promise.resolve({ width: 200, height: 100 });
      }),
    },
    ColorMatrixFilter: fake.FakeColorMatrixFilter,
    Container: fake.FakeContainer,
    Graphics: fake.FakeGraphics,
    Sprite: fake.FakeSprite,
    Text: fake.FakeText,
    TextStyle: fake.FakeTextStyle,
    CanvasTextMetrics: fake.fakeCanvasTextMetrics,
  };
});

import { createPlayerPanelLayer } from './playerPanelLayer';
import { createRibbonsLayer } from './ribbonsLayer';
import { Assets } from 'pixi.js';

const owner = sampleScene.ships[0];
const other = sampleScene.ships.find(s => s.teamId !== owner.teamId)!;
const ribbonDefinitions = {
  hits: { label: 'Main battery hit', iconKey: 'hit', isSubribbon: false, imageUrl: 'data:image/png;base64,hit' },
  fires: { label: 'Set on fire', iconKey: 'fire', isSubribbon: false, imageUrl: 'data:image/png;base64,fire' },
  pens: { label: 'Penetrations', iconKey: 'pen', isSubribbon: true, imageUrl: 'data:image/png;base64,pen' },
  plain: { label: 'Plain', iconKey: 'plain', isSubribbon: false },
};
const scene: ReplayScene = {
  ...sampleScene,
  replay: { ...sampleScene.replay, perspectiveEntityId: owner.id },
  ownerSilhouette: 'data:image/png;base64,owner-only',
  ownerStats: [{ t: 0, value: { damage: 123456, potentialDamage: 555555, spottingDamage: 77777, ribbons: { hits: 7, fires: 2, pens: 5, plain: 3, zero: 0 } } }],
  ribbonDefinitions,
  damage: [{ id: 'd1', t: 5, targetId: other.id, amount: 1200 }] as ReplayScene['damage'],
  consumables: [{ id: 'c1', shipId: other.id, name: 'RegenCrew', start: 0, end: 40 }],
};

const ctx = {
  mode: 'export',
  createText: (style: Record<string, unknown>) => new FakeText(style),
  classIconSprite: () => new FakeSprite({ width: 72, height: 128 }),
} as unknown as StageContext;

function frame(selectedId: string, time = 30, target: ReplayScene = scene): BroadcastFrameState {
  const state = evaluateScene(target, time);
  return { scene: target, time, state, selectedId, ownerId: owner.id, ownerTeamId: owner.teamId, ally: [], enemy: [], interactive: false, reducedMotion: false, selectShip: () => undefined };
}

beforeEach(() => { load.calls.length = 0; load.hold = false; load.resolvers.length = 0; vi.mocked(Assets.load).mockClear(); });

describe('player panel layer', () => {
  it('labels itself for the stage and sits in the player box', () => {
    const layer = createPlayerPanelLayer(ctx, scene);
    expect(layer.container.label).toBe('player-panel');
    expect(layer.container.position).toMatchObject({ x: 1115, y: 34 });
  });

  it('shows the owner identity, three owner stats and the silhouette', async () => {
    const layer = createPlayerPanelLayer(ctx, scene);
    await layer.ready?.();
    layer.update(frame(owner.id));
    const texts = visibleTexts(layer.container as unknown as FakeContainer);
    expect(texts).toEqual(expect.arrayContaining([`[${owner.clan}]`, owner.playerName, owner.shipName, '123,456', 'DAMAGE', '555,555', 'POTENTIAL', '77,777', 'SPOTTING', '50,600', ' / 50,600 HP']));
    expect(texts).not.toContain('HEADING');
    expect(texts).not.toContain('OBSERVED HP LOST');
    const silhouettes = allOf(layer.container as unknown as FakeContainer, FakeSprite).filter(s => s.filters);
    expect(silhouettes).toHaveLength(1);
    expect(silhouettes[0].visible).toBe(true);
    // 200 x 100 texture fitted "contain" into 310 x 84.
    expect(silhouettes[0].scale.x).toBeCloseTo(0.84);
    // Frame box 310 x 84 at x 1576, y 42.
    expect(silhouettes[0].position).toMatchObject({ x: 461 + 155, y: 8 + 42 });
    expect(layer.container.position.x + 461).toBe(1576);
    // #d9d9d9 at alpha .85, the look of CSS `brightness(0) invert(.85)` with `opacity:.85`.
    const matrix = (silhouettes[0].filters![0] as { matrix: number[] }).matrix;
    const pixel = (r: number, g: number, b: number, a: number) => [0, 1, 2, 3].map(row =>
      matrix[row * 5] * r + matrix[row * 5 + 1] * g + matrix[row * 5 + 2] * b + matrix[row * 5 + 3] * a + matrix[row * 5 + 4]);
    for (const color of [[0, 0, 0], [0.9, 0.4, 0.1], [1, 1, 1]]) {
      const [r, g, b, a] = pixel(color[0], color[1], color[2], 1);
      expect([r, g, b].map(v => Math.round(v * 255))).toEqual([217, 217, 217]);
      expect(a).toBe(0.85);
    }
    expect(pixel(0.5, 0.5, 0.5, 0)[3]).toBe(0);
    expect(silhouettes[0].alpha).toBe(1);
  });

  it('places the owner stat boxes where the DOM puts them (frame y 136.9, 145.9, labels 176.9)', () => {
    const layer = createPlayerPanelLayer(ctx, scene);
    layer.update(frame(owner.id));
    const texts = allOf(layer.container as unknown as FakeContainer, FakeText);
    const frameY = (value: string) => 34 + texts.find(t => t.text === value)!.y;
    expect(frameY('123,456')).toBeCloseTo(136.9, 0);
    expect(frameY('555,555')).toBeCloseTo(145.9, 0);
    expect(frameY('77,777')).toBeCloseTo(145.9, 0);
    for (const label of ['DAMAGE', 'POTENTIAL', 'SPOTTING']) expect(frameY(label)).toBeCloseTo(176.9, 0);
  });

  it('hides owner-only data when another ship is selected', async () => {
    const layer = createPlayerPanelLayer(ctx, scene);
    await layer.ready?.();
    layer.update(frame(owner.id));
    layer.update(frame(other.id));
    const root = layer.container as unknown as FakeContainer;
    const texts = visibleTexts(root);
    expect(texts).toEqual(expect.arrayContaining([`[${other.clan}]`, other.playerName, other.shipName, 'HEADING', 'VISIBILITY', 'OBSERVED HP LOST', '1,200', '65,400']));
    expect(texts).not.toContain('123,456');
    expect(texts).not.toContain('DAMAGE');
    expect(allOf(root, FakeSprite).filter(s => s.filters)[0].visible).toBe(false);
    layer.update(frame(owner.id));
    expect(allOf(root, FakeSprite).filter(s => s.filters)[0].visible).toBe(true);
  });

  it('shows a dash for missing owner stats', () => {
    const bare: ReplayScene = { ...scene, ownerStats: undefined, ownerSilhouette: undefined };
    const layer = createPlayerPanelLayer(ctx, bare);
    layer.update(frame(owner.id, 30, bare));
    expect(visibleTexts(layer.container as unknown as FakeContainer)).toEqual(expect.arrayContaining(['—']));
  });

  it('draws the HP fill as a share of the track and skips an empty fill', () => {
    const layer = createPlayerPanelLayer(ctx, scene);
    const root = layer.container as unknown as FakeContainer;
    const fill = () => allOf(root.children.find(c => c.label === 'hp')!, FakeGraphics)[1];
    const withHealth = (value: number): ReplayScene => ({ ...scene, ships: scene.ships.map(s => s.id === other.id ? { ...s, health: [{ t: 0, value }] } : s) });
    layer.update(frame(other.id, 30, withHealth(other.maxHealth / 2)));
    expect(fill().ops[0]).toEqual({ op: 'rect', args: [0, 8.5, 175, 8] });
    layer.update(frame(other.id, 30, withHealth(0)));
    expect(fill().ops).toHaveLength(0);
    expect(visibleTexts(root)).toEqual(expect.arrayContaining(['Sunk', '0']));
  });

  it('ready() waits for the silhouette texture and loads nothing without one', async () => {
    load.hold = true;
    const layer = createPlayerPanelLayer(ctx, scene);
    let done = false;
    const ready = layer.ready!().then(() => { done = true; });
    await Promise.resolve();
    await Promise.resolve();
    expect(done).toBe(false);
    load.resolvers.forEach(resolve => resolve({ width: 200, height: 100 }));
    await ready;
    expect(done).toBe(true);
    expect(load.calls).toEqual(['data:image/png;base64,owner-only']);

    load.calls.length = 0;
    load.hold = false;
    await createPlayerPanelLayer(ctx, { ...scene, ownerSilhouette: undefined }).ready?.();
    expect(load.calls).toEqual([]);
  });

  it('rejects ready() when the texture fails', async () => {
    vi.mocked(Assets.load).mockRejectedValueOnce(new Error('404'));
    await expect(createPlayerPanelLayer(ctx, scene).ready!()).rejects.toThrow('404');
  });

  it('tints the class icon like the DOM and rotates nothing itself', () => {
    const layer = createPlayerPanelLayer(ctx, scene);
    layer.update(frame(owner.id));
    const icon = allOf(layer.container as unknown as FakeContainer, FakeSprite)[0];
    expect(icon.tint).toBe('#ff69b4');
    // 72 x 128 texture fitted into the 25 x 20 icon box.
    expect(icon.scale.x).toBeCloseTo(20 / 128);
  });
});

describe('ribbons layer', () => {
  const badgeSprites = (layer: ReturnType<typeof createRibbonsLayer>) => allOf(layer.container as unknown as FakeContainer, FakeSprite);

  it('labels itself and sits in the ribbons box', () => {
    const layer = createRibbonsLayer(ctx, scene);
    expect(layer.container.label).toBe('ribbons');
    expect(layer.container.position).toMatchObject({ x: 1115, y: 254 });
  });

  it('loads each ribbon image once and skips sub-ribbons, and waits for them', async () => {
    load.hold = true;
    const layer = createRibbonsLayer(ctx, scene);
    let done = false;
    const ready = layer.ready!().then(() => { done = true; });
    await Promise.resolve();
    await Promise.resolve();
    expect(done).toBe(false);
    expect(load.calls.sort()).toEqual(['data:image/png;base64,fire', 'data:image/png;base64,hit']);
    load.resolvers.forEach(resolve => resolve({ width: 108, height: 42 }));
    await ready;
    expect(done).toBe(true);
  });

  it('draws one badge per ribbon with its count, using the image or the label', async () => {
    const layer = createRibbonsLayer(ctx, scene);
    await layer.ready!();
    layer.update(frame(owner.id));
    const root = layer.container as unknown as FakeContainer;
    expect(visibleTexts(root)).toEqual(['2', '7', 'Plain', '3']);
    expect(badgeSprites(layer).filter(s => s.visible)).toHaveLength(2);
    expect(visibleTexts(root)).not.toContain('5');
    expect(visibleTexts(root)).not.toContain('No active consumables observed');
  });

  it('keeps nominal badges and 12 px gaps, and shrinks all badges equally when too wide', async () => {
    const layer = createRibbonsLayer(ctx, scene);
    await layer.ready!();
    layer.update(frame(owner.id));
    const row = (layer.container as unknown as FakeContainer).children.find(c => c.label === 'ribbon-list')!;
    const xs = row.children.map(c => c.position.x);
    expect(xs.slice(0, 3)).toEqual([0, 120, 240]);
    const sprite = row.children[0].children[0] as FakeSprite;
    // 200 x 100 texture fitted "contain" into 108 x 42.
    expect(sprite.scale.x).toBeCloseTo(0.42);

    const many = Object.fromEntries(Array.from({ length: 10 }, (_, i) => [`r${i}`, i + 1]));
    const crowded: ReplayScene = { ...scene, ownerStats: [{ t: 0, value: { damage: 0, potentialDamage: 0, spottingDamage: 0, ribbons: many } }] };
    const crowdedLayer = createRibbonsLayer(ctx, crowded);
    crowdedLayer.update(frame(owner.id, 30, crowded));
    const crowdedRow = (crowdedLayer.container as unknown as FakeContainer).children.find(c => c.label === 'ribbon-list')!;
    const width = (770 - 9 * 12) / 10;
    expect(crowdedRow.children[1].position.x).toBeCloseTo(width + 12);
    expect(crowdedRow.children[9].position.x + width).toBeCloseTo(770);
    expect(crowdedRow.children[0].position.y).toBeCloseTo(19 + (42 - width * 42 / 108) / 2);
  });

  it('draws nothing without owner stats', () => {
    const bare: ReplayScene = { ...scene, ownerStats: undefined };
    const layer = createRibbonsLayer(ctx, bare);
    layer.update(frame(owner.id, 30, bare));
    expect(visibleTexts(layer.container as unknown as FakeContainer)).toEqual([]);
  });

  it('shows active consumables for another ship and hides the owner ribbons', async () => {
    const layer = createRibbonsLayer(ctx, scene);
    await layer.ready!();
    layer.update(frame(owner.id));
    layer.update(frame(other.id));
    const texts = visibleTexts(layer.container as unknown as FakeContainer);
    expect(texts).toEqual(['Regen Crew ', '10s']);
  });

  it('says so when the other ship has no active consumables', () => {
    const bare: ReplayScene = { ...scene, consumables: [] };
    const layer = createRibbonsLayer(ctx, bare);
    layer.update(frame(other.id, 30, bare));
    expect(visibleTexts(layer.container as unknown as FakeContainer)).toEqual(['No active consumables observed']);
  });

  it('wraps chips onto a new row when they do not fit', () => {
    const names = ['RegenCrew', 'DamageControl', 'HydroacousticSearch', 'SurveillanceRadar', 'SmokeGenerator', 'DefensiveAntiAirFire'];
    const busy: ReplayScene = { ...scene, consumables: names.map((name, i) => ({ id: `c${i}`, shipId: other.id, name, start: 0, end: 90 })) };
    const layer = createRibbonsLayer(ctx, busy);
    layer.update(frame(other.id, 30, busy));
    const row = (layer.container as unknown as FakeContainer).children.find(c => c.label === 'consumables')!;
    const names2 = row.children.filter((c): c is FakeText => c instanceof FakeText && c.text.endsWith(' '));
    expect(names2).toHaveLength(6);
    const ys = new Set(names2.map(t => t.position.y));
    expect(ys.size).toBeGreaterThan(1);
    expect(Math.max(...names2.map(t => t.position.x))).toBeLessThan(770);
  });
});
