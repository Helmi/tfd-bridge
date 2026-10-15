import { describe, expect, it, vi } from 'vitest';
import { sampleScene } from '../data/sampleScene';
import type { ReplayScene } from '../types';
import { allOf, FakeContainer, FakeFillGradient, FakeGraphics, FakeSprite, FakeText } from './pixiFake';
import type { BroadcastFrameState, StageContext } from './types';

vi.mock('pixi.js', async () => {
  const fake = await import('./pixiFake');
  return {
    Container: fake.FakeContainer, Graphics: fake.FakeGraphics, Sprite: fake.FakeSprite, FillGradient: fake.FakeFillGradient,
    DOMAdapter: { get: () => ({}) }, Texture: { from: () => ({}) },
  };
});

import { createEndingLayer, premultipliedStops } from './endingLayer';

const scene = (outcome?: 'victory' | 'defeat' | 'draw', complete = true): ReplayScene =>
  ({ ...sampleScene, replay: { ...sampleScene.replay, outcome, complete } });
const duration = sampleScene.replay.duration;

const ctx = { createText: (style: Record<string, unknown>) => new FakeText(style) } as unknown as StageContext;
const shadow = { width: 480, height: 270, destroy: vi.fn() };
const frame = (target: ReplayScene, elapsed: number) => ({ scene: target, time: duration + elapsed }) as BroadcastFrameState;
function build(target: ReplayScene) {
  const layer = createEndingLayer(ctx, target, () => shadow as never);
  const root = layer.container as unknown as FakeContainer;
  return {
    layer, root,
    title: allOf(root, FakeText)[0],
    black: allOf(root, FakeGraphics)[1],
    fog: allOf(root, FakeGraphics)[0],
  };
}
const gradientsOf = (fog: FakeGraphics) => fog.ops.filter(o => o.op === 'fill').map(o => (o.args[0] as FakeFillGradient));

describe('ending layer', () => {
  it('is a full-frame layer that never takes pointer events', () => {
    const { root, layer } = build(scene('victory'));
    expect(layer.container.label).toBe('ending');
    expect((root as unknown as { eventMode: string }).eventMode).toBe('none');
    const sprite = allOf(root, FakeSprite)[0] as unknown as { width: number; height: number };
    expect(sprite).toMatchObject({ width: 1920, height: 1080 });
  });

  it('is hidden before the end and shown from the end on', () => {
    const { layer, root } = build(scene('victory'));
    layer.update(frame(scene('victory'), -0.01));
    expect(root.visible).toBe(false);
    layer.update(frame(scene('victory'), 0));
    expect(root.visible).toBe(true);
    layer.update(frame(scene('victory'), -1));
    expect(root.visible).toBe(false);
  });

  it.each([
    [scene('victory'), 'VICTORY'], [scene('defeat'), 'DEFEAT'], [scene('draw'), 'DRAW'],
    [scene(undefined, false), 'RECORDING ENDED'], [scene(undefined, true), 'REPLAY ENDED'],
  ])('shows the title %#', (target, expected) => {
    const { layer, title } = build(target);
    layer.update(frame(target, 1));
    expect(title.text).toBe(expected);
  });

  it('centres a 100 px title with the CSS look', () => {
    const { title } = build(scene('victory'));
    expect(title.position).toMatchObject({ x: 960, y: 540 });
    expect(title.anchor).toMatchObject({ x: 0.5, y: 0.5 });
    expect(title.style).toMatchObject({ fontSize: 100, fontWeight: '600', letterSpacing: 8, fill: '#f0f7f2', dropShadow: { alpha: 0.6, blur: 40, distance: 4 } });
  });

  it('holds 3 seconds, fades for 1 second, then stays black', () => {
    const target = scene('defeat');
    const { layer, black } = build(target);
    for (const [elapsed, alpha] of [[0, 0], [2.99, 0], [3.5, 0.5], [4, 1], [4.5, 1]]) {
      layer.update(frame(target, elapsed));
      expect(black.alpha).toBeCloseTo(alpha);
    }
  });

  it('draws the three fog ellipses in the outcome colour, centre gradient first', () => {
    const target = scene('victory');
    const { layer, fog } = build(target);
    layer.update(frame(target, 0));
    const [center, lower, upper] = gradientsOf(fog).map(g => g.options);
    expect(center).toMatchObject({ type: 'radial', center: { x: 960, y: 540 }, scale: 540 / 960, textureSpace: 'global' });
    expect(center.outerRadius).toBeCloseTo(Math.SQRT2 * 960);
    expect((center.colorStops as { color: string }[])[0].color).toBe('rgba(61, 177, 126, 0.5)');
    expect((center.colorStops as { color: string }[]).at(-1)!.color).toBe('rgba(3, 10, 9, 0.92)');
    expect(lower).toMatchObject({ center: { x: 1440, y: 756 }, scale: 756 / 1440 });
    expect(lower.colorStops).toEqual([
      { offset: 0, color: 'rgba(61, 177, 126, 0.35)' }, { offset: 0.6, color: 'rgba(61, 177, 126, 0)' }, { offset: 1, color: 'rgba(61, 177, 126, 0)' },
    ]);
    expect(upper).toMatchObject({ center: { x: 480, y: 432 }, scale: 648 / 1440 });
    expect((upper.colorStops as { offset: number }[])[1].offset).toBe(0.65);
  });

  it('builds the fog once per colour and destroys gradients on destroy', () => {
    const target = scene('defeat');
    const { layer, fog } = build(target);
    layer.update(frame(target, 0));
    const first = gradientsOf(fog);
    layer.update(frame(target, 1));
    expect(gradientsOf(fog)).toEqual(first);
    expect((first[0].options.colorStops as { color: string }[])[0].color).toBe('rgba(194, 65, 66, 0.5)');
    layer.destroy();
    expect(first.every(g => g.destroyed)).toBe(true);
    expect(shadow.destroy).toHaveBeenCalled();
  });
});

describe('premultiplied stops', () => {
  const from = { rgb: [61, 177, 126] as const, alpha: 0.5 };
  const to = { rgb: [3, 10, 9] as const, alpha: 0.92 };
  it('keeps both ends and weights the middle colour by alpha', () => {
    const stops = premultipliedStops(from, to, 2);
    expect(stops.map(s => s.offset)).toEqual([0, 0.5, 1]);
    expect(stops[0].color).toBe('rgba(61, 177, 126, 0.5)');
    expect(stops[2].color).toBe('rgba(3, 10, 9, 0.92)');
    // Red: (61 * .5 + 3 * .92) / 2 / .71 = 23, darker than the straight average 32.
    expect(stops[1].color).toBe('rgba(23, 69, 50, 0.71)');
  });
});
