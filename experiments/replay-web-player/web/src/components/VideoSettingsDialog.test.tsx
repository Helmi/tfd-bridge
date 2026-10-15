import { describe, expect, it, vi } from 'vitest';
import { renderToStaticMarkup } from 'react-dom/server';

const detect = vi.hoisted(() => vi.fn());
vi.mock('../video/codecs', async importOriginal => ({...await importOriginal<typeof import('../video/codecs')>(), detectEncoderSupport: detect}));
import { detectEncoderSupport, type EncoderSupport } from '../video/codecs';
import { VideoSettingsDialog, VideoSettingsFields, settleEncoder } from './VideoSettingsDialog';
import type { LocalVideoSettings } from '../video/renderSettings';

const settings: LocalVideoSettings = {resolution:'1080', fps:30, codec:'av1', encoder:'gpu', bitrate:8_000_000, speed:10};
const all = {gpu: true, cpu: true};
const none = {gpu: false, cpu: false};
const support = (patch: Partial<EncoderSupport> = {}): EncoderSupport => ({h264: all, av1: all, hevc: none, ...patch});

const fields = (opts: {settings?: LocalVideoSettings; support?: EncoderSupport; batchCount?: number} = {}) =>
  renderToStaticMarkup(<VideoSettingsFields batchCount={opts.batchCount} duration={754} settings={opts.settings ?? settings} support={opts.support} onChange={() => {}}/>);
const button = (markup: string, label: string) => markup.match(new RegExp(`<button[^>]*>(?:(?!</button>).)*${label}(?:(?!</button>).)*</button>`))?.[0];
const NOTE = 'GPU encoding isn&#x27;t available for these settings here — the CPU exporter is used.'; // as serialised in HTML

describe('VideoSettingsDialog layout', () => {
  it('has one labelled group per row and the dialog title', () => {
    const markup = renderToStaticMarkup(<VideoSettingsDialog duration={754} initial={settings} onClose={() => {}} onRender={() => {}}/>);
    for (const label of ['Resolution', 'Frame rate', 'Codec', 'Bitrate', 'Battle speed']) expect(markup).toContain(`role="group" aria-label="${label}"`);
    expect(markup).toContain('Save your replay');
    expect(markup).toContain('Save as my default');
    expect(markup).toContain('Render &amp; save');
  });
  it('shows nothing from the removed parts', () => {
    const markup = renderToStaticMarkup(<VideoSettingsDialog duration={754} initial={settings} onClose={() => {}} onRender={() => {}}/>);
    for (const gone of ['<select', 'checkbox', 'Recent exports', 'Suggested for', 'Smoother map edges', 'Expected size', 'Compact', 'Balanced', '24 fps', 'Saves to']) expect(markup).not.toContain(gone);
  });
  it('labels the buttons as specified', () => {
    const markup = fields({support: support()});
    for (const text of ['1080p', '1920 × 1080', '1440p', '2560 × 1440', '4K', '3840 × 2160', '30 fps', '60 fps',
      'H.264 · GPU', 'H.264 · CPU', 'AV1 · GPU', 'AV1 · CPU', '5×', '10×', '15×', '20×', '25×']) expect(markup).toContain(text);
    for (const n of [4, 8, 12, 20, 24, 36, 48, 60, 84]) expect(markup).toContain(`${n} Mbps`);
    expect(markup).not.toContain('80 Mbps');
  });
  it('marks the current selection with aria-pressed', () => {
    const markup = fields({support: support()});
    expect(button(markup, 'AV1 · GPU')).toContain('aria-pressed="true"');
    expect(button(markup, 'AV1 · CPU')).toContain('aria-pressed="false"');
    expect(button(markup, '1080p')).toContain('aria-pressed="true"');
    expect(button(markup, '30 fps')).toContain('aria-pressed="true"');
    expect(button(markup, '8 Mbps')).toContain('aria-pressed="true"');
    expect(button(markup, '10×')).toContain('aria-pressed="true"');
    expect(button(markup, '60 fps')).toContain('aria-pressed="false"');
  });
});

describe('encoder support', () => {
  it('keeps every button enabled and shows no note until the first probe returns', () => {
    const markup = fields();
    expect(markup).not.toContain('disabled');
    expect(markup).not.toContain(NOTE);
    expect(markup).not.toContain('HEVC');
  });
  it('disables buttons from the mocked probe and shows the single info line', async () => {
    detect.mockResolvedValue(support({av1: {gpu: false, cpu: true}}));
    const result = await detectEncoderSupport({resolution: '1440', fps: 60, bitrate: 8_000_000});
    const markup = fields({support: result});
    expect(button(markup, 'AV1 · GPU')).toContain('disabled');
    expect(button(markup, 'AV1 · CPU')).not.toContain('disabled');
    expect(button(markup, 'H.264 · GPU')).not.toContain('disabled');
    expect(markup.split(NOTE)).toHaveLength(2);
  });
  it('shows no info line when every visible GPU button is enabled', () => {
    expect(fields({support: support()})).not.toContain(NOTE);
  });
  it('ignores a missing HEVC GPU encoder while HEVC is hidden, but not when it is shown', () => {
    expect(fields({support: support({hevc: none})})).not.toContain('HEVC');
    expect(fields({support: support({hevc: none})})).not.toContain(NOTE);
    expect(fields({support: support({hevc: {gpu: false, cpu: true}})})).toContain(NOTE);
  });
  it('shows the info line on a machine without any GPU encoder', () => {
    const cpuOnly = {gpu: false, cpu: true};
    const markup = fields({support: {h264: cpuOnly, av1: cpuOnly, hevc: none}});
    expect(markup).toContain(NOTE);
    expect(button(markup, 'AV1 · GPU')).toContain('disabled');
  });
  it('offers HEVC buttons only when supported', () => {
    expect(fields({support: support({hevc: all})})).toContain('HEVC · GPU');
    expect(fields({support: support({hevc: all})})).toContain('HEVC · CPU');
    expect(fields({support: support({hevc: none})})).not.toContain('HEVC');
  });
});

describe('selection fallback', () => {
  it('keeps an enabled selection', () => {
    expect(settleEncoder(settings, support())).toBe(settings);
  });
  it('moves to the same codec on the CPU when the GPU button is disabled', () => {
    expect(settleEncoder(settings, support({av1: {gpu: false, cpu: true}}))).toEqual({...settings, codec: 'av1', encoder: 'cpu'});
  });
  it('moves to the first enabled button when the codec has neither', () => {
    expect(settleEncoder(settings, support({av1: none}))).toEqual({...settings, codec: 'h264', encoder: 'gpu'});
    expect(settleEncoder(settings, support({av1: none, h264: {gpu: false, cpu: true}}))).toEqual({...settings, codec: 'h264', encoder: 'cpu'});
  });
  it('leaves the selection alone when nothing is available', () => {
    expect(settleEncoder(settings, {h264: none, av1: none, hevc: none})).toBe(settings);
  });
});

describe('size line', () => {
  it('shows video length and approximate size for a single save', () => {
    const markup = fields({support: support()});
    expect(markup).toMatch(/\d+:\d\d video<\/strong><span>· about \d+ MB/);
    expect(markup).not.toContain('min ·');
  });
  it('is hidden for a batch save', () => {
    expect(fields({batchCount: 3, support: support()})).not.toContain('video</strong>');
    const dialog = renderToStaticMarkup(<VideoSettingsDialog batchCount={3} duration={0} initial={settings} onClose={() => {}} onRender={() => {}}/>);
    expect(dialog).toContain('Save 3 replays');
    expect(dialog).not.toContain('about');
  });
});
