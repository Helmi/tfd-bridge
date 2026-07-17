// Offline, deterministic scene → mp4. Steps an explicit frame clock (not
// wall-clock rAF): render frame i at t=i/fps, hand the Pixi WebGL canvas straight
// to a WebCodecs VideoEncoder (hardware H.264), mux with mp4-muxer. Frames stream
// through the encoder so memory stays flat regardless of battle length.
import { ArrayBufferTarget, Muxer } from 'mp4-muxer';
import type { ReplayScene } from '../types';
import { BroadcastRenderer, VIRTUAL_HEIGHT } from './broadcastRenderer';

export interface RenderProgress {
  frame: number;
  total: number;
}

/** Vertical resolution → renderer scale (layout is authored at 1080p virtual). */
const RESOLUTION_SCALE: Record<'1080' | '1440' | '2160', number> = {
  '1080': 1080 / VIRTUAL_HEIGHT,
  '1440': 1440 / VIRTUAL_HEIGHT,
  '2160': 2160 / VIRTUAL_HEIGHT,
};

/** H.264 High-profile codec string with a level that fits the frame size. */
function h264CodecString(width: number, height: number): string {
  const mb = Math.ceil(width / 16) * Math.ceil(height / 16); // macroblocks per frame
  // [max frame MBs, level hex] — 4.0(≤1080p) · 4.2 · 5.0(≤1440p) · 5.1(≤4K) · 5.2
  const table: [number, number][] = [[8192, 0x28], [8704, 0x2a], [22080, 0x32], [36864, 0x33], [Infinity, 0x34]];
  let level = 0x34;
  for (const [cap, lv] of table) { if (mb <= cap) { level = lv; break; } }
  if ((width > 2048 || height > 1088) && level < 0x32) level = 0x32; // >1080p needs ≥L5.0
  return `avc1.6400${level.toString(16).padStart(2, '0')}`;
}

/** Probe for a supported encoder config, preferring hardware, then software. */
async function pickH264Config(
  base: { width: number; height: number; bitrate: number; framerate: number },
): Promise<VideoEncoderConfig | null> {
  const codec = h264CodecString(base.width, base.height);
  const accels: HardwareAcceleration[] = ['prefer-hardware', 'no-preference', 'prefer-software'];
  for (const acceleration of accels) {
    const cfg: VideoEncoderConfig = { codec, ...base, hardwareAcceleration: acceleration, latencyMode: 'quality' };
    try {
      const support = await VideoEncoder.isConfigSupported(cfg);
      if (support.supported) return support.config ?? cfg;
    } catch { /* try next acceleration */ }
  }
  return null;
}

export interface RenderResult {
  mp4: Uint8Array;
  frames: number;
  width: number;
  height: number;
  fps: number;
  elapsedMs: number;
}

export function webCodecsAvailable(): boolean {
  return typeof VideoEncoder !== 'undefined' && typeof VideoFrame !== 'undefined';
}

