import { describe, expect, it, vi } from 'vitest';
import { setTextIfChanged, truncateToWidth, type MeasureText } from './text';

// Every character is 10 px wide.
const measure: MeasureText = value => Array.from(value).length * 10;
const style = { fontSize: 16 };

describe('truncateToWidth', () => {
  it('returns text that fits unchanged', () => {
    expect(truncateToWidth('Yamato', style, 60, measure)).toBe('Yamato');
  });
  it('ends with an ellipsis that fits the width', () => {
    const result = truncateToWidth('Montgomery', style, 70, measure);
    expect(result).toBe('Montgo…');
    expect(measure(result, style)).toBeLessThanOrEqual(70);
  });
  it('never puts the ellipsis after a space or a non-breaking space', () => {
    expect(truncateToWidth('Des Moines', style, 50, measure)).toBe('Des…');
    expect(truncateToWidth('Des\u00a0Moines', style, 50, measure)).toBe('Des…');
  });
  it('keeps non-breaking spaces inside text that fits', () => {
    expect(truncateToWidth('A\u00a0Name', style, 70, measure)).toBe('A\u00a0Name');
  });
  it('counts a non-breaking space as a normal character', () => {
    expect(truncateToWidth('A\u00a0Longname', style, 60, measure)).toBe('A\u00a0Lon…');
  });
  it('does not split surrogate pairs', () => {
    expect(truncateToWidth('ab😀😀😀', style, 40, measure)).toBe('ab😀…');
  });
  it('returns only the ellipsis when nothing else fits', () => {
    expect(truncateToWidth('Yamato', style, 10, measure)).toBe('…');
  });
});

describe('setTextIfChanged', () => {
  it('assigns only on change', () => {
    const set = vi.fn();
    const text = { _text: 'a', get text() { return this._text; }, set text(value: string) { set(value); this._text = value; } };
    setTextIfChanged(text, 'a');
    setTextIfChanged(text, 'b');
    setTextIfChanged(text, 'b');
    expect(set).toHaveBeenCalledTimes(1);
    expect(set).toHaveBeenCalledWith('b');
  });
});
