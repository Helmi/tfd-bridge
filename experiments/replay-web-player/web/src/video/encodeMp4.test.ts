import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ReplayScene } from '../types';

const fake = vi.hoisted(() => ({
  destroy: vi.fn(), renderFrame: vi.fn(), frameClose: vi.fn(), encoderClose: vi.fn(),
  fastStart: undefined as unknown, create: vi.fn(), emitted: [] as [number, number][], queue: [] as number[], muxerCodec: undefined as unknown, listeners: [] as (() => void)[], stall: false, hardwareOk: true, encoder: undefined as { encodeQueueSize: number } | undefined, configureError: false, encodeError: false, sizes: [] as number[],
}));
vi.mock('./sharedFrameRenderer', () => ({
  VIRTUAL_HEIGHT: 1080,
  SharedFrameRenderer: { create: fake.create },
}));
vi.mock('mp4-muxer', () => ({
  ArrayBufferTarget: class { buffer = new ArrayBuffer(fake.sizes.shift() ?? 8); },
  StreamTarget: class {
    constructor(public options: {onData: (d: Uint8Array, p: number) => void; chunked: boolean; chunkSize: number}) {}
  },
  Muxer: class {
    target: {options?: {onData: (d: Uint8Array, p: number) => void}};
    constructor(opts: {target: {options?: {onData: (d: Uint8Array, p: number) => void}}; fastStart: unknown; video: {codec: unknown}}) {
      this.target = opts.target; fake.fastStart = opts.fastStart; fake.muxerCodec = opts.video.codec;
    }
    finalize() {
      // Emit order as the real muxer: body chunk first, then the mdat-size patch at the start.
      this.target.options?.onData(new Uint8Array(100), 0);
      this.target.options?.onData(new Uint8Array(40), 100);
      this.target.options?.onData(new Uint8Array(8), 0);
    }
  },
}));
import { EncoderUnavailableError } from './codecs';
import { h264CodecString, renderSceneToMp4, STREAM_MAX_PENDING } from './encodeMp4';

const scene = { replay: {duration: 1} } as ReplayScene;
const options = {fps: 1, speed: 1};
beforeEach(() => {
  vi.clearAllMocks();
  fake.configureError = false;
  fake.encodeError = false;
  fake.sizes = [];
  fake.listeners = [];
  fake.stall = false;
  fake.hardwareOk = true;
  fake.muxerCodec = undefined;
  fake.create.mockResolvedValue({width:1920, height:1080, canvas:{}, renderFrame:fake.renderFrame, destroy:fake.destroy});
  vi.stubGlobal('VideoFrame', class {close() {fake.frameClose();}});
  vi.stubGlobal('VideoEncoder', class {
    static async isConfigSupported(config: {hardwareAcceleration?: string}) {return {supported: fake.hardwareOk || config.hardwareAcceleration !== 'prefer-hardware', config};}
    state = 'configured';
    encodeQueueSize = 0;
    configure() {if(fake.configureError) throw new Error('configuration failed');}
    constructor() {fake.encoder = this;}
    addEventListener(_type: string, listener: () => void) {fake.listeners.push(listener);}
    removeEventListener(_type: string, listener: () => void) {fake.listeners = fake.listeners.filter(l => l !== listener);}
    encode() {if(fake.encodeError) throw new Error('encode failed'); if(fake.stall) {this.encodeQueueSize = 9; fake.stall = false;}}
    async flush() {}
    close() {this.state = 'closed'; fake.encoderClose();}
  });
});
afterEach(() => vi.unstubAllGlobals());

