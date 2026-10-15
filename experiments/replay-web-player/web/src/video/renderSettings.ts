import { videoFrameCount } from '../engine/ending';
import type { VideoCodec } from './codecs';

export interface LocalVideoSettings {
  resolution: '1080' | '1440' | '2160';
  mapSupersampling?: boolean;
  /** Defaults to H.264 when absent. */
  codec?: VideoCodec;
  fps: 24 | 30 | 60;
  bitrate: number;
  speed: 5 | 10 | 20;
}

export const LOCAL_VIDEO_PRESETS: {id: string; name: string; description: string; settings: LocalVideoSettings}[] = [
  {id:'compact', name:'Compact', description:'Smaller file', settings:{resolution:'1080',fps:30,bitrate:2_000_000,speed:10}},
  {id:'balanced', name:'Balanced', description:'Recommended', settings:{resolution:'1080',fps:30,bitrate:4_000_000,speed:10}},
  {id:'sharper', name:'Sharper', description:'More detail', settings:{resolution:'1080',fps:30,bitrate:6_000_000,speed:10}},
  {id:'smooth', name:'Smooth', description:'60 fps motion', settings:{resolution:'1080',fps:60,bitrate:6_000_000,speed:10}},
];
export const DEFAULT_LOCAL_VIDEO = LOCAL_VIDEO_PRESETS[1].settings;

export function estimateLocalVideo(battleSeconds: number, settings: LocalVideoSettings) {
  const frames = videoFrameCount(battleSeconds, settings.speed, settings.fps);
  const seconds = frames / settings.fps;
  return {frames, seconds, megabytes: seconds * settings.bitrate / 8 / 1_000_000};
}

export const BITRATE_OPTIONS_MBPS = [1, 2, 4, 6, 8, 12, 16, 20, 25, 30, 40, 50, 60, 80];

const SUGGESTED_MBPS: Record<LocalVideoSettings['resolution'], {standard: [number, number]; high: [number, number]}> = {
  '1080': {standard: [8, 12], high: [12, 16]},
  '1440': {standard: [16, 25], high: [25, 40]},
  '2160': {standard: [40, 50], high: [60, 80]},
};

// AV1 and HEVC reach the same quality at lower bitrates than H.264.
const SUGGESTED_MBPS_EFFICIENT: typeof SUGGESTED_MBPS = {
  '1080': {standard: [4, 8], high: [8, 12]},
  '1440': {standard: [8, 16], high: [16, 25]},
  '2160': {standard: [25, 30], high: [40, 50]},
};

/** Suggested bitrates in Mbps for a resolution, frame rate and codec. */
export function suggestedBitrates(resolution: LocalVideoSettings['resolution'], fps: LocalVideoSettings['fps'], codec: VideoCodec = 'h264') {
  const table = codec === 'h264' ? SUGGESTED_MBPS : SUGGESTED_MBPS_EFFICIENT;
  const [good, artifactFree] = table[resolution][fps === 60 ? 'high' : 'standard'];
  return {good, artifactFree};
}

export const BATTLE_LENGTH_MINUTES = [10, 15, 20];

export function formatFileSize(megabytes: number) {
  return megabytes < 1000 ? `${Math.round(megabytes)} MB` : `${(megabytes / 1000).toFixed(1)} GB`;
}

/** Expected file size for typical battle lengths with the current settings. */
export function battleLengthSizes(settings: LocalVideoSettings) {
  return BATTLE_LENGTH_MINUTES.map(minutes => {
    const megabytes = estimateLocalVideo(minutes * 60, settings).megabytes;
    return {minutes, megabytes, label: formatFileSize(megabytes)};
  });
}
