import { videoFrameCount } from '../engine/ending';
import type { EncoderChoice, VideoCodec } from './codecs';

export interface LocalVideoSettings {
  resolution: '1080' | '1440' | '2160';
  /** Always on for local exports; the dialog no longer offers a choice. */
  mapSupersampling?: boolean;
  /** Defaults to H.264 when absent. */
  codec?: VideoCodec;
  /** Only this encoder may run; absent tries hardware, then software. */
  encoder?: EncoderChoice;
  fps: 24 | 30 | 60;
  bitrate: number;
  speed: 5 | 10 | 15 | 20 | 25;
}

export const DEFAULT_LOCAL_VIDEO: LocalVideoSettings = {
  resolution: '1080', fps: 30, codec: 'av1', encoder: 'gpu', bitrate: 8_000_000, speed: 10, mapSupersampling: true,
};

export const RESOLUTION_LABELS: Record<LocalVideoSettings['resolution'], [string, string]> = {
  '1080': ['1080p', '1920 × 1080'], '1440': ['1440p', '2560 × 1440'], '2160': ['4K', '3840 × 2160'],
};
export const RESOLUTION_OPTIONS: LocalVideoSettings['resolution'][] = ['1080', '1440', '2160'];
export const FPS_OPTIONS: (30 | 60)[] = [30, 60];
export const BITRATE_OPTIONS_MBPS = [4, 8, 12, 20, 24, 36, 48, 60, 84];
export const SPEED_OPTIONS: LocalVideoSettings['speed'][] = [5, 10, 15, 20, 25];

/** `factor` is the share of the target bitrate the encoder really reaches (see `sizeFactor`); 1 means the full target. */
export function estimateLocalVideo(battleSeconds: number, settings: LocalVideoSettings, factor = 1) {
  const frames = videoFrameCount(battleSeconds, settings.speed, settings.fps);
  const seconds = frames / settings.fps;
  return {frames, seconds, bytes: seconds * settings.bitrate / 8 * factor};
}

/**
 * Total size of a batch. Battles without a known duration count with the average of the others;
 * undefined when no battle has a duration.
 */
export function estimateBatchBytes(durations: (number | undefined)[], settings: LocalVideoSettings, factor = 1): number | undefined {
  const known = durations.filter((d): d is number => d !== undefined);
  if (!known.length) return undefined;
  const average = known.reduce((sum, d) => sum + d, 0) / known.length;
  return durations.reduce<number>((sum, d) => sum + estimateLocalVideo(d ?? average, settings, factor).bytes, 0);
}

/** Binary units (1 MB = 1,048,576 bytes), as Explorer and Discord count them. */
export function formatFileSize(bytes: number) {
  const mb = bytes / 1_048_576;
  return mb < 1024 ? `${Math.round(mb)} MB` : `${(bytes / 1_073_741_824).toFixed(1)} GB`;
}

export const VIDEO_DEFAULT_KEY = 'tfd.video.default';

export function defaultStorage(): Pick<Storage, 'getItem' | 'setItem'> | undefined {
  try { return globalThis.localStorage; } catch { return undefined; } // blocked storage throws
}

/** The user's saved default, field by field; invalid or missing fields use the built-in default. */
export function loadVideoDefault(storage = defaultStorage()): LocalVideoSettings {
  let stored: Partial<Record<keyof LocalVideoSettings, unknown>> = {};
  try {
    const parsed: unknown = JSON.parse(storage?.getItem(VIDEO_DEFAULT_KEY) ?? 'null');
    if (parsed && typeof parsed === 'object') stored = parsed;
  } catch { /* unreadable: built-in default */ }
  const pick = <T>(value: unknown, allowed: readonly T[], fallback: T): T => allowed.includes(value as T) ? value as T : fallback;
  return {
    ...DEFAULT_LOCAL_VIDEO,
    resolution: pick(stored.resolution, RESOLUTION_OPTIONS, DEFAULT_LOCAL_VIDEO.resolution),
    fps: pick(stored.fps, FPS_OPTIONS, DEFAULT_LOCAL_VIDEO.fps),
    codec: pick(stored.codec, ['h264', 'av1', 'hevc'], DEFAULT_LOCAL_VIDEO.codec),
    encoder: pick(stored.encoder, ['gpu', 'cpu'], DEFAULT_LOCAL_VIDEO.encoder),
    bitrate: pick(stored.bitrate, BITRATE_OPTIONS_MBPS.map(n => n * 1_000_000), DEFAULT_LOCAL_VIDEO.bitrate),
    speed: pick(stored.speed, SPEED_OPTIONS, DEFAULT_LOCAL_VIDEO.speed),
  };
}

/** Stores the current selection as the user's default. Returns false when storage is unavailable. */
export function saveVideoDefault(settings: LocalVideoSettings, storage = defaultStorage()): boolean {
  const {resolution, fps, codec, encoder, bitrate, speed} = settings;
  try { storage!.setItem(VIDEO_DEFAULT_KEY, JSON.stringify({resolution, fps, codec, encoder, bitrate, speed})); return true; }
  catch { return false; }
}
