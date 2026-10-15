// Offline, deterministic scene → mp4. Steps an explicit frame clock (not
// wall-clock rAF): rasterize the shared replay frame at explicit timestamps,
// encode (H.264, AV1 or HEVC) with WebCodecs and mux into MP4. Raw frames are bounded;
// compressed output remains in memory until saved.
import { ArrayBufferTarget, Muxer, StreamTarget } from 'mp4-muxer';
import type { ReplayScene } from '../types';
import { SharedFrameRenderer, VIRTUAL_HEIGHT } from './sharedFrameRenderer';
import { budgetBitrate, retryBitrate } from './encodingPolicy';
import { videoFrameCount, videoFrameTime } from '../engine/ending';
import { CODEC_LABELS, EncoderUnavailableError, MUXER_CODECS, h264CodecString, pickEncoderConfig, type EncodeReport, type EncoderChoice, type VideoCodec } from './codecs';

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

export { h264CodecString };

/** Receives muxer output in emit order; `position` is the byte offset in the file. */
export interface VideoSink {
  write(data: Uint8Array, position: number): void;
  /** Chunks accepted but not yet stored; the frame loop waits while this is high. */
  pending(): number;
  /** Resolves when the next pending chunk has been stored (no timers). */
  nextDrain(): Promise<void>;
}

/** Muxer chunk size and the number of queued chunks that stalls the frame loop. */
export const STREAM_CHUNK_BYTES = 16 * 1024 * 1024;
export const STREAM_MAX_PENDING = 4;

export interface RenderResult {
  /** Empty when the output was streamed to a sink; see `bytes`. */
  mp4: Uint8Array;
  /** Total file size in bytes (also set for streamed output). */
  bytes: number;
  frames: number;
  width: number;
  height: number;
  fps: number;
  elapsedMs: number;
  /** Encoder that ran and how fast. */
  encode: EncodeReport;
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
  /** Defaults to H.264; budgeted renders always use H.264. */
  codec?: VideoCodec;
  /** Only this encoder may run (`gpu` = hardware, `cpu` = software); absent tries hardware, then software. */
  encoder?: EncoderChoice;
  onProgress?: (p: RenderProgress) => void;
  signal?: AbortSignal;
  maxBytes?: number;
  /** Stream the file to a sink instead of holding it in memory (no byte budget only). */
  sink?: VideoSink;
}

