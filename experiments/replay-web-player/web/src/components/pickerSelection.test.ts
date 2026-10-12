import { describe, expect, it } from 'vitest';
import { MAX_BATCH_SELECTION, selectedReplays, selectionFull, toggleSelection } from './pickerSelection';

describe('picker selection rules', () => {
  it('toggles on and off', () => {
    const one = toggleSelection([], 'a');
    expect(one).toEqual(['a']);
    expect(toggleSelection(one, 'a')).toEqual([]);
  });

  it('refuses the 21st selection but still allows deselecting', () => {
    const full = Array.from({ length: MAX_BATCH_SELECTION }, (_, i) => `r${i}`);
    expect(selectionFull(full)).toBe(true);
    expect(toggleSelection(full, 'extra')).toBe(full);
    expect(toggleSelection(full, 'r0')).toHaveLength(MAX_BATCH_SELECTION - 1);
  });

  it('keeps the selection independent of filtering and paging', () => {
    const all = [{ id: 'a' }, { id: 'b' }, { id: 'c' }];
    expect(selectedReplays(all, ['c', 'a'])).toEqual([{ id: 'a' }, { id: 'c' }]);
    expect(selectedReplays(all.slice(0, 1), ['c', 'a'])).toEqual([{ id: 'a' }]);
  });
});
