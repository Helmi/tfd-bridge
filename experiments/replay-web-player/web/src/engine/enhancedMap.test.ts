import { webcrypto } from 'node:crypto';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const { textureFrom } = vi.hoisted(() => ({ textureFrom: vi.fn() }));
vi.mock('pixi.js', () => ({ Texture: { from: textureFrom } }));
vi.mock('../assets/maps/catalog.json', async () => {
  const { createHash } = await import('node:crypto');
  const hash = createHash('sha256').update('original').digest('hex');
  return { default: { version: 1, style: 'coastal-v1', maps: ['a', 'b', 'c', 'd', 'e'].map(id => ({
    id: `spaces/${id}`, sourceCompositeSha256: hash, width: 1520, height: 1520,
    land: `assets/maps/coastal-v1/${id}/land.webp`, water: `assets/maps/coastal-v1/${id}/water.webp`,
  })) } };
});

function browserMocks() {
  const drawImage = vi.fn();
  const decode = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
  const images: { src: string; naturalWidth: number; naturalHeight: number }[] = [];
  class MockImage {
    src = '';
    naturalWidth = 1520;
    naturalHeight = 1520;
    constructor() { images.push(this); }
    decode = decode;
  }
  const canvases: { width: number; height: number; getContext: ReturnType<typeof vi.fn> }[] = [];
  vi.stubGlobal('Image', MockImage);
  vi.stubGlobal('document', {
    baseURI: 'http://127.0.0.1:43210/player/',
    createElement: vi.fn(() => {
      const canvas = { width: 0, height: 0, getContext: vi.fn(() => ({ drawImage })) };
      canvases.push(canvas);
      return canvas;
    }),
  });
  vi.stubEnv('BASE_URL', '/player/');
  vi.stubGlobal('crypto', webcrypto);
  const fetch = vi.fn(async () => new Response('original'));
  vi.stubGlobal('fetch', fetch);
  textureFrom.mockImplementation(canvas => ({ source: { resource: canvas }, destroy: vi.fn() }));
  return { images, canvases, drawImage, decode, fetch };
}

beforeEach(() => { vi.resetModules(); textureFrom.mockReset(); });
afterEach(() => { vi.unstubAllGlobals(); vi.unstubAllEnvs(); });

