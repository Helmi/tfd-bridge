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

describe('codec-aware suggested bitrates', () => {
  it('follow the AV1/HEVC table and stay selectable', () => {
    const table = {'1080':[[4,8],[4,8],[8,12]], '1440':[[8,16],[8,16],[16,25]], '2160':[[25,30],[25,30],[40,50]]};
    for (const codec of ['av1', 'hevc'] as const) for (const r of resolutions) [24,30,60].forEach((fps, i) => {
      const s = suggestedBitrates(r, fps as 24|30|60, codec);
      expect([s.good, s.artifactFree]).toEqual(table[r][i]);
      expect(BITRATE_OPTIONS_MBPS).toContain(s.good);
      expect(BITRATE_OPTIONS_MBPS).toContain(s.artifactFree);
      expect(s.good).toBeLessThan(s.artifactFree);
    });
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

describe('VideoSettingsDialog recent exports', () => {
  it('shows codec, GPU/CPU and frames per second', () => {
    const encode = {codec: 'av1' as const, hardware: true, renderMsPerFrame: 10, waitMsPerFrame: 2, fps: 41.3};
    const markup = renderToStaticMarkup(<VideoSettingsDialog comparisons={[{settings, elapsedMs: 5000, bytes: 1_048_576, encode}]} duration={600} initial={settings} native onClose={() => {}} onRender={() => {}}/>);
    expect(markup).toContain('AV1 · GPU · 41 fps');
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
    expect(markup).toContain('Plays everywhere: Discord, phones, editors. Largest files.');
    expect(markup).toContain('<option value="av1"');
    expect(markup).not.toContain('<option value="hevc"');
    for (const s of battleLengthSizes(settings)) expect(markup).toContain(`<span>${s.minutes} min · ${s.label}</span>`);
    expect(markup.match(/<span>\d+ min · /g)).toHaveLength(3);
  });
});
