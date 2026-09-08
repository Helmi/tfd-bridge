import { videoFrameCount } from '../engine/ending';

export interface LocalVideoSettings {
  resolution: '1080' | '1440';
  mapSupersampling?: boolean;
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
