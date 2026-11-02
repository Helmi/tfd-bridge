import { CODEC_LABELS, type EncoderChoice, type VideoCodec } from './codecs';
import type { SavedRender } from './encodeMp4';
import { defaultStorage, formatFileSize, RESOLUTION_LABELS, RESOLUTION_OPTIONS, type LocalVideoSettings } from './renderSettings';

export const EXPORT_HISTORY_KEY = 'tfd.video.history';
/** Entries kept in storage; the history view shows all of them. */
export const EXPORT_HISTORY_MAX = 50;

export interface ExportHistoryEntry {
  /** ISO time of the export. */
  at: string;
  ship: string;
  map: string;
  battleSeconds: number;
  resolution: LocalVideoSettings['resolution'];
  fps: number;
  /** Target bitrate in bps; absent in old entries. */
  bitrate?: number;
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
    resolution: settings.resolution, fps: settings.fps, bitrate: settings.bitrate, codec: settings.codec ?? 'h264',
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

export interface ExportRow {
  when: string; ship: string; map: string; format: string; encoder: string;
  bitrate: string; speed: string; video: string; render: string; size: string;
}

/** The cell texts of one history table row. Unknown values show "—". */
export function formatExportRow(entry: ExportHistoryEntry): ExportRow {
  const gpu = entry.hardware ?? (entry.encoder ? entry.encoder === 'gpu' : undefined);
  return {
    when: formatExportTime(entry.at).replace(',', ''),
    ship: entry.ship,
    map: entry.map,
    format: `${RESOLUTION_LABELS[entry.resolution][0]}${entry.fps} · ${CODEC_LABELS[entry.codec]}`,
    encoder: gpu === undefined ? '—' : gpu ? 'GPU' : 'CPU',
    bitrate: entry.bitrate === undefined ? '—' : `${Math.round(entry.bitrate / 100_000) / 10} Mbps`,
    speed: `${entry.speed}×`,
    video: clock(entry.videoSeconds),
    render: renderTime(entry.renderMs),
    size: formatFileSize(entry.bytes),
  };
}

/** Local time, e.g. "2 Nov, 10:25". */
export function formatExportTime(at: string): string {
  return new Date(at).toLocaleString('en-GB', {day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit'});
}

const SIZE_FACTOR_DEFAULTS: Record<VideoCodec, Record<EncoderChoice, number>> = {
  h264: {gpu: 0.8, cpu: 0.9}, av1: {gpu: 1.0, cpu: 0.5}, hevc: {gpu: 0.8, cpu: 0.8},
};
const SIZE_FACTOR_MIN_ENTRIES = 2;

/** Median; the mean of the two middle values for an even count. */
export function median(values: number[]): number {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = sorted.length >> 1;
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

/**
 * How much of the target bitrate the encoder really reaches: the median of real bytes over
 * (video seconds × bitrate / 8) for past exports with the same codec and encoder. Entries without
 * a bitrate or encoder are ignored. Fewer than two matches use a per-codec default.
 */
export function sizeFactor(codec: VideoCodec, encoder: EncoderChoice, history: ExportHistoryEntry[]): number {
  const ratios: number[] = [];
  for (const e of history) {
    const used = e.hardware === true ? 'gpu' : e.hardware === false ? 'cpu' : e.encoder;
    if (e.codec !== codec || used !== encoder || e.bitrate === undefined) continue;
    const ratio = e.bytes / (e.videoSeconds * e.bitrate / 8);
    if (Number.isFinite(ratio) && ratio > 0) ratios.push(ratio);
  }
  const factor = ratios.length >= SIZE_FACTOR_MIN_ENTRIES ? median(ratios) : SIZE_FACTOR_DEFAULTS[codec][encoder];
  return Math.min(1.1, Math.max(0.3, factor));
}

const isNumber = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value);
const isOptional = (value: unknown, check: (v: unknown) => boolean) => value === undefined || check(value);

function isEntry(value: unknown): value is ExportHistoryEntry {
  if (!value || typeof value !== 'object') return false;
  const e = value as Record<string, unknown>;
  return typeof e.at === 'string' && typeof e.ship === 'string' && typeof e.map === 'string'
    && isNumber(e.battleSeconds) && RESOLUTION_OPTIONS.includes(e.resolution as never) && isNumber(e.fps)
    && isOptional(e.bitrate, v => isNumber(v) && v > 0)
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

/** Puts the entry first and keeps the newest 50. Returns false when storage is unavailable. */
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
