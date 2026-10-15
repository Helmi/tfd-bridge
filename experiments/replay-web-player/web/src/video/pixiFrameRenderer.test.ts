import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { ReplayScene } from '../types';

const fake = vi.hoisted(() => ({
  calls: [] as string[],
  create: vi.fn(),
  canvas: { width: 0, height: 0, tag: 'canvas' },
}));
vi.mock('../broadcast/BroadcastStage', () => ({ BroadcastStage: { create: fake.create } }));

import { PixiFrameRenderer } from './pixiFrameRenderer';

const scene = {
  replay: { duration: 100, perspectiveEntityId: 'me' },
  ships: [{ id: 'other', relation: 'enemy' }, { id: 'me', relation: 'self' }],
} as unknown as ReplayScene;

beforeEach(() => {
  fake.calls.length = 0;
  fake.create.mockReset();
  fake.create.mockResolvedValue({
    canvas: fake.canvas,
    update: (...args: unknown[]) => { fake.calls.push(`update:${args.slice(1).join(',')}`); },
    render: () => { fake.calls.push('render'); },
    destroy: () => { fake.calls.push('destroy'); },
  });
});

describe('PixiFrameRenderer', () => {
  it.each([
    [1, 1920, 1080],
    [4 / 3, 2560, 1440],
    [2, 3840, 2160],
  ])('scale %s gives %sx%s', async (scale, width, height) => {
    const renderer = await PixiFrameRenderer.create(scene, { scale });
    expect([renderer.width, renderer.height]).toEqual([width, height]);
    expect(fake.create).toHaveBeenCalledWith(scene, { mode: 'export', scale, mapSupersampling: false });
  });

  it('passes mapSupersampling and defaults to scale 1', async () => {
    const renderer = await PixiFrameRenderer.create(scene, { mapSupersampling: true });
    expect(fake.create).toHaveBeenCalledWith(scene, { mode: 'export', scale: 1, mapSupersampling: true });
    expect(renderer.width).toBe(1920);
  });

  it('renderFrame updates, then renders, then the canvas is readable', async () => {
    const renderer = await PixiFrameRenderer.create(scene, { scale: 1 });
    await renderer.renderFrame(42);
    expect(fake.calls).toEqual(['update:42,me', 'render']);
    expect(renderer.canvas).toBe(fake.canvas);
  });

  it('keeps the raw time for the ending and selects the perspective ship', async () => {
    const renderer = await PixiFrameRenderer.create(scene, { scale: 1 });
    await renderer.renderFrame(103);
    expect(fake.calls[0]).toBe('update:103,me');
  });

  it('propagates a stage creation failure', async () => {
    fake.create.mockRejectedValue(new Error('Layer "map" failed'));
    await expect(PixiFrameRenderer.create(scene)).rejects.toThrow('Layer "map" failed');
  });

  it('destroy releases the stage', async () => {
    const renderer = await PixiFrameRenderer.create(scene);
    renderer.destroy();
    expect(fake.calls).toContain('destroy');
  });
});
