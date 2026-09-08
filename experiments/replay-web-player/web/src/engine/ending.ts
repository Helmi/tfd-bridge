export const ENDING_SECONDS = 4.5;

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
