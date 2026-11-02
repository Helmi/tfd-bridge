import { describe, expect, it } from 'vitest';
import { renderToStaticMarkup } from 'react-dom/server';
import { ExportHistoryDialog } from './ExportHistoryDialog';
import { buildExportEntry, type ExportHistoryEntry } from '../video/exportHistory';

const entries = (count: number): ExportHistoryEntry[] => Array.from({length: count}, (_, i) => buildExportEntry(
  {at: new Date(2026, 10, 2, 10, 25 - i).toISOString(), ship: `Ship ${count - i}`, map: 'Land of Fire', battleSeconds: 760,
    settings: {resolution: '1080', fps: 30, codec: 'av1', encoder: 'gpu', bitrate: 8_000_000, speed: 25}},
  {downloadName: 'x.mp4', elapsedMs: 8600, frames: 1263, bytes: 52_000_000, encode: {codec: 'av1', hardware: true, renderMsPerFrame: 5, waitMsPerFrame: 1, fps: 139.6}}));
const render = (list: ExportHistoryEntry[]) => renderToStaticMarkup(<ExportHistoryDialog entries={list} onClose={() => {}}/>);

describe('ExportHistoryDialog', () => {
  it('has a title with the count and a Close button', () => {
    const markup = render(entries(3));
    expect(markup).toContain('aria-labelledby="export-history-title"');
    expect(markup).toContain('Recent exports (3)');
    expect(markup).toContain('>Close</button>');
  });
  it('lists every entry in the given (newest first) order with date and bitrate', () => {
    const markup = render(entries(3));
    expect(markup.match(/<li>/g)).toHaveLength(3);
    expect(markup.indexOf('Ship 3')).toBeLessThan(markup.indexOf('Ship 2'));
    expect(markup.indexOf('Ship 2')).toBeLessThan(markup.indexOf('Ship 1'));
    expect(markup).toContain('2 Nov, 10:25');
    expect(markup).toContain('2 Nov, 10:24');
    expect(markup).toContain('<small>1080p30 · AV1 GPU · 8 Mbps · 25× · 12:40 battle → 0:43 video · done in 9 s (140 fps) · 50 MB</small>');
  });
  it('renders 50 entries in a scrollable list', () => {
    const markup = render(entries(50));
    expect(markup.match(/<li>/g)).toHaveLength(50);
    expect(markup).toContain('class="video-history export-history-list"');
  });
});
