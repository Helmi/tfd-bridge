import { describe, expect, it } from 'vitest';
import { renderToStaticMarkup } from 'react-dom/server';
import { VideoSettingsDialog } from './VideoSettingsDialog';
import { BITRATE_OPTIONS_MBPS, battleLengthSizes, estimateLocalVideo, formatFileSize, suggestedBitrates, type LocalVideoSettings } from '../video/renderSettings';

const settings: LocalVideoSettings = {resolution:'2160', fps:30, bitrate:80_000_000, speed:10};
const resolutions = ['1080', '1440', '2160'] as const;

describe('suggested bitrates', () => {
  it('match the table for every resolution and frame rate', () => {
    const table = {'1080':[[8,12],[8,12],[12,16]], '1440':[[16,25],[16,25],[25,40]], '2160':[[40,50],[40,50],[60,80]]};
    for (const r of resolutions) [24,30,60].forEach((fps, i) => {
      const s = suggestedBitrates(r, fps as 24|30|60);
      expect([s.good, s.artifactFree]).toEqual(table[r][i]);
    });
  });
  it('are selectable bitrates and Good is below Artifact-free', () => {
    for (const r of resolutions) for (const fps of [24,30,60] as const) {
      const s = suggestedBitrates(r, fps);
      expect(BITRATE_OPTIONS_MBPS).toContain(s.good);
      expect(BITRATE_OPTIONS_MBPS).toContain(s.artifactFree);
      expect(s.good).toBeLessThan(s.artifactFree);
    }
  });
});

describe('battle length sizes', () => {
  it('use estimateLocalVideo for 10, 15 and 20 minutes', () => {
    expect(battleLengthSizes(settings).map(s => [s.minutes, s.megabytes])).toEqual([10,15,20].map(m => [m, estimateLocalVideo(m * 60, settings).megabytes]));
  });
  it('formats MB below 1000 and GB above', () => {
    expect(formatFileSize(999.4)).toBe('999 MB');
    expect(formatFileSize(1000)).toBe('1.0 GB');
    expect(formatFileSize(1234)).toBe('1.2 GB');
  });
});

describe('VideoSettingsDialog', () => {
  const render = (batchCount?: number) => renderToStaticMarkup(<VideoSettingsDialog batchCount={batchCount} duration={600} initial={settings} native onClose={() => {}} onRender={() => {}}/>);
  it.each([undefined, 3])('shows 4K, 80 Mbps, suggestions and sizes (batchCount %s)', batchCount => {
    const markup = render(batchCount);
    expect(markup).toContain('3840 × 2160');
    expect(markup).toContain('80 Mbps');
    expect(markup).toContain('Good 40 Mbps');
    expect(markup).toContain('Artifact-free 50 Mbps');
    for (const s of battleLengthSizes(settings)) expect(markup).toContain(`${s.minutes} min · ${s.label}`);
  });
});
