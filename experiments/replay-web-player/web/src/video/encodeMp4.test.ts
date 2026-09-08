import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ReplayScene } from '../types';

const fake = vi.hoisted(() => ({
  destroy: vi.fn(), renderFrame: vi.fn(), frameClose: vi.fn(), encoderClose: vi.fn(),
  create: vi.fn(), configureError: false, encodeError: false, sizes: [] as number[],
}));
vi.mock('./sharedFrameRenderer', () => ({
  VIRTUAL_HEIGHT: 1080,
  SharedFrameRenderer: { create: fake.create },
}));
vi.mock('mp4-muxer', () => ({
  ArrayBufferTarget: class { buffer = new ArrayBuffer(fake.sizes.shift() ?? 8); },
  Muxer: class {
    target: unknown;
    constructor(opts: {target: unknown}) { this.target = opts.target; }
    finalize() {}
  },
}));
import { h264CodecString, renderSceneToMp4 } from './encodeMp4';

const scene = { replay: {duration: 1} } as ReplayScene;
const options = {fps: 1, speed: 1};
beforeEach(() => {
  vi.clearAllMocks();
  fake.configureError = false;
  fake.encodeError = false;
  fake.sizes = [];
  fake.create.mockResolvedValue({width:1920, height:1080, canvas:{}, renderFrame:fake.renderFrame, destroy:fake.destroy});
  vi.stubGlobal('VideoFrame', class {close() {fake.frameClose();}});
  vi.stubGlobal('VideoEncoder', class {
    static async isConfigSupported(config: unknown) {return {supported:true, config};}
    state = 'configured';
    encodeQueueSize = 0;
    configure() {if(fake.configureError) throw new Error('configuration failed');}
    encode() {if(fake.encodeError) throw new Error('encode failed');}
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
  it('fails after bounded retries instead of returning an oversized file', async () => {
    fake.sizes = [3_000_000, 3_000_000, 3_000_000];
    await expect(renderSceneToMp4(scene, {...options,maxBytes:2_000_000})).rejects.toThrow('exceeds the upload limit');
    expect(fake.create).toHaveBeenCalledTimes(3);
    expect(fake.destroy).toHaveBeenCalledTimes(3);
  });
});
