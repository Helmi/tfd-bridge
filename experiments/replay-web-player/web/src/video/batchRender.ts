import type { RenderProgress } from './encodeMp4';
import type { LocalVideoSettings } from './renderSettings';
import { renderInBridge } from './bridgeRender';

export interface BatchReplay {
  id: string;
  filename: string;
  shipName: string;
  mapName: string;
  playedAt: string;
}

export type BatchResult =
  | { replay: BatchReplay; status: 'saved' }
  | { replay: BatchReplay; status: 'failed'; error: string }
  | { replay: BatchReplay; status: 'skipped' };

export const OLD_BRIDGE_REASON = 'This TFD Bridge version cannot render videos in the background. Please update it.';

export function sortOldestFirst<T extends { playedAt: string }>(replays: T[]): T[] {
  return [...replays].sort((a, b) => Date.parse(a.playedAt) - Date.parse(b.playedAt));
}

/**
 * Renders the replays strictly one after another, oldest first (the bridge
 * allows one render at a time). A failed battle never stops the batch. Aborting
 * the signal cancels the current render and skips everything not yet saved.
 */
export async function runBatch(options: {
  replays: BatchReplay[];
  settings: LocalVideoSettings;
  signal: AbortSignal;
  render?: typeof renderInBridge;
  onStart?: (index: number, replay: BatchReplay) => void;
  onProgress?: (progress: RenderProgress) => void;
}): Promise<BatchResult[]> {
  const { settings, signal, render = renderInBridge, onStart, onProgress } = options;
  const results: BatchResult[] = [];
  const ordered = sortOldestFirst(options.replays);
  for (const [index, replay] of ordered.entries()) {
    if (signal.aborted) { results.push({ replay, status: 'skipped' }); continue; }
    onStart?.(index, replay);
    try {
      // The bridge resolves replays by their replays-folder-relative name (the summary id), not the bare filename.
      const saved = await render(replay.id, undefined, (progress) => onProgress?.(progress), signal, settings);
      results.push(saved ? { replay, status: 'saved' } : { replay, status: 'failed', error: OLD_BRIDGE_REASON });
    } catch (reason) {
      results.push(signal.aborted
        ? { replay, status: 'skipped' }
        : { replay, status: 'failed', error: reason instanceof Error ? reason.message : String(reason) });
    }
  }
  return results;
}
