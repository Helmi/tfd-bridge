// Codec strings, encoder config probing and live GPU/CPU detection (WebCodecs).
export type VideoCodec = 'h264' | 'av1' | 'hevc';
export type Resolution = '1080' | '1440' | '2160';
export type CodecSupport = 'gpu' | 'cpu' | 'unavailable';

export const CODEC_LABELS: Record<VideoCodec, string> = {h264: 'H.264', av1: 'AV1', hevc: 'HEVC'};
export const CODEC_ORDER: VideoCodec[] = ['h264', 'av1', 'hevc'];
/** mp4-muxer codec for each video codec. */
export const MUXER_CODECS = {h264: 'avc', av1: 'av1', hevc: 'hevc'} as const;

export const CODEC_HINTS: Record<VideoCodec, string> = {
  h264: 'Plays everywhere: Discord, phones, editors. Largest files.',
  av1: 'Same quality with files about a third smaller. Plays in Discord, YouTube, Chrome, Edge, Firefox and VLC; may not play on older iPhones, some TVs or older video editors.',
  hevc: 'Small files and good on Apple devices. On Windows it may need the HEVC extension and often does not play in Discord or browsers.',
};
const CPU_HINT_BASE = 'No GPU encoder for these settings — the CPU encodes instead, which is';
export const CPU_ONLY_HINT: Record<VideoCodec, string> = {
  h264: `${CPU_HINT_BASE} usually slower.`,
  hevc: `${CPU_HINT_BASE} usually slower.`,
  av1: `${CPU_HINT_BASE} much slower.`,
};

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

/** Probe for a supported encoder config, preferring hardware, then software. */
export async function pickEncoderConfig(
  codec: VideoCodec, base: EncoderBase,
): Promise<{config: VideoEncoderConfig; hardware: boolean} | null> {
  const accels: HardwareAcceleration[] = ['prefer-hardware', 'no-preference', 'prefer-software'];
  for (const acceleration of accels) {
    const cfg = encoderConfig(codec, base, acceleration);
    try {
      const support = await VideoEncoder.isConfigSupported(cfg);
      if (support.supported) return {config: support.config ?? cfg, hardware: acceleration === 'prefer-hardware'};
    } catch { /* try next acceleration */ }
  }
  return null;
}

/** GPU / CPU only / unavailable for each codec, with the config the encoder would use. */
export async function detectCodecSupport(
  settings: {resolution: Resolution; fps: number; bitrate: number},
): Promise<Record<VideoCodec, CodecSupport>> {
  const [width, height] = RESOLUTION_SIZE[settings.resolution];
  const base = {width, height, bitrate: settings.bitrate, framerate: settings.fps};
  const supported = async (codec: VideoCodec, acceleration: HardwareAcceleration) => {
    try { return Boolean((await VideoEncoder.isConfigSupported(encoderConfig(codec, base, acceleration))).supported); }
    catch { return false; }
  };
  const detect = async (codec: VideoCodec): Promise<CodecSupport> =>
    (await supported(codec, 'prefer-hardware')) ? 'gpu'
      : (await supported(codec, 'prefer-software')) ? 'cpu' : 'unavailable';
  if (typeof VideoEncoder === 'undefined') return {h264: 'unavailable', av1: 'unavailable', hevc: 'unavailable'};
  const [h264, av1, hevc] = await Promise.all(CODEC_ORDER.map(detect));
  return {h264, av1, hevc};
}
