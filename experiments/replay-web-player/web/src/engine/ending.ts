import type { ReplayScene } from '../types';

export const ENDING_SECONDS = 4.5;

const HOLD_SECONDS = 3;
const ENDING_RGB = { victory: [61, 177, 126], defeat: [194, 65, 66], neutral: [116, 139, 149] } as const;

export interface EndingModel {
  title: string;
  rgb: readonly [number, number, number];
  /** Opacity of the black cover: 0 for the hold, then a one second fade. */
  blackAlpha: number;
}

/** What the closing screen shows `elapsed` seconds after the battle end. `null` before the end. */
export function endingModel(replay: Pick<ReplayScene['replay'], 'outcome' | 'complete'>, elapsed: number): EndingModel | null {
  if (elapsed < 0) return null;
  const { outcome } = replay;
  const title = outcome === 'victory' ? 'Victory' : outcome === 'defeat' ? 'Defeat'
    : outcome === 'draw' ? 'Draw' : replay.complete === false ? 'Recording ended' : 'Replay ended';
  const rgb = outcome === 'victory' ? ENDING_RGB.victory : outcome === 'defeat' ? ENDING_RGB.defeat : ENDING_RGB.neutral;
  return { title, rgb, blackAlpha: Math.max(0, Math.min(1, elapsed - HOLD_SECONDS)) };
}

/** Range inputs quantize to 50ms; snap the last step to the exact battle end. */
export function seekPlayback(next: number, duration: number): number {
  const clamped = Math.max(0, Math.min(duration, next));
  return duration - clamped < 0.05 ? duration : clamped;
}

/** Battle time is accelerated; the closing sequence always uses screen seconds. */
export function advancePlayback(time: number, delta: number, duration: number, speed: number): number {
  if (time >= duration) return Math.min(duration + ENDING_SECONDS, time + delta);
  const battleRemaining = (duration - time) / speed;
  return delta < battleRemaining ? time + delta * speed
    : Math.min(duration + ENDING_SECONDS, duration + delta - battleRemaining);
}

export function videoFrameCount(duration: number, speed: number, fps: number): number {
  return Math.max(1, Math.ceil(duration / speed * fps)) + Math.ceil(ENDING_SECONDS * fps);
}

export function videoFrameTime(frame: number, duration: number, speed: number, fps: number): number {
  const battleFrames = Math.max(1, Math.ceil(duration / speed * fps));
  return frame < battleFrames ? frame / fps * speed : duration + (frame - battleFrames) / fps;
}