export async function renderSceneToMp4(scene: ReplayScene, opts: RenderOptions = {}): Promise<RenderResult> {
  const started = performance.now();
  if (!Number.isFinite(scene.replay.duration) || scene.replay.duration <= 0) {
    throw new Error('Replay duration must be positive and finite');
  }
  if (opts.bitrate !== undefined && (!Number.isFinite(opts.bitrate) || opts.bitrate <= 0)) {
    throw new Error('Video bitrate must be positive and finite');
  }
  if (opts.sink && opts.maxBytes !== undefined) throw new Error('Streaming output cannot be combined with a byte budget');
  if (opts.maxBytes === undefined) return encodeOnce(scene, opts);
  const speed = opts.speed ?? 10;
  if (!Number.isFinite(speed) || speed <= 0) throw new Error('Playback speed must be positive');
  const fps = opts.fps ?? 30;
  const duration = videoFrameCount(scene.replay.duration, speed, fps) / fps;
  let bitrate = budgetBitrate(opts.maxBytes, duration, opts.bitrate ?? 4_000_000);
  for (let attempt = 0; attempt < 3; attempt++) {
    const result = await encodeOnce(scene, {...opts, codec:'h264', resolution:opts.resolution ?? '1080', bitrate,
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
  const codec = opts.codec ?? 'h264';
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
    const sink = opts.sink;
    let streamedBytes = 0;
    const muxer = new Muxer({
      target: sink
        ? new StreamTarget({
            onData: (data, position) => {
              streamedBytes = Math.max(streamedBytes, position + data.byteLength);
              sink.write(data, position);
            },
            chunked: true,
            chunkSize: STREAM_CHUNK_BYTES,
          })
        : new ArrayBufferTarget(),
      video: { codec: MUXER_CODECS[codec], width, height, frameRate: fps },
      fastStart: sink ? false : 'in-memory',
    });

    // The level MUST fit the resolution (1440p/4K exceed H.264 L4.0's 1080p cap),
    // and a hardware encoder can reject a config software would accept — so probe
    // for a supported config, preferring hardware but falling back.
    const picked = await pickEncoderConfig(codec, { width, height, bitrate, framerate: fps }, opts.encoder);
    if (!picked) {
      if (opts.encoder) throw new EncoderUnavailableError();
      throw new Error(`No supported ${CODEC_LABELS[codec]} encoder config for ${width}x${height}. Try a lower resolution.`);
    }

    let encoderError: Error | null = null;
    let wake: (() => void) | undefined; // lets an encoder fault end a dequeue wait
    encoder = new VideoEncoder({
      output: (chunk, meta) => muxer.addVideoChunk(chunk, meta),
      error: (e) => { encoderError = e instanceof Error ? e : new Error(String(e)); console.error('VideoEncoder error', e); wake?.(); },
    });
    encoder.configure(picked.config);

    // Resolves on the encoder's next `dequeue` event, an encoder fault or an abort.
    const nextDequeue = (enc: VideoEncoder) => new Promise<void>((resolve) => {
      const done = () => {
        enc.removeEventListener('dequeue', done);
        opts.signal?.removeEventListener('abort', done);
        wake = undefined;
        resolve();
      };
      wake = done;
      enc.addEventListener('dequeue', done);
      opts.signal?.addEventListener('abort', done);
    });
    let renderMs = 0;
    let waitMs = 0;
    const loopStarted = performance.now();
    const frameDurUs = 1_000_000 / fps;
    for (let i = 0; i < total; i++) {
      opts.signal?.throwIfAborted();
      if (encoderError) throw encoderError; // surface the real fault, not a stale close()
      // Battle time advances `speed`x faster than video time.
      const renderStarted = performance.now();
      await renderer.renderFrame(videoFrameTime(i, scene.replay.duration, speed, fps));
      renderMs += performance.now() - renderStarted;
      const frame = new VideoFrame(renderer.canvas, {
        timestamp: Math.round(i * frameDurUs),
        duration: Math.round(frameDurUs),
      });
      try { encoder.encode(frame, { keyFrame: i % (fps * 2) === 0 }); }
      finally { frame.close(); }
      // Backpressure: keep the encode queue bounded so memory stays flat and the
      // UI thread gets to breathe on long battles.
      const waitStarted = performance.now();
      while (encoder.encodeQueueSize > 8) {
        opts.signal?.throwIfAborted();
        if (encoderError) throw encoderError;
        await nextDequeue(encoder);
      }
      // Streaming: stall while too many chunks wait for upload (bounds memory).
      while (sink && sink.pending() > STREAM_MAX_PENDING) {
        opts.signal?.throwIfAborted();
        if (encoderError) throw encoderError;
        await sink.nextDrain();
      }
      waitMs += performance.now() - waitStarted;
      if (opts.onProgress && (i % 5 === 0 || i === total - 1)) {
        opts.onProgress({ frame: i + 1, total, attempt: 1 });
      }
    }
    await encoder.flush();
    if (encoderError) throw encoderError;
    const loopSeconds = (performance.now() - loopStarted) / 1000;
    muxer.finalize();
    const mp4 = sink ? new Uint8Array(0) : new Uint8Array((muxer.target as ArrayBufferTarget).buffer);
    return {
      mp4,
      bytes: sink ? streamedBytes : mp4.byteLength,
      frames: total,
      width,
      height,
      fps,
      elapsedMs: performance.now() - started,
      encode: {
        codec, hardware: picked.hardware,
        renderMsPerFrame: renderMs / total, waitMsPerFrame: waitMs / total,
        fps: loopSeconds > 0 ? total / loopSeconds : 0,
      },
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
  /** Encoder that ran and how fast; absent for older bridges. */
  encode?: EncodeReport;
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
  videoOptions?: Pick<RenderOptions, 'resolution' | 'fps' | 'bitrate' | 'speed' | 'mapSupersampling' | 'codec' | 'encoder'>,
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

  return { path, downloadName, elapsedMs: result.elapsedMs, frames: result.frames, bytes: result.mp4.byteLength, encode: result.encode };
}
