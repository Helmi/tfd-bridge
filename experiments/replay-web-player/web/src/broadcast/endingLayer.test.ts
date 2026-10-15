import { describe, expect, it, vi } from 'vitest';
import { sampleScene } from '../data/sampleScene';
import type { ReplayScene } from '../types';
import { allOf, FakeContainer, FakeGraphics, FakeSprite, FakeText } from './pixiFake';
import { FakeCanvas } from './fogFake';
import type { BroadcastFrameState, StageContext } from './types';

const canvases: FakeCanvas[] = [];
vi.mock('pixi.js', async () => {
  const fake = await import('./pixiFake');
  const { FakeCanvas } = await import('./fogFake');
  return {
    Container: fake.FakeContainer, Graphics: fake.FakeGraphics, Sprite: fake.FakeSprite,
    DOMAdapter: { get: () => ({ createCanvas: (width: number, height: number) => { const c = new FakeCanvas(width, height); canvases.push(c); return c; } }) },
    Texture: { from: (canvas: FakeCanvas) => ({ canvas, source: { update: vi.fn() }, destroy: vi.fn() }) },
  };
});

import { createEndingLayer, paintFog, premultipliedStops } from './endingLayer';

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
    black: allOf(root, FakeGraphics)[0],
    canvas: canvases.at(-1)!.context,
  };
}

describe('ending layer', () => {
  it('is a full-frame layer that never takes pointer events', () => {
    const { root, layer } = build(scene('victory'));
    expect(layer.container.label).toBe('ending');
    expect((root as unknown as { eventMode: string }).eventMode).toBe('none');
    for (const sprite of allOf(root, FakeSprite)) expect(sprite).toMatchObject({ width: 1920, height: 1080 });
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

  it('paints the fog once per colour and destroys its texture on destroy', () => {
    const target = scene('defeat');
    const { layer, canvas } = build(target);
    layer.update(frame(target, 0));
    const first = canvas.fills.length;
    expect(first).toBe(3);
    layer.update(frame(target, 1));
    expect(canvas.fills).toHaveLength(first);
    layer.destroy();
    expect(shadow.destroy).toHaveBeenCalled();
  });
});

describe('fog composite', () => {
  // The canvas is a quarter of the 1920x1080 frame.
  const k = 0.25;
  const paint = (rgb: readonly [number, number, number] = [61, 177, 126]) => {
    const canvas = new FakeCanvas(480, 270);
    paintFog(canvas.context as never, 480, 270, rgb);
    return canvas.context;
  };
  const at = (context: ReturnType<typeof paint>, x: number, y: number) => context.alphaAt(x * k, y * k);
  // CSS: 1 - (1 - a) products, with each ellipse fading linearly to its end stop.
  const ellipse = (peak: number, end: number, dx: number, dy: number) => peak * Math.max(0, 1 - Math.hypot(dx, dy) / Math.SQRT2 / end);

  it('has no opaque base fill: every layer is a gradient', () => {
    const context = paint();
    expect(context.fills).toHaveLength(3);
    expect(context.fills.every(f => typeof f.style === 'object')).toBe(true);
  });

  it('is about half opaque at the centre, plus the two ellipses', () => {
    const upper = ellipse(0.48, 0.65, 480 / 1440, 108 / 648);
    const lower = ellipse(0.35, 0.6, 480 / 1440, 216 / 756);
    const expected = 1 - (1 - 0.5) * (1 - lower) * (1 - upper);
    expect(expected).toBeCloseTo(0.703, 2);
    expect(at(paint(), 960, 540)).toBeCloseTo(expected, 2);
  });

  it('reaches 0.92 in a far corner that no ellipse touches', () => {
    expect(at(paint(), 0, 1080)).toBeCloseTo(0.92, 2);
  });

  it('stacks the 25%/40% ellipse on top of the 75%/70% one', () => {
    const context = paint();
    const centres = context.fills.map(f => f.tx / k);
    expect(centres).toEqual([960, 1440, 480]);
  });

  it('keeps the frame visible through the right-hand panel area', () => {
    // (1450, 150) is where the lead measured the extra darkening. The old opaque base fill gave about 0.95 here.
    expect(at(paint(), 1450, 150)).toBeCloseTo(0.77, 1);
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
