import { afterEach, describe, expect, it, vi } from 'vitest';
import { av1CodecString, detectEncoderSupport, hevcCodecString, pickEncoderConfig } from './codecs';

afterEach(() => vi.unstubAllGlobals());

describe('AV1 and HEVC level selection', () => {
  const sizes: [string, number, number, number][] = [['1080p30', 1920, 1080, 30], ['1440p60', 2560, 1440, 60], ['2160p30', 3840, 2160, 30], ['2160p60', 3840, 2160, 60]];
  it('picks the lowest AV1 level that fits', () => {
    expect(sizes.map(([, w, h, f]) => av1CodecString(w, h, f))).toEqual(['av01.0.08M.08', 'av01.0.12M.08', 'av01.0.12M.08', 'av01.0.13M.08']);
  });
  it('picks the lowest HEVC level that fits', () => {
    expect(sizes.map(([, w, h, f]) => hevcCodecString(w, h, f))).toEqual(['hvc1.1.6.L120.B0', 'hvc1.1.6.L150.B0', 'hvc1.1.6.L150.B0', 'hvc1.1.6.L153.B0']);
  });
});

const stub = (ok: (config: VideoEncoderConfig) => boolean) => {
  const calls: VideoEncoderConfig[] = [];
  vi.stubGlobal('VideoEncoder', {isConfigSupported: async (config: VideoEncoderConfig) => {calls.push(config); return {supported: ok(config), config};}});
  return calls;
};

describe('detectEncoderSupport', () => {
  const settings = {resolution: '1080' as const, fps: 30, bitrate: 8_000_000};
  it('tests hardware and software separately for every codec', async () => {
    // H.264: GPU + CPU; AV1: CPU only; HEVC: nothing.
    const calls = stub(c => c.codec.startsWith('avc1') || (c.codec.startsWith('av01') && c.hardwareAcceleration === 'prefer-software'));
    expect(await detectEncoderSupport(settings)).toEqual({
      h264: {gpu: true, cpu: true}, av1: {gpu: false, cpu: true}, hevc: {gpu: false, cpu: false}});
    const av1 = calls.filter(c => c.codec.startsWith('av01'));
    expect(av1.map(c => c.hardwareAcceleration).sort()).toEqual(['prefer-hardware', 'prefer-software']);
    expect(av1[0]).toMatchObject({codec: 'av01.0.08M.08', width: 1920, height: 1080, framerate: 30, bitrate: 8_000_000});
  });
  it('reports a GPU-only encoder as not available on the CPU', async () => {
    stub(c => c.hardwareAcceleration === 'prefer-hardware');
    expect((await detectEncoderSupport(settings)).h264).toEqual({gpu: true, cpu: false});
  });
  it('reports everything unavailable without WebCodecs or when probing throws', async () => {
    const none = {gpu: false, cpu: false};
    vi.stubGlobal('VideoEncoder', {isConfigSupported: async () => {throw new Error('boom');}});
    expect(await detectEncoderSupport(settings)).toEqual({h264: none, av1: none, hevc: none});
    vi.unstubAllGlobals();
    expect(await detectEncoderSupport(settings)).toEqual({h264: none, av1: none, hevc: none});
  });
});

describe('pickEncoderConfig', () => {
  const base = {width: 1920, height: 1080, bitrate: 8_000_000, framerate: 30};
  it('gpu requests only prefer-hardware', async () => {
    const calls = stub(() => false);
    expect(await pickEncoderConfig('av1', base, 'gpu')).toBeNull();
    expect(calls.map(c => c.hardwareAcceleration)).toEqual(['prefer-hardware']);
  });
  it('cpu requests only prefer-software and reports software', async () => {
    const calls = stub(() => true);
    const picked = await pickEncoderConfig('av1', base, 'cpu');
    expect(calls.map(c => c.hardwareAcceleration)).toEqual(['prefer-software']);
    expect(picked?.hardware).toBe(false);
  });
  it('keeps the legacy order without a choice', async () => {
    const calls = stub(() => false);
    await pickEncoderConfig('h264', base);
    expect(calls.map(c => c.hardwareAcceleration)).toEqual(['prefer-hardware', 'no-preference', 'prefer-software']);
  });
});
