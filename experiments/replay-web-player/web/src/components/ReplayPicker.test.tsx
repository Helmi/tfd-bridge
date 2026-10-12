import { describe, expect, it } from 'vitest';
import { renderToStaticMarkup } from 'react-dom/server';
import { ReplayPicker, type LocalReplaySummary } from './ReplayPicker';
import { MAX_BATCH_SELECTION } from './pickerSelection';

const replay = (i: number, extra: Partial<LocalReplaySummary> = {}): LocalReplaySummary => ({
  id: `r${i}`, filename: `r${i}.wowsreplay`, shipName: `Ship ${i}`, shipClass: 'cruiser', mapName: 'Map',
  playedAt: '2026-01-01T10:00:00Z', modifiedAt: '2026-01-01T10:00:00Z', size: 1_048_576, ...extra,
});
const many = Array.from({ length: 25 }, (_, i) => replay(i, i === 1 ? { complete: false } : {}));
const markup = (props: Partial<Parameters<typeof ReplayPicker>[0]> = {}) => renderToStaticMarkup(
  <ReplayPicker replays={many} total={25} currentFilename="" onChoose={() => undefined} onClose={() => undefined} {...props} />);

describe('ReplayPicker selection mode', () => {
  it('offers the batch button only when the bridge enables it', () => {
    expect(markup()).not.toContain('Render multiple battles');
    expect(markup({ onRenderMultiple: () => undefined })).toContain('Render multiple battles');
  });

  it('shows checkboxes, the count and a disabled Next at zero selected', () => {
    const html = markup({ onRenderMultiple: () => undefined, initialSelection: [] });
    expect(html).toContain('role="checkbox"');
    expect(html).toContain('Cancel selection');
    expect(html).toContain('0 selected');
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Next<\/button>/);
    expect(html).toContain('Incomplete');
  });

  it('enables Next with a selection and keeps checked rows marked', () => {
    const html = markup({ onRenderMultiple: () => undefined, initialSelection: ['r3'] });
    expect(html).toContain('1 selected');
    expect(html).not.toMatch(/<button[^>]*disabled=""[^>]*>Next<\/button>/);
    expect(html).toContain('aria-checked="true"');
  });

  it('at the limit shows the hint and disables unselected rows', () => {
    const ids = many.slice(0, MAX_BATCH_SELECTION).map(r => r.id);
    const html = markup({ onRenderMultiple: () => undefined, initialSelection: ids });
    expect(html).toContain(`${MAX_BATCH_SELECTION} selected`);
    expect(html).toContain('Maximum of 20 battles per batch');
    expect(html.match(/aria-checked="false"[^>]*disabled=""|disabled=""[^>]*aria-checked="false"/g)).toHaveLength(5);
  });
});
