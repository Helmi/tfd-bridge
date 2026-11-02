import { CODEC_LABELS, type EncoderChoice, type VideoCodec } from './codecs';
import type { SavedRender } from './encodeMp4';
import { defaultStorage, formatFileSize, RESOLUTION_LABELS, RESOLUTION_OPTIONS, type LocalVideoSettings } from './renderSettings';

export const EXPORT_HISTORY_KEY = 'tfd.video.history';
/** Entries kept in storage; the export dialog shows only the newest few. */
export const EXPORT_HISTORY_MAX = 5;

export interface ExportHistoryEntry {
  /** ISO time of the export. */
  at: string;
  ship: string;
  map: string;
  battleSeconds: number;
  resolution: LocalVideoSettings['resolution'];
  fps: number;
  codec: VideoCodec;
  encoder?: EncoderChoice;
  hardware?: boolean;
  speed: number;
  videoSeconds: number;
  renderMs: number;
  renderFps?: number;
  bytes: number;
}

export interface ExportInput {
  at: string;
  ship: string;
  map: string;
  battleSeconds: number;
  settings: LocalVideoSettings;
}

type HistoryStorage = Pick<Storage, 'getItem' | 'setItem'>;

export function buildExportEntry(input: ExportInput, saved: SavedRender): ExportHistoryEntry {
  const {settings} = input;
  const measured = saved.elapsedMs > 0 ? saved.frames / (saved.elapsedMs / 1000) : undefined;
  const reported = saved.encode?.fps;
  const renderFps = reported !== undefined && reported > 0 ? reported : measured;
  const entry: ExportHistoryEntry = {
    at: input.at, ship: input.ship, map: input.map, battleSeconds: input.battleSeconds,
    resolution: settings.resolution, fps: settings.fps, codec: settings.codec ?? 'h264',
    speed: settings.speed, videoSeconds: saved.frames / settings.fps, renderMs: saved.elapsedMs, bytes: saved.bytes,
  };
  if (settings.encoder) entry.encoder = settings.encoder;
  if (saved.encode) entry.hardware = saved.encode.hardware;
  if (renderFps !== undefined) entry.renderFps = renderFps;
  return entry;
}

const clock = (seconds: number) => {
  const whole = Math.ceil(seconds);
  return `${Math.floor(whole / 60)}:${(whole % 60).toString().padStart(2, '0')}`;
};

function renderTime(ms: number): string {
  const seconds = Math.max(1, Math.round(ms / 1000));
  return seconds < 120 ? `${seconds} s` : `${Math.floor(seconds / 60)}:${(seconds % 60).toString().padStart(2, '0')}`;
}

/** One dialog line: a bold main part and a small grey rest. */
export function formatExportLine(entry: ExportHistoryEntry): {main: string; rest: string} {
  const gpu = entry.hardware ?? (entry.encoder ? entry.encoder === 'gpu' : undefined);
  const codec = gpu === undefined ? CODEC_LABELS[entry.codec] : `${CODEC_LABELS[entry.codec]} ${gpu ? 'GPU' : 'CPU'}`;
  const speed = entry.renderFps === undefined ? '' : ` (${Math.round(entry.renderFps)} fps)`;
  return {
    main: `${entry.ship} · ${entry.map}`,
    rest: [
      `${RESOLUTION_LABELS[entry.resolution][0]}${entry.fps}`,
      codec,
      `${entry.speed}×`,
      `${clock(entry.battleSeconds)} battle → ${clock(entry.videoSeconds)} video`,
      `done in ${renderTime(entry.renderMs)}${speed}`,
      formatFileSize(entry.bytes / 1_000_000),
    ].join(' · '),
  };
}

const isNumber = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value);
const isOptional = (value: unknown, check: (v: unknown) => boolean) => value === undefined || check(value);

function isEntry(value: unknown): value is ExportHistoryEntry {
  if (!value || typeof value !== 'object') return false;
  const e = value as Record<string, unknown>;
  return typeof e.at === 'string' && typeof e.ship === 'string' && typeof e.map === 'string'
    && isNumber(e.battleSeconds) && RESOLUTION_OPTIONS.includes(e.resolution as never) && isNumber(e.fps)
    && (e.codec === 'h264' || e.codec === 'av1' || e.codec === 'hevc')
    && isOptional(e.encoder, v => v === 'gpu' || v === 'cpu') && isOptional(e.hardware, v => typeof v === 'boolean')
    && isNumber(e.speed) && isNumber(e.videoSeconds) && isNumber(e.renderMs) && isOptional(e.renderFps, isNumber) && isNumber(e.bytes);
}

/** Newest first. Unreadable data gives an empty list; invalid entries are dropped one by one. */
export function loadExportHistory(storage: HistoryStorage | undefined = defaultStorage()): ExportHistoryEntry[] {
  try {
    const parsed: unknown = JSON.parse(storage?.getItem(EXPORT_HISTORY_KEY) ?? 'null');
    return Array.isArray(parsed) ? parsed.filter(isEntry).slice(0, EXPORT_HISTORY_MAX) : [];
  } catch { return []; }
}

/** Puts the entry first and keeps the newest five. Returns false when storage is unavailable. */
export function addExportHistory(entry: ExportHistoryEntry, storage: HistoryStorage | undefined = defaultStorage()): boolean {
  try {
    const next = [entry, ...loadExportHistory(storage)].slice(0, EXPORT_HISTORY_MAX);
    storage!.setItem(EXPORT_HISTORY_KEY, JSON.stringify(next));
    return true;
  } catch { return false; }
}

/** Never throws: a failed history write must not break an export. */
export function recordExport(input: ExportInput, saved: SavedRender, storage?: HistoryStorage): void {
  try { addExportHistory(buildExportEntry(input, saved), storage ?? defaultStorage()); } catch { /* history is optional */ }
}

/**
 * The bridge result when there is one, else the in-page export (older bridge).
 * `record` runs once, only after the export resolved; a throw or abort records nothing.
 */
export async function exportAndRecord(
  renderers: {bridge?: () => Promise<SavedRender | undefined>; local: () => Promise<SavedRender>},
  record: (saved: SavedRender) => void,
): Promise<SavedRender> {
  const saved = (await renderers.bridge?.()) ?? await renderers.local();
  record(saved);
  return saved;
}
