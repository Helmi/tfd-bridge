// Offline, deterministic scene → mp4. Steps an explicit frame clock (not
// wall-clock rAF): rasterize the shared replay frame at explicit timestamps,
// encode H.264 with WebCodecs and mux into MP4. Raw frames are bounded;
// compressed output remains in memory until saved.
import { ArrayBufferTarget, Muxer } from 'mp4-muxer';
import type { ReplayScene } from '../types';
import { SharedFrameRenderer, VIRTUAL_HEIGHT } from './sharedFrameRenderer';
import { budgetBitrate, retryBitrate } from './encodingPolicy';
import { videoFrameCount, videoFrameTime } from '../engine/ending';

export interface RenderProgress {
  frame: number;
  total: number;
  attempt: number;
}

/** Vertical resolution → renderer scale (layout is authored at 1080p virtual). */
const RESOLUTION_SCALE: Record<'1080' | '1440' | '2160', number> = {
  '1080': 1080 / VIRTUAL_HEIGHT,
  '1440': 1440 / VIRTUAL_HEIGHT,
  '2160': 2160 / VIRTUAL_HEIGHT,
};

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

/** Probe for a supported encoder config, preferring hardware, then software. */
async function pickH264Config(
  base: { width: number; height: number; bitrate: number; framerate: number },
): Promise<VideoEncoderConfig | null> {
  const codec = h264CodecString(base.width, base.height, base.framerate);
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

export interface RenderOptions {
  mapSupersampling?: boolean;
  fps?: number;
  bitrate?: number;
  speed?: number;
  resolution?: '1080' | '1440' | '2160';
  onProgress?: (p: RenderProgress) => void;
  signal?: AbortSignal;
  maxBytes?: number;
}

export async function renderSceneToMp4(scene: ReplayScene, opts: RenderOptions = {}): Promise<RenderResult> {
  const started = performance.now();
  if (!Number.isFinite(scene.replay.duration) || scene.replay.duration <= 0) {
    throw new Error('Replay duration must be positive and finite');
  }
  if (opts.bitrate !== undefined && (!Number.isFinite(opts.bitrate) || opts.bitrate <= 0)) {
    throw new Error('Video bitrate must be positive and finite');
  }
  if (opts.maxBytes === undefined) return encodeOnce(scene, opts);
  const speed = opts.speed ?? 10;
  if (!Number.isFinite(speed) || speed <= 0) throw new Error('Playback speed must be positive');
  const fps = opts.fps ?? 30;
  const duration = videoFrameCount(scene.replay.duration, speed, fps) / fps;
  let bitrate = budgetBitrate(opts.maxBytes, duration, opts.bitrate ?? 4_000_000);
  for (let attempt = 0; attempt < 3; attempt++) {
    const result = await encodeOnce(scene, {...opts, resolution:opts.resolution ?? '1080', bitrate,
      onProgress: progress => opts.onProgress?.({...progress, attempt: attempt + 1})});
    if (result.mp4.byteLength <= opts.maxBytes) return {...result, elapsedMs: performance.now() - started};
    bitrate = retryBitrate(bitrate, result.mp4.byteLength, opts.maxBytes);
    if (bitrate < 128_000) break;
  }
  throw new Error('Rendered video exceeds the upload limit; no video was uploaded');
}

async function encodeOnce(
  scene: ReplayScene,
  opts: RenderOptions = {},
): Promise<RenderResult> {
  if (!webCodecsAvailable()) {
    throw new Error('WebCodecs (VideoEncoder) is not available in this runtime.');
  }
  const fps = opts.fps ?? 30;
  // Time-compress the battle: default 10x → a ~15min battle becomes a ~90s clip.
  const speed = opts.speed ?? 10;
  if (!Number.isFinite(fps) || fps < 1 || fps > 60 || !Number.isFinite(speed) || speed <= 0) {
    throw new Error('Frame rate must be 1–60 and playback speed must be positive');
  }
  opts.signal?.throwIfAborted();
  const resolution = opts.resolution ?? '1080';
  const scale = RESOLUTION_SCALE[resolution];
  const bitrate = opts.bitrate ?? (resolution === '2160' ? 12_000_000 : resolution === '1440' ? 8_000_000 : 4_000_000);
  const total = videoFrameCount(scene.replay.duration, speed, fps);
  const started = performance.now();

  const renderer = await SharedFrameRenderer.create(scene, { scale, mapSupersampling: opts.mapSupersampling ?? false });
  const width = renderer.width;
  const height = renderer.height;
  let encoder: VideoEncoder | undefined;
  try {
    opts.signal?.throwIfAborted();
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
      throw new Error(`No supported H.264 encoder config for ${width}x${height}. Try a lower resolution.`);
    }

    let encoderError: Error | null = null;
    encoder = new VideoEncoder({
      output: (chunk, meta) => muxer.addVideoChunk(chunk, meta),
      error: (e) => { encoderError = e instanceof Error ? e : new Error(String(e)); console.error('VideoEncoder error', e); },
    });
    encoder.configure(config);

    const frameDurUs = 1_000_000 / fps;
    for (let i = 0; i < total; i++) {
      opts.signal?.throwIfAborted();
      if (encoderError) throw encoderError; // surface the real fault, not a stale close()
      // Battle time advances `speed`x faster than video time.
      await renderer.renderFrame(videoFrameTime(i, scene.replay.duration, speed, fps));
      const frame = new VideoFrame(renderer.canvas, {
        timestamp: Math.round(i * frameDurUs),
        duration: Math.round(frameDurUs),
      });
      try { encoder.encode(frame, { keyFrame: i % (fps * 2) === 0 }); }
      finally { frame.close(); }
      // Backpressure: keep the encode queue bounded so memory stays flat and the
      // UI thread gets to breathe on long battles.
      while (encoder.encodeQueueSize > 8) {
        opts.signal?.throwIfAborted();
        if (encoderError) throw encoderError;
        await new Promise<void>((resolve) => setTimeout(resolve, 0));
      }
      if (opts.onProgress && (i % 5 === 0 || i === total - 1)) {
        opts.onProgress({ frame: i + 1, total, attempt: 1 });
      }
    }
    await encoder.flush();
    if (encoderError) throw encoderError;
    muxer.finalize();
    const { buffer } = muxer.target as ArrayBufferTarget;
    return {
      mp4: new Uint8Array(buffer),
      frames: total,
      width,
      height,
      fps,
      elapsedMs: performance.now() - started,
    };
  } finally {
    // Setup/configuration can fail too; release the map and encoder in all cases.
    if (encoder && encoder.state !== 'closed') { try { encoder.close(); } catch { /* already closed */ } }
    renderer.destroy();
  }
}