describe('bundled map textures', () => {
  it('uses the original map for unknown names or changed original PNG bytes', async () => {
    const mocks = browserMocks();
    const { acquireEnhancedMapTexture: acquire } = await import('./enhancedMap');
    expect(await acquire('spaces/unknown', '/original.png')).toBeUndefined();
    expect(mocks.fetch).not.toHaveBeenCalled();
    mocks.fetch.mockImplementation(async () => new Response('updated game geography'));
    expect(await acquire('spaces/a', '/changed.png')).toBeUndefined();
    expect(mocks.fetch).toHaveBeenCalledOnce();
    expect(mocks.images).toHaveLength(0);
    expect(textureFrom).not.toHaveBeenCalled();
  });

  it('loads only the requested map, using the player base path and native layer dimensions', async () => {
    const mocks = browserMocks();
    const { acquireEnhancedMapTexture: acquire } = await import('./enhancedMap');
    const lease = await acquire('spaces/a', '/original.png');
    expect(lease).toBeDefined();
    expect(mocks.images.map(image => image.src)).toEqual([
      'http://127.0.0.1:43210/player/assets/maps/coastal-v1/a/water.webp',
      'http://127.0.0.1:43210/player/assets/maps/coastal-v1/a/land.webp',
    ]);
    expect(mocks.canvases[0]).toMatchObject({ width: 1520, height: 1520 });
    expect(mocks.drawImage.mock.calls).toEqual([[mocks.images[0], 0, 0], [mocks.images[1], 0, 0]]);
    lease!.release();
  });

  it('deduplicates concurrent source verification and layer composition', async () => {
    const mocks = browserMocks();
    let finish!: () => void;
    const gate = new Promise<void>(resolve => { finish = resolve; });
    mocks.decode.mockReturnValue(gate);
    const { acquireEnhancedMapTexture: acquire } = await import('./enhancedMap');
    const pending = Promise.all([acquire('spaces/a', '/original.png'), acquire('spaces/a', '/original.png')]);
    await vi.waitFor(() => expect(mocks.decode).toHaveBeenCalledTimes(2));
    expect(mocks.fetch).toHaveBeenCalledOnce();
    finish();
    const [first, second] = await pending;
    expect(first!.texture).toBe(second!.texture);
    expect(textureFrom).toHaveBeenCalledOnce();
    first!.release();
    expect(second!.texture.destroy).not.toHaveBeenCalled();
    second!.release();
  });

  it('falls back on failed layers and retries composition on a later acquisition', async () => {
    const mocks = browserMocks();
    mocks.decode.mockRejectedValueOnce(new Error('Missing water layer'));
    const { acquireEnhancedMapTexture: acquire } = await import('./enhancedMap');
    const href = 'data:image/png;base64,b3JpZ2luYWw=';
    expect(await acquire('spaces/a', href)).toBeUndefined();
    expect(textureFrom).not.toHaveBeenCalled();
    const retry = await acquire('spaces/a', href);
    expect(retry).toBeDefined();
    expect(mocks.fetch).toHaveBeenCalledOnce();
    retry!.release();
  });

  it('rejects incorrectly sized layers instead of stretching the shoreline', async () => {
    const mocks = browserMocks();
    mocks.decode.mockImplementation(async () => { mocks.images[0].naturalWidth = 760; });
    const { acquireEnhancedMapTexture: acquire } = await import('./enhancedMap');
    expect(await acquire('spaces/a', '/original.png')).toBeUndefined();
    expect(textureFrom).not.toHaveBeenCalled();
  });

  it('rechecks mutable URLs before reusing an already cached texture', async () => {
    const mocks = browserMocks();
    const { acquireEnhancedMapTexture: acquire } = await import('./enhancedMap');
    const initial = (await acquire('spaces/a', '/original.png'))!;
    initial.release();
    mocks.fetch.mockImplementation(async () => new Response('updated game geography'));
    expect(await acquire('spaces/a', '/original.png')).toBeUndefined();
    expect(mocks.fetch).toHaveBeenCalledTimes(2);
  });

  it('retries transient source failures instead of permanently disabling the map', async () => {
    const mocks = browserMocks();
    mocks.fetch.mockRejectedValueOnce(new Error('Temporary image fetch failure'));
    const { acquireEnhancedMapTexture: acquire } = await import('./enhancedMap');
    expect(await acquire('spaces/a', '/original.png')).toBeUndefined();
    const retry = await acquire('spaces/a', '/original.png');
    expect(retry).toBeDefined();
    expect(mocks.fetch).toHaveBeenCalledTimes(2);
    retry!.release();
  });

  it('keeps active consumers safe and retains only the two most recently released idle maps', async () => {
    browserMocks();
    const { acquireEnhancedMapTexture: acquire } = await import('./enhancedMap');
    const first = (await acquire('spaces/a', '/original.png'))!;
    const second = (await acquire('spaces/a', '/original.png'))!;
    const idle = [];
    for (const id of ['b', 'c', 'd']) {
      const lease = (await acquire(`spaces/${id}`, '/original.png'))!;
      idle.push(lease);
      lease.release();
    }
    expect(idle[0].texture.destroy).toHaveBeenCalledWith(true);
    expect(idle[1].texture.destroy).not.toHaveBeenCalled();
    expect(idle[2].texture.destroy).not.toHaveBeenCalled();
    first.release();
    first.release(); // Repeated cleanup must not release the other consumer.
    const extra = (await acquire('spaces/e', '/original.png'))!;
    extra.release();
    expect(second.texture.destroy).not.toHaveBeenCalled();
    second.release();
    expect(second.texture.destroy).not.toHaveBeenCalled();
    expect(idle[2].texture.destroy).toHaveBeenCalledWith(true);
    const reused = (await acquire('spaces/a', '/original.png'))!;
    expect(reused.texture).toBe(second.texture);
    expect(textureFrom).toHaveBeenCalledTimes(5);
    reused.release();
  });
});
