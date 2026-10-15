import { afterEach, describe, expect, it, vi } from 'vitest';
import { CPU_ONLY_HINT, av1CodecString, detectCodecSupport, hevcCodecString } from './codecs';

describe('CPU-only hint', () => {
  it('says usually slower for H.264/HEVC and much slower for AV1', () => {
    const base = 'No GPU encoder for these settings — the CPU encodes instead, which is';
    expect(CPU_ONLY_HINT.h264).toBe(`${base} usually slower.`);
    expect(CPU_ONLY_HINT.hevc).toBe(`${base} usually slower.`);
    expect(CPU_ONLY_HINT.av1).toBe(`${base} much slower.`);
  });
});

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

describe('detectCodecSupport', () => {
  const settings = {resolution: '1080' as const, fps: 30, bitrate: 8_000_000};
  const stub = (ok: (config: VideoEncoderConfig) => boolean) => {
    const calls: VideoEncoderConfig[] = [];
    vi.stubGlobal('VideoEncoder', {isConfigSupported: async (config: VideoEncoderConfig) => {calls.push(config); return {supported: ok(config), config};}});
    return calls;
  };
  it('maps hardware → GPU, software only → CPU only, neither → unavailable', async () => {
    const calls = stub(c => c.codec.startsWith('avc1') || (c.codec.startsWith('av01') && c.hardwareAcceleration === 'prefer-software') ? c.codec.startsWith('avc1') ? c.hardwareAcceleration === 'prefer-hardware' : true : false);
    expect(await detectCodecSupport(settings)).toEqual({h264: 'gpu', av1: 'cpu', hevc: 'unavailable'});
    const av1 = calls.filter(c => c.codec.startsWith('av01'));
    expect(av1.map(c => c.hardwareAcceleration)).toEqual(['prefer-hardware', 'prefer-software']);
    expect(av1[0]).toMatchObject({codec: 'av01.0.08M.08', width: 1920, height: 1080, framerate: 30, bitrate: 8_000_000});
  });
  it('reports everything unavailable without WebCodecs or when probing throws', async () => {
    vi.stubGlobal('VideoEncoder', {isConfigSupported: async () => {throw new Error('boom');}});
    expect(await detectCodecSupport(settings)).toEqual({h264: 'unavailable', av1: 'unavailable', hevc: 'unavailable'});
  });
});