describe('render resource lifetime and upload size enforcement', () => {
  it('encodes the complete ending at normal speed after the final battle frame', async () => {
    const result=await renderSceneToMp4(scene,{fps:30,speed:10});
    expect(result.frames).toBe(138);
    expect(fake.renderFrame.mock.calls[3][0]).toBe(1);
    expect(fake.renderFrame.mock.calls[93][0]).toBe(4);
    expect(fake.renderFrame.mock.calls[123][0]).toBe(5);
    expect(fake.renderFrame.mock.lastCall?.[0]).toBeCloseTo(5.4666666667);
  });
  it('chooses an H.264 level that also supports the frame rate', () => {
    expect(h264CodecString(1920,1080,30)).toBe('avc1.640028');
    expect(h264CodecString(1920,1080,60)).toBe('avc1.64002a');
    expect(h264CodecString(2560,1440,60)).toBe('avc1.640033');
    expect(h264CodecString(3840,2160,60)).toBe('avc1.640034');
  });
  it('uses a 1080p canvas for the default local export', async () => {
    await renderSceneToMp4(scene, options);
    expect(fake.create).toHaveBeenCalledWith(scene, {scale:1,mapSupersampling:false});
  });
  it('fails with encoder_unavailable instead of switching when the chosen encoder is unsupported', async () => {
    fake.hardwareOk = false;
    await expect(renderSceneToMp4(scene, {...options, encoder: 'gpu'})).rejects.toBeInstanceOf(EncoderUnavailableError);
    expect(fake.renderFrame).not.toHaveBeenCalled();
    expect((await renderSceneToMp4(scene, {...options, encoder: 'cpu'})).encode.hardware).toBe(false);
    expect((await renderSceneToMp4(scene, options)).encode.hardware).toBe(false); // no choice: legacy fallback
  });
  it('rejects invalid timing or bitrate before allocating a renderer', async () => {
    for (const duration of [0, -1, NaN, Infinity]) {
      await expect(renderSceneToMp4({replay:{duration}} as ReplayScene,options)).rejects.toThrow('duration');
    }
    for (const bitrate of [0,-1,NaN,Infinity]) {
      await expect(renderSceneToMp4(scene,{...options,bitrate})).rejects.toThrow('bitrate');
    }
    expect(fake.create).not.toHaveBeenCalled();
  });
  it('releases the renderer and encoder if configuration fails', async () => {
    fake.configureError = true;
    await expect(renderSceneToMp4(scene, options)).rejects.toThrow('configuration failed');
    expect(fake.destroy).toHaveBeenCalledOnce();
    expect(fake.encoderClose).toHaveBeenCalledOnce();
  });
  it('closes the raw frame even when encoding it throws', async () => {
    fake.encodeError = true;
    await expect(renderSceneToMp4(scene, options)).rejects.toThrow('encode failed');
    expect(fake.frameClose).toHaveBeenCalledOnce();
    expect(fake.destroy).toHaveBeenCalledOnce();
    expect(fake.encoderClose).toHaveBeenCalledOnce();
  });
  it('releases loaded assets when cancelled during renderer setup', async () => {
    const controller = new AbortController();
    fake.create.mockImplementationOnce(async () => {
      controller.abort();
      return {width:1920,height:1080,destroy:fake.destroy};
    });
    await expect(renderSceneToMp4(scene, {...options, signal:controller.signal})).rejects.toThrow();
    expect(fake.destroy).toHaveBeenCalledOnce();
    expect(fake.renderFrame).not.toHaveBeenCalled();
  });
  it('rejects an oversized encoded result and returns only the smaller retry', async () => {
    fake.sizes = [3_000_000, 1_000_000];
    const result = await renderSceneToMp4(scene, {...options, maxBytes:2_000_000});
    expect(result.mp4.byteLength).toBe(1_000_000);
    expect(fake.create).toHaveBeenCalledTimes(2);
    expect(fake.destroy).toHaveBeenCalledTimes(2);
  });
  it('streams muxer output to the sink in emit order without an in-memory buffer', async () => {
    const writes: [number, number][] = [];
    const sink = {write: (d: Uint8Array, p: number) => {writes.push([d.byteLength, p]);}, pending: () => 0, nextDrain: async () => undefined};
    const result = await renderSceneToMp4(scene, {...options, sink});
    expect(writes).toEqual([[100, 0], [40, 100], [8, 0]]);
    expect(fake.fastStart).toBe(false);
    expect(result.mp4.byteLength).toBe(0);
    expect(result.bytes).toBe(140);
  });
  it('waits for the sink until an upload finishes, without timers', async () => {
    const timers = vi.spyOn(globalThis, 'setTimeout');
    let pending = STREAM_MAX_PENDING + 3;
    let drains = 0;
    const sink = {write: () => undefined, pending: () => pending, nextDrain: async () => {drains++; pending--;}};
    await renderSceneToMp4(scene, {...options, sink});
    expect(drains).toBe(3);
    expect(timers).not.toHaveBeenCalled();
  });
  it('resumes a stalled encode queue on the dequeue event, without timers', async () => {
    const timers = vi.spyOn(globalThis, 'setTimeout');
    fake.stall = true;
    const work = renderSceneToMp4(scene, options);
    for (let i = 0; i < 50 && fake.listeners.length === 0; i++) await Promise.resolve();
    expect(fake.listeners.length).toBe(1);
    fake.encoder!.encodeQueueSize = 0;
    fake.listeners.forEach(l => l());
    await work;
    expect(fake.listeners).toEqual([]);
    expect(timers).not.toHaveBeenCalled();
  });
  it('encodes AV1 with the av1 muxer codec and reports GPU and timing', async () => {
    const result = await renderSceneToMp4(scene, {...options, codec: 'av1'});
    expect(fake.muxerCodec).toBe('av1');
    expect(result.encode).toMatchObject({codec: 'av1', hardware: true});
    for (const key of ['renderMsPerFrame', 'waitMsPerFrame', 'fps'] as const) expect(result.encode[key]).toBeGreaterThanOrEqual(0);
  });
  it('reports CPU when only the software encoder is accepted', async () => {
    fake.hardwareOk = false;
    const result = await renderSceneToMp4(scene, {...options, codec: 'hevc'});
    expect(fake.muxerCodec).toBe('hevc');
    expect(result.encode.hardware).toBe(false);
  });
  it('keeps budgeted renders on H.264 and defaults to H.264', async () => {
    await renderSceneToMp4(scene, {...options, codec: 'av1', maxBytes: 2_000_000});
    expect(fake.muxerCodec).toBe('avc');
    await renderSceneToMp4(scene, options);
    expect(fake.muxerCodec).toBe('avc');
  });
  it('does not combine streaming with a byte budget', async () => {
    const sink = {write: () => undefined, pending: () => 0, nextDrain: async () => undefined};
    await expect(renderSceneToMp4(scene, {...options, sink, maxBytes: 1_000_000})).rejects.toThrow('byte budget');
  });
  it('fails after bounded retries instead of returning an oversized file', async () => {
    fake.sizes = [3_000_000, 3_000_000, 3_000_000];
    await expect(renderSceneToMp4(scene, {...options,maxBytes:2_000_000})).rejects.toThrow('exceeds the upload limit');
    expect(fake.create).toHaveBeenCalledTimes(3);
    expect(fake.destroy).toHaveBeenCalledTimes(3);
  });
});
