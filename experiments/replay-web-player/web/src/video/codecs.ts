// Codec strings, encoder config probing and live GPU/CPU detection (WebCodecs).
export type VideoCodec = 'h264' | 'av1' | 'hevc';
export type Resolution = '1080' | '1440' | '2160';
export type EncoderChoice = 'gpu' | 'cpu';
export type EncoderSupport = Record<VideoCodec, Record<EncoderChoice, boolean>>;

export const CODEC_LABELS: Record<VideoCodec, string> = {h264: 'H.264', av1: 'AV1', hevc: 'HEVC'};
export const CODEC_ORDER: VideoCodec[] = ['h264', 'av1', 'hevc'];
/** mp4-muxer codec for each video codec. */
export const MUXER_CODECS = {h264: 'avc', av1: 'av1', hevc: 'hevc'} as const;

export const ENCODER_UNAVAILABLE_MESSAGE = "The selected encoder isn't available for these settings. Pick another codec option.";

/** The chosen GPU/CPU encoder cannot encode this configuration; there is no silent switch. */
export class EncoderUnavailableError extends Error {
  constructor() { super(ENCODER_UNAVAILABLE_MESSAGE); this.name = 'EncoderUnavailableError'; }
}

/** Output size of each resolution (the renderer lays out 16:9 at 1080p virtual). */
export const RESOLUTION_SIZE: Record<Resolution, [number, number]> = {
  '1080': [1920, 1080], '1440': [2560, 1440], '2160': [3840, 2160],
};

/** Encoder report of one finished render. */
export interface EncodeReport {
  codec: VideoCodec;
  /** True when the accepted encoder config was hardware (GPU) accelerated. */
  hardware: boolean;
  renderMsPerFrame: number;
  waitMsPerFrame: number;
  fps: number;
}

/** H.264 High-profile codec string with a level that fits the frame size. */
export function h264CodecString(width: number, height: number, fps: number): string {
  const mb = Math.ceil(width / 16) * Math.ceil(height / 16); // macroblocks per frame
  // Both frame size and macroblocks/second matter: 1080p60 needs L4.2.
  // Limits: chromium media/parsers/h264_level_limits.cc (H.264 Annex A).
  const table: [number, number, number][] = [[8192, 245760, 0x28], [8704, 522240, 0x2a],
    [22080, 589824, 0x32], [36864, 983040, 0x33], [36864, 2073600, 0x34]];
  let level = 0x34;
  for (const [cap, rate, lv] of table) { if (mb <= cap && mb * fps <= rate) { level = lv; break; } }
  if ((width > 2048 || height > 1088) && level < 0x32) level = 0x32; // >1080p needs ≥L5.0
  return `avc1.6400${level.toString(16).padStart(2, '0')}`;
}

/** Lowest level whose picture size and sample rate limits fit; the last level otherwise. */
function pickLevel(table: [number, number, number][], width: number, height: number, fps: number): number {
  const pixels = width * height;
  for (const [maxPixels, maxRate, level] of table) { if (pixels <= maxPixels && pixels * fps <= maxRate) return level; }
  return table[table.length - 1][2];
}

// [max luma picture size, max luma samples/s, level]. AV1 spec Annex A.3 (level index), Main tier.
const AV1_LEVELS: [number, number, number][] = [
  [147_456, 4_423_680, 0], [278_784, 8_363_520, 1], [665_856, 19_975_680, 4], [1_065_024, 31_950_720, 5],
  [2_359_296, 70_778_880, 8], [2_359_296, 141_557_760, 9], [8_912_896, 267_386_880, 12],
  [8_912_896, 534_773_760, 13], [8_912_896, 1_069_547_520, 14], [8_912_896, 2_139_095_040, 15],
];
// H.265 Annex A Table A.8 (level_idc = 30 × level), Main tier.
const HEVC_LEVELS: [number, number, number][] = [
  [983_040, 33_177_600, 93], [2_228_224, 66_846_720, 120], [2_228_224, 133_693_440, 123],
  [8_912_896, 267_386_880, 150], [8_912_896, 534_773_760, 153], [8_912_896, 1_069_547_520, 156],
];

/** AV1 Main profile, 8-bit, with a level that fits the frame size and rate. */
export function av1CodecString(width: number, height: number, fps: number): string {
  return `av01.0.${String(pickLevel(AV1_LEVELS, width, height, fps)).padStart(2, '0')}M.08`;
}

/** HEVC Main profile with a level that fits the frame size and rate. */
export function hevcCodecString(width: number, height: number, fps: number): string {
  return `hvc1.1.6.L${pickLevel(HEVC_LEVELS, width, height, fps)}.B0`;
}

export function codecString(codec: VideoCodec, width: number, height: number, fps: number): string {
  return codec === 'av1' ? av1CodecString(width, height, fps)
    : codec === 'hevc' ? hevcCodecString(width, height, fps) : h264CodecString(width, height, fps);
}

export interface EncoderBase { width: number; height: number; bitrate: number; framerate: number }

export function encoderConfig(codec: VideoCodec, base: EncoderBase, acceleration: HardwareAcceleration): VideoEncoderConfig {
  return {codec: codecString(codec, base.width, base.height, base.framerate), ...base,
    hardwareAcceleration: acceleration, latencyMode: 'quality'};
}

/** Probe for a supported encoder config. `gpu` tries only hardware, `cpu` only software; without a choice: hardware, then software. */
export async function pickEncoderConfig(
  codec: VideoCodec, base: EncoderBase, encoder?: EncoderChoice,
): Promise<{config: VideoEncoderConfig; hardware: boolean} | null> {
  const accels: HardwareAcceleration[] = encoder === 'gpu' ? ['prefer-hardware'] : encoder === 'cpu' ? ['prefer-software']
    : ['prefer-hardware', 'no-preference', 'prefer-software'];
  for (const acceleration of accels) {
    const cfg = encoderConfig(codec, base, acceleration);
    try {
      const support = await VideoEncoder.isConfigSupported(cfg);
      if (support.supported) return {config: support.config ?? cfg, hardware: acceleration === 'prefer-hardware'};
    } catch { /* try next acceleration */ }
  }
  return null;
}

/** Whether the GPU and the CPU encoder each accept the exact config for every codec. */
export async function detectEncoderSupport(
  settings: {resolution: Resolution; fps: number; bitrate: number},
): Promise<EncoderSupport> {
  const [width, height] = RESOLUTION_SIZE[settings.resolution];
  const base = {width, height, bitrate: settings.bitrate, framerate: settings.fps};
  const supported = async (codec: VideoCodec, acceleration: HardwareAcceleration) => {
    try { return Boolean((await VideoEncoder.isConfigSupported(encoderConfig(codec, base, acceleration))).supported); }
    catch { return false; }
  };
  const detect = async (codec: VideoCodec) => ({gpu: await supported(codec, 'prefer-hardware'), cpu: await supported(codec, 'prefer-software')});
  if (typeof VideoEncoder === 'undefined') return {h264: {gpu: false, cpu: false}, av1: {gpu: false, cpu: false}, hevc: {gpu: false, cpu: false}};
  const [h264, av1, hevc] = await Promise.all(CODEC_ORDER.map(detect));
  return {h264, av1, hevc};
}