export interface SavedRender {
  jobId?: string;
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
  signal?: AbortSignal,
  videoOptions?: Pick<RenderOptions, 'resolution' | 'fps' | 'bitrate' | 'speed' | 'mapSupersampling'>,
): Promise<SavedRender> {
  const result = await renderSceneToMp4(scene, { ...videoOptions, onProgress, signal });
  signal?.throwIfAborted();
  const downloadName = `${filenameBase}.mp4`;

  let path: string | undefined;
  if (import.meta.env.VITE_BRIDGE === '1') {
    const resp = await fetch(`/player/api/render?name=${encodeURIComponent(downloadName)}`, {
      method: 'POST',
      headers: { 'Content-Type': 'video/mp4' },
      body: result.mp4 as BodyInit,
    });
    if (!resp.ok) throw new Error(`Saving video failed: HTTP ${resp.status}`);
    path = ((await resp.json()) as { path?: string }).path;
    if (!path) throw new Error('Bridge did not return the saved video path');
  }

  if (!path) {
    const blob = new Blob([result.mp4 as BlobPart], { type: 'video/mp4' });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = downloadName;
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
    setTimeout(() => URL.revokeObjectURL(url), 60_000);
  }

  return { path, downloadName, elapsedMs: result.elapsedMs, frames: result.frames, bytes: result.mp4.byteLength };
}