export async function renderSceneToMp4(
  scene: ReplayScene,
  opts: {
    fps?: number;
    bitrate?: number;
    speed?: number;
    resolution?: '1080' | '1440' | '2160';
    onProgress?: (p: RenderProgress) => void;
  } = {},
): Promise<RenderResult> {
  if (!webCodecsAvailable()) {
    throw new Error('WebCodecs (VideoEncoder) is not available in this runtime.');
  }
  const fps = opts.fps ?? 30;
  // Time-compress the battle: default 10x → a ~15min battle becomes a ~90s clip.
  const speed = opts.speed ?? 10;
  const resolution = opts.resolution ?? '1440';
  const scale = RESOLUTION_SCALE[resolution];
  const bitrate = opts.bitrate ?? (resolution === '2160' ? 24_000_000 : resolution === '1440' ? 12_000_000 : 8_000_000);
  const total = Math.max(1, Math.ceil((scene.replay.duration / speed) * fps));
  const started = performance.now();

  const renderer = await BroadcastRenderer.create(scene, { scale });
  const width = renderer.width;
  const height = renderer.height;
  const muxer = new Muxer({
    target: new ArrayBufferTarget(),
    video: { codec: 'avc', width, height, frameRate: fps },
    fastStart: 'in-memory',
  });

  // The H.264 level MUST fit the resolution (1440p/4K exceed L4.0's 1080p cap),
  // and a hardware encoder can reject a config software would accept — so probe
  // for a supported config, preferring hardware but falling back.
  const config = await pickH264Config({ width, height, bitrate, framerate: fps });
  if (!config) {
    renderer.destroy();
    throw new Error(`No supported H.264 encoder config for ${width}x${height}. Try a lower resolution.`);
  }

  let encoderError: Error | null = null;
  const encoder = new VideoEncoder({
    output: (chunk, meta) => muxer.addVideoChunk(chunk, meta),
    error: (e) => { encoderError = e instanceof Error ? e : new Error(String(e)); console.error('VideoEncoder error', e); },
  });
  encoder.configure(config);

  try {
    const frameDurUs = 1_000_000 / fps;
    for (let i = 0; i < total; i++) {
      if (encoderError) throw encoderError; // surface the real fault, not a stale close()
      // Battle time advances `speed`x faster than video time.
      renderer.renderFrame((i / fps) * speed);
      const frame = new VideoFrame(renderer.canvas, {
        timestamp: Math.round(i * frameDurUs),
        duration: Math.round(frameDurUs),
      });
      encoder.encode(frame, { keyFrame: i % (fps * 2) === 0 });
      frame.close();
      // Backpressure: keep the encode queue bounded so memory stays flat and the
      // UI thread gets to breathe on long battles.
      if (encoder.encodeQueueSize > 8) {
        await new Promise<void>((resolve) => setTimeout(resolve, 0));
      }
      if (opts.onProgress && (i % 5 === 0 || i === total - 1)) {
        opts.onProgress({ frame: i + 1, total });
      }
    }
    await encoder.flush();
    if (encoderError) throw encoderError;
    muxer.finalize();
  } finally {
    // An errored encoder auto-closes; guard so cleanup never throws over the cause.
    if (encoder.state !== 'closed') { try { encoder.close(); } catch { /* already closed */ } }
    renderer.destroy();
  }

  const { buffer } = muxer.target as ArrayBufferTarget;
  return {
    mp4: new Uint8Array(buffer),
    frames: total,
    width,
    height,
    fps,
    elapsedMs: performance.now() - started,
  };
}

export interface SavedRender {
  path?: string;
  downloadName: string;
  elapsedMs: number;
  frames: number;
  bytes: number;
}

/**
 * Render the scene and persist the mp4. Prefers the bridge's loopback save
 * endpoint (writes to a local folder, returns the path); falls back to a browser
 * download when not running inside the bridge (e.g. the vite-dev experiment).
 */
export async function renderAndSave(
  scene: ReplayScene,
  filenameBase: string,
  onProgress?: (p: RenderProgress) => void,
): Promise<SavedRender> {
  const result = await renderSceneToMp4(scene, { onProgress });
  const downloadName = `${filenameBase}.mp4`;

  let path: string | undefined;
  try {
    const resp = await fetch(`/player/api/render?name=${encodeURIComponent(downloadName)}`, {
      method: 'POST',
      headers: { 'Content-Type': 'video/mp4' },
      body: result.mp4 as BodyInit,
    });
    if (resp.ok) {
      path = ((await resp.json()) as { path?: string }).path;
    }
  } catch {
    // Not in the bridge — fall through to a browser download.
  }

  if (!path) {
    const blob = new Blob([result.mp4 as BlobPart], { type: 'video/mp4' });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = downloadName;
    anchor.click();
    URL.revokeObjectURL(url);
  }

  return { path, downloadName, elapsedMs: result.elapsedMs, frames: result.frames, bytes: result.mp4.byteLength };
}
